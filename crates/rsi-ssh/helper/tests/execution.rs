#![cfg(target_os = "linux")]
#[path = "execution/observed_files.rs"]
mod observed_files;
use rsi_execution_local::NativeCapabilities;
use rsi_meta::{FiberHandle, ResolvedFactory, Runtime, UpdateMode};
use rsi_process::{
    DuplexProcessContract, ProcessContract, ProcessError, PtyProcessContract, PtySize,
};
use rsi_process_local::ProcessLocalFactory;
use rsi_sandbox::{SandboxContract, SandboxMode};
use rsi_sandbox_local::SandboxLocalFactory;
use rsi_ssh_client::{ProcessConnection, RemotePlan};
use rsi_ssh_helper::ExecutionServer;
use rsi_ssh_protocol::execution::{Preparation, Program, StartOptions};
use rsi_ssh_transport::{Connection, Role};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncWrite, DuplexStream, WriteHalf},
    task::JoinHandle,
};

#[derive(Debug, Default)]
struct RevocableGate(std::sync::atomic::AtomicBool);
impl rsi_execution::ExecutionAdmission for RevocableGate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<rsi_execution::ExecutionOperation> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        Ok(rsi_execution::ExecutionOperation::new(()))
    }
}

struct Fixture {
    _runtime: Runtime,
    fibers: Vec<FiberHandle>,
    root: tempfile::TempDir,
    client: ProcessConnection,
    connection: Connection,
    server: JoinHandle<rsi_process::Result<()>>,
    gate: Arc<std::sync::atomic::AtomicBool>,
    file_describes: Arc<std::sync::atomic::AtomicUsize>,
}
struct GatedWriter {
    writer: WriteHalf<DuplexStream>,
    blocked: Arc<std::sync::atomic::AtomicBool>,
}
impl AsyncWrite for GatedWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.blocked.load(std::sync::atomic::Ordering::SeqCst) {
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.writer).poll_write(cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.writer).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}
impl Fixture {
    fn file_descriptions(&self) -> usize {
        self.file_describes
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    async fn new(restricted: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::default();
        let sandbox = ResolvedFactory::linked(
            "sandbox",
            "fixture",
            UpdateMode::Replayable,
            Arc::new(SandboxLocalFactory::default()),
        );
        let process = ResolvedFactory::linked(
            "process",
            "fixture",
            UpdateMode::Replayable,
            Arc::new(ProcessLocalFactory),
        );
        let fibers = vec![runtime.root().apply(sandbox, json!({"bubblewrap": if restricted {vec!["/usr/bin/bwrap"]} else {vec![]}, "landlock": []})).await.unwrap(), runtime.root().apply(process, json!({})).await.unwrap()];
        let file_describes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let capabilities = NativeCapabilities {
            sandbox: runtime.root().lookup_local::<SandboxContract>().unwrap(),
            process: runtime.root().lookup_local::<ProcessContract>().unwrap(),
            duplex: runtime
                .root()
                .lookup_local::<DuplexProcessContract>()
                .unwrap(),
            pty: runtime.root().lookup_local::<PtyProcessContract>().unwrap(),
            files: Arc::new(observed_files::Observed {
                inner: rsi_files::LocalFiles::new().unwrap(),
                describes: file_describes.clone(),
            }),
        };
        let environment = vec![("FIXTURE".into(), "target-owned".into())];
        let programs = BTreeMap::from([
            (
                "shell".into(),
                Program {
                    program: "/bin/sh".into(),
                    environment: environment.clone(),
                },
            ),
            (
                "cat".into(),
                Program {
                    program: "/bin/cat".into(),
                    environment,
                },
            ),
        ]);
        let server = ExecutionServer::new(capabilities, programs).await.unwrap();
        let (client_io, helper_io) = tokio::io::duplex(4096);
        let (cr, cw) = tokio::io::split(client_io);
        let (hr, hw) = tokio::io::split(helper_io);
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (connection, _) = Connection::start(cr, cw, Role::Client, 41).unwrap();
        let (helper, incoming) = Connection::start(
            hr,
            GatedWriter {
                writer: hw,
                blocked: gate.clone(),
            },
            Role::Helper,
            41,
        )
        .unwrap();
        let server = tokio::spawn(server.serve(helper, incoming));
        let client = ProcessConnection::new(connection.clone()).unwrap();
        Self {
            _runtime: runtime,
            fibers,
            root,
            client,
            connection,
            server,
            gate,
            file_describes,
        }
    }
    async fn preparation(&self, selector: &str, script: Option<&str>, pty: bool) -> Preparation {
        let program = self.client.resolve(selector).await.unwrap();
        let path = self
            .client
            .canonicalize(self.root.path().to_str().unwrap())
            .await
            .unwrap();
        Preparation {
            source_reader: false,
            mode: if pty {
                SandboxMode::WorkspaceWrite
            } else {
                SandboxMode::DangerFullAccess
            },
            pty,
            program: program.program,
            environment: program.environment,
            cwd: path.clone(),
            workspace: path,
            arguments: script.map_or_else(Vec::new, |script| vec!["-c".into(), script.into()]),
        }
    }
    async fn plan(&self, selector: &str, script: Option<&str>, pty: bool) -> RemotePlan {
        self.client
            .prepare(self.preparation(selector, script, pty).await)
            .await
            .unwrap()
    }
    fn binding(&self, caller: rsi_files_protocol::FilesCaller) -> rsi_files_protocol::FilesBinding {
        rsi_files_protocol::FilesBinding::new(
            caller,
            "session",
            "revision",
            self.root.path().to_path_buf(),
        )
        .unwrap()
    }
    async fn close(self) {
        self.connection.close();
        tokio::time::timeout(Duration::from_secs(5), self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for fiber in self.fibers.into_iter().rev() {
            assert!(fiber.dispose().await.is_clean());
        }
    }
}
fn batch(bytes: usize, capture: usize) -> StartOptions {
    StartOptions::Batch {
        stdin_bytes: bytes,
        stdout_max_bytes: capture,
        stderr_max_bytes: capture,
        termination_grace_ms: 50,
    }
}
fn duplex() -> StartOptions {
    StartOptions::Duplex {
        stdout_buffer_bytes: 4096,
        stderr_max_bytes: 4096,
        termination_grace_ms: 50,
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("bounded fixture operation")
}

#[tokio::test]
async fn native_batch_upload_preserves_four_mib_and_reports_actual_tail_loss() {
    let fixture = Fixture::new(false).await;
    let stdin = vec![b'x'; rsi_process::MAXIMUM_PROCESS_STDIN_BYTES];
    let plan = fixture.plan("cat", None, false).await;
    let process = bounded(fixture.client.spawn(plan, batch(stdin.len(), 4096), stdin))
        .await
        .unwrap();
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    let read = process.stdout().read_from(0).unwrap();
    assert_eq!(read.bytes, vec![b'x'; 4096]);
    assert_eq!(
        read.oldest_offset,
        (rsi_process::MAXIMUM_PROCESS_STDIN_BYTES - 4096) as u64
    );
    assert_eq!(
        read.next_offset,
        rsi_process::MAXIMUM_PROCESS_STDIN_BYTES as u64
    );
    assert!(read.lossy);
    assert!(read.full_output.is_none());
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn target_policy_is_exact_and_children_never_inherit_helper_or_service_environment() {
    let fixture = Fixture::new(false).await;
    let mut request = fixture.preparation("shell", Some("printf '%s:%s:%s:%s:%s' \"$FIXTURE\" \"${HOME-unset}\" \"${SSH_AUTH_SOCK-unset}\" \"${NOTIFY_SOCKET-unset}\" \"${DBUS_SESSION_BUS_ADDRESS-unset}\""), false).await;
    request
        .environment
        .push(("INJECTED".into(), "not-in-target-policy".into()));
    assert!(matches!(
        fixture.client.prepare(request.clone()).await,
        Err(ProcessError::InvalidInput(_))
    ));
    request.environment.pop();
    let plan = fixture.client.prepare(request).await.unwrap();
    let process = bounded(fixture.client.spawn(plan, batch(0, 4096), vec![]))
        .await
        .unwrap();
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    assert_eq!(
        process.stdout().read_from(0).unwrap().bytes,
        b"target-owned:unset:unset:unset:unset"
    );
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn native_duplex_acknowledges_input_and_preserves_lossless_output_and_clean_eof() {
    let fixture = Fixture::new(false).await;
    let plan = fixture.plan("cat", None, false).await;
    let process = bounded(fixture.client.spawn_duplex(plan, duplex()))
        .await
        .unwrap();
    let bytes = vec![b'd'; 64 * 1024];
    let input = process.stdin();
    let output = process.stdout();
    let written = tokio::spawn(async move {
        let mut offset = 0;
        while offset < bytes.len() {
            offset += input.write(&bytes[offset..]).await.unwrap();
        }
        input.close().await.unwrap();
        offset
    });
    let read = bounded(async {
        let mut bytes = vec![];
        loop {
            let read = output.read(701).await.unwrap();
            bytes.extend(read.bytes);
            if read.eof {
                break;
            }
        }
        bytes
    })
    .await;
    assert_eq!(read, vec![b'd'; 64 * 1024]);
    assert_eq!(written.await.unwrap(), 64 * 1024);
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn same_epoch_different_connection_cannot_consume_a_prepared_plan() {
    let source = Fixture::new(false).await;
    let other = Fixture::new(false).await;
    let plan = source.plan("shell", Some("touch forbidden"), false).await;
    assert!(matches!(
        other.client.spawn(plan, batch(0, 1024), vec![]).await,
        Err(ProcessError::InvalidInput(_))
    ));
    assert!(!source.root.path().join("forbidden").exists());
    assert!(!other.root.path().join("forbidden").exists());
    source.close().await;
    other.close().await;
}

#[tokio::test]
async fn lost_start_reply_and_abandoned_waiter_still_reap_the_actual_native_child() {
    let fixture = Fixture::new(false).await;
    let plan = fixture
        .plan(
            "shell",
            Some("printf '%s' \"$$\" > child; exec /bin/sleep 300"),
            false,
        )
        .await;
    fixture
        .gate
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let client = fixture.client.clone();
    let start = tokio::spawn(async move { client.spawn_duplex(plan, duplex()).await });
    let pid = bounded(async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(fixture.root.path().join("child")).await
                && let Ok(pid) = text.parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    start.abort();
    fixture.close().await;
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
#[ignore = "requires native Linux Bubblewrap, openat2 and user namespaces"]
async fn native_pty_output_resize_ack_and_input_survive_the_mux() {
    let fixture = Fixture::new(true).await;
    let plan = fixture
        .plan(
            "shell",
            Some("printf 'READY\\n'; read value; /bin/stty size; printf 'RESULT_%s\\n' \"$value\""),
            true,
        )
        .await;
    let process = bounded(fixture.client.spawn_pty(
        plan,
        StartOptions::Pty {
            columns: 80,
            rows: 24,
            termination_grace_ms: 50,
        },
    ))
    .await
    .unwrap();
    let mut output = vec![];
    bounded(async {
        while !output.windows(5).any(|bytes| bytes == b"READY") {
            output.extend(process.read().await.unwrap().bytes);
        }
        process
            .resize(PtySize {
                columns: 100,
                rows: 30,
            })
            .await
            .unwrap();
        assert_eq!(process.write(b"OK\n").await.unwrap(), 3);
        loop {
            let read = process.read().await.unwrap();
            output.extend(read.bytes);
            if read.eof {
                break;
            }
        }
        assert_eq!(process.wait().await.unwrap().exit_code, Some(0));
    })
    .await;
    assert!(
        output.windows(6).any(|bytes| bytes == b"30 100"),
        "{output:?}"
    );
    assert!(output.windows(9).any(|bytes| bytes == b"RESULT_OK"));
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn abandoned_preparations_release_capacity_and_reused_slots_run_without_stale_credit() {
    let fixture = Fixture::new(false).await;
    let request = fixture
        .preparation("shell", Some("printf reused"), false)
        .await;
    let mut plans = Vec::new();
    for _ in 0..20 {
        plans.push(fixture.client.prepare(request.clone()).await.unwrap());
    }
    assert!(matches!(
        fixture.client.prepare(request.clone()).await,
        Err(ProcessError::Capacity)
    ));
    drop(plans);
    let plan = bounded(async {
        loop {
            match fixture.client.prepare(request.clone()).await {
                Ok(plan) => break plan,
                Err(ProcessError::Capacity) => tokio::time::sleep(Duration::from_millis(2)).await,
                Err(error) => panic!("unexpected preparation failure: {error:?}"),
            }
        }
    })
    .await;
    let process = bounded(fixture.client.spawn(plan, batch(0, 1024), vec![]))
        .await
        .unwrap();
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    assert_eq!(process.stdout().read_from(0).unwrap().bytes, b"reused");
    let peek = process.stdout().peek_tail(3).unwrap();
    assert_eq!(peek.bytes, b"sed");
    assert_eq!(peek.oldest_offset, 3);
    assert!(peek.lossy);
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn saturated_lossless_output_does_not_block_termination_ack_or_native_reaping() {
    let fixture = Fixture::new(false).await;
    let plan = fixture
        .plan("shell", Some("exec /usr/bin/yes saturated"), false)
        .await;
    let process = bounded(fixture.client.spawn_duplex(plan, duplex()))
        .await
        .unwrap();
    let pid = process.pid();
    // Native buffer and four transport credits fill without an output consumer.
    tokio::time::sleep(Duration::from_millis(50)).await;
    bounded(fixture.connection.heartbeat()).await.unwrap();
    process.terminate();
    bounded(process.wait_settlement()).await.unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn a_prepared_reply_abandoned_in_its_publication_queue_releases_the_target_plan() {
    let fixture = Fixture::new(false).await;
    let request = fixture.preparation("cat", None, false).await;
    let mut held = Vec::new();
    for _ in 0..19 {
        held.push(fixture.client.prepare(request.clone()).await.unwrap());
    }
    let mut unpublished = Box::pin(fixture.client.prepare(request.clone()));
    std::future::poll_fn(|cx| {
        assert!(unpublished.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // A capacity rejection proves the twentieth preparation was admitted. Its
    // caller has never polled again to consume the retained publication result.
    bounded(async {
        loop {
            match fixture.client.prepare(request.clone()).await {
                Err(ProcessError::Capacity) => break,
                Ok(extra) => drop(extra),
                Err(error) => panic!("unexpected preparation failure: {error:?}"),
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(unpublished);
    let replacement = bounded(async {
        loop {
            match fixture.client.prepare(request.clone()).await {
                Ok(plan) => break plan,
                Err(ProcessError::Capacity) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected replacement failure: {error:?}"),
            }
        }
    })
    .await;
    drop(replacement);
    drop(held);
    fixture.close().await;
}

#[tokio::test]
async fn target_files_preserve_exact_bytes_names_versions_and_opaque_caller_bindings() {
    use rsi_files_protocol::{FileKind, Files, FilesCaller, FilesError, RelativePath};
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::new(false).await;
    let exact_name = std::ffi::OsString::from_vec(vec![b'f', 0xff]);
    std::fs::write(fixture.root.path().join(&exact_name), [0, 255, 13, 10]).unwrap();
    let files = rsi_ssh_client::RemoteFiles::new(fixture.client.clone());
    let caller = FilesCaller::default();
    let binding = fixture.binding(caller.clone());
    let opened = bounded(files.open(
        binding.clone(),
        RelativePath::new(&[b'f', 0xff]).unwrap(),
        FileKind::File,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    let page = bounded(files.read(
        binding.clone(),
        opened.token.clone(),
        0,
        64,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    assert_eq!(page.bytes_hex, "00ff0d0a");
    let foreign = fixture.binding(FilesCaller::default());
    assert_eq!(
        files
            .read(
                foreign.clone(),
                opened.token.clone(),
                0,
                64,
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Binding)
    );
    assert_eq!(
        files.release(&foreign, &opened.token),
        Err(FilesError::Binding)
    );
    let directory = files
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let descriptions = fixture.file_descriptions();
    let listing = files
        .list(
            binding.clone(),
            directory.token.clone(),
            0,
            32,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].path.as_bytes(), &[b'f', 0xff]);
    std::fs::set_permissions(
        fixture.root.path().join(&exact_name),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_eq!(
        files
            .read(
                binding.clone(),
                opened.token.clone(),
                0,
                64,
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Changed)
    );
    assert_eq!(
        fixture.file_descriptions(),
        descriptions,
        "read/list must validate their selected native object without sweeping other handles"
    );
    files.release_caller(&caller);
    assert_eq!(
        files.describe(&binding, &opened.token),
        Err(FilesError::Unavailable)
    );
    assert_eq!(
        files.describe(&binding, &directory.token),
        Err(FilesError::Unavailable)
    );
    drop(files);
    fixture.close().await;
}

#[tokio::test]
async fn target_files_keep_the_opened_root_when_its_path_is_replaced_and_reject_symlinks() {
    use rsi_files_protocol::{FileKind, Files, FilesBinding, FilesCaller, RelativePath};
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::new(false).await;
    let root = fixture.root.path().join("original");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file"), b"old").unwrap();
    let files = rsi_ssh_client::RemoteFiles::new(fixture.client.clone());
    let binding =
        FilesBinding::new(FilesCaller::default(), "session", "revision", root.clone()).unwrap();
    let opened = files
        .open(
            binding.clone(),
            RelativePath::new(b"file").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    std::fs::rename(&root, fixture.root.path().join("retained")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file"), b"new").unwrap();
    let page = files
        .read(
            binding.clone(),
            opened.token.clone(),
            0,
            64,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(page.bytes_hex, "6f6c64");
    std::os::unix::fs::symlink(
        fixture.root.path().join("retained/file"),
        root.join("escape"),
    )
    .unwrap();
    assert!(
        files
            .open(
                binding.clone(),
                RelativePath::new(b"escape").unwrap(),
                FileKind::File,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    files.release(&binding, &opened.token).unwrap();
    drop(files);
    fixture.close().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end lease/revocation/cleanup scenario.
async fn ssh_execution_lease_pins_all_capabilities_and_rechecks_revocation_without_losing_cleanup()
{
    use rsi_execution::ExecutionTargetId;
    use rsi_files_protocol::{FileKind, FilesCaller, RelativePath};
    use rsi_process::DuplexProcessSpec;
    use rsi_sandbox::{ProcessRequest, ProcessStdio, WorkspaceReadRequest};
    use std::sync::atomic::Ordering;
    use tokio_util::sync::CancellationToken;
    let fixture = Fixture::new(false).await;
    std::fs::write(fixture.root.path().join("proof"), b"target").unwrap();
    let target = ExecutionTargetId::parse("e".repeat(32)).unwrap();
    let provider = rsi_ssh_client::execution_provider(
        rsi_execution::HostEpoch::from_bytes([17; 16]),
        fixture.client.clone(),
        target,
        7,
    )
    .unwrap();
    let gate = Arc::new(RevocableGate::default());
    let lease = provider.lease(gate.clone()).unwrap();
    assert_eq!(lease.binding().target_revision(), 7);
    assert_eq!(lease.binding().connection_epoch(), 41);
    let coordinates = lease
        .canonicalize(fixture.root.path().to_str().unwrap())
        .await
        .unwrap();
    let program = lease.resolve_program("cat").await.unwrap();
    let scope = lease
        .workspace_read(WorkspaceReadRequest {
            mode: SandboxMode::ReadOnly,
            cwd: coordinates.path().into(),
            workspace: coordinates.path().into(),
        })
        .await
        .unwrap();
    assert_eq!(scope.workspace(), fixture.root.path());
    let plan = lease
        .prepare(ProcessRequest {
            stdio: ProcessStdio::Pipes,
            mode: SandboxMode::DangerFullAccess,
            program,
            arguments: vec![],
            cwd: coordinates.path().into(),
            workspace: coordinates.path().into(),
        })
        .await
        .unwrap();
    let environment = plan.environment().to_vec();
    let process = lease
        .spawn_duplex(DuplexProcessSpec {
            process: plan,
            environment,
            stdout_buffer_bytes: 4096,
            stderr_max_bytes: 4096,
            termination_grace_ms: 50,
        })
        .await
        .unwrap();
    let files = lease.files().unwrap();
    let binding = fixture.binding(FilesCaller::default());
    let opened = files
        .open(
            binding.clone(),
            RelativePath::new(b"proof").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        files
            .read(
                binding.clone(),
                opened.token.clone(),
                0,
                32,
                CancellationToken::new()
            )
            .await
            .unwrap()
            .bytes_hex,
        "746172676574"
    );
    assert_eq!(process.stdin().write(b"before\n").await.unwrap(), 7);
    assert_eq!(
        bounded(process.stdout().read(64)).await.unwrap().bytes,
        b"before\n"
    );
    gate.0.store(true, Ordering::SeqCst);
    assert_eq!(
        process.stdin().write(b"forbidden\n").await,
        Err(ProcessError::ShuttingDown)
    );
    assert!(
        files
            .read(
                binding.clone(),
                opened.token.clone(),
                0,
                32,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    files.release(&binding, &opened.token).unwrap();
    let pid = process.pid();
    process.terminate();
    bounded(process.wait_settlement()).await.unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    drop(process);
    drop(files);
    fixture.close().await;
}

#[tokio::test]
async fn separate_execution_leases_cannot_read_or_release_each_others_file_tokens() {
    use rsi_execution::{ExecutionAdmission, ExecutionOperation, ExecutionTargetId};
    use rsi_files_protocol::{FileKind, FilesBinding, FilesCaller, FilesError, RelativePath};
    use tokio_util::sync::CancellationToken;
    #[derive(Debug)]
    struct Gate;
    impl ExecutionAdmission for Gate {
        fn admit(
            &self,
            _kind: rsi_execution::ExecutionAdmissionKind,
        ) -> rsi_process::Result<ExecutionOperation> {
            Ok(ExecutionOperation::new(()))
        }
    }
    let fixture = Fixture::new(false).await;
    std::fs::write(fixture.root.path().join("proof"), b"scoped").unwrap();
    let provider = rsi_ssh_client::execution_provider(
        rsi_execution::HostEpoch::from_bytes([17; 16]),
        fixture.client.clone(),
        ExecutionTargetId::parse("c".repeat(32)).unwrap(),
        1,
    )
    .unwrap();
    let first = provider.lease(Arc::new(Gate)).unwrap().files().unwrap();
    let second = provider.lease(Arc::new(Gate)).unwrap().files().unwrap();
    let caller = FilesCaller::default();
    let binding = FilesBinding::new(
        caller.clone(),
        "same-session",
        "same-revision",
        fixture.root.path().to_path_buf(),
    )
    .unwrap();
    let a = first
        .open(
            binding.clone(),
            RelativePath::new(b"proof").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let b = second
        .open(
            binding.clone(),
            RelativePath::new(b"proof").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        second
            .read(
                binding.clone(),
                a.token.clone(),
                0,
                64,
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Binding)
    );
    assert_eq!(second.release(&binding, &a.token), Err(FilesError::Binding));
    first.release_caller(&caller);
    assert_eq!(
        second
            .read(
                binding.clone(),
                b.token.clone(),
                0,
                64,
                CancellationToken::new()
            )
            .await
            .unwrap()
            .bytes_hex,
        "73636f706564"
    );
    second.release(&binding, &b.token).unwrap();
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one target catalog admission and exhaustion scenario"
)]
async fn explicit_target_programs_are_frozen_bounded_and_cannot_override_account_or_lifecycle_environment()
 {
    use rsi_ssh_protocol::initialization::ProgramPolicy;
    let fixture = Fixture::new(false).await;
    for key in [
        "HOME",
        "PATH",
        "SSH_AUTH_SOCK",
        "NOTIFY_SOCKET",
        "WATCHDOG_USEC",
        "DBUS_SESSION_BUS_ADDRESS",
        "LD_PRELOAD",
    ] {
        assert!(matches!(
            fixture
                .client
                .resolve_configured(ProgramPolicy {
                    command: "sh".into(),
                    environment: vec![(key.into(), "forbidden".into())],
                })
                .await,
            Err(ProcessError::InvalidInput(_))
        ));
    }
    let policy = ProgramPolicy {
        command: "sh".into(),
        environment: vec![("LABEL".into(), "explicit-target".into())],
    };
    let program = fixture
        .client
        .resolve_configured(policy.clone())
        .await
        .unwrap();
    assert!(program.program.starts_with('/'));
    for key in ["HOME", "PATH", "USER", "LOGNAME"] {
        assert_eq!(
            program
                .environment
                .iter()
                .filter(|(name, _)| name == key)
                .count(),
            1
        );
    }
    let home = program
        .environment
        .iter()
        .find(|(name, _)| name == "HOME")
        .unwrap()
        .1
        .clone();
    let preparation = Preparation {
        source_reader: false,
        mode: SandboxMode::DangerFullAccess,
        pty: false,
        program: program.program.clone(),
        environment: program.environment.clone(),
        arguments: vec![
            "-c".into(),
            "printf '%s:%s:%s' \"$LABEL\" \"$HOME\" \"${SSH_AUTH_SOCK-unset}\"".into(),
        ],
        cwd: fixture.root.path().to_str().unwrap().into(),
        workspace: fixture.root.path().to_str().unwrap().into(),
    };
    let plan = fixture.client.prepare(preparation.clone()).await.unwrap();
    let process = fixture
        .client
        .spawn(plan, batch(0, 4096), vec![])
        .await
        .unwrap();
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    assert_eq!(
        process.stdout().read_from(0).unwrap().bytes,
        format!("explicit-target:{home}:unset").as_bytes()
    );
    let mut forged = preparation;
    forged
        .environment
        .push(("INJECTED".into(), "unresolved".into()));
    assert!(matches!(
        fixture.client.prepare(forged).await,
        Err(ProcessError::InvalidInput(_))
    ));
    for index in 1..128 {
        fixture
            .client
            .resolve_configured(ProgramPolicy {
                command: "sh".into(),
                environment: vec![("LABEL".into(), index.to_string())],
            })
            .await
            .unwrap();
    }
    assert_eq!(
        fixture.client.resolve_configured(policy).await.unwrap(),
        program
    );
    assert!(matches!(
        fixture
            .client
            .resolve_configured(ProgramPolicy {
                command: "sh".into(),
                environment: vec![("LABEL".into(), "overflow".into())],
            })
            .await,
        Err(ProcessError::Capacity)
    ));
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn twenty_long_polled_processes_leave_ordinary_capacity_and_drop_settles_all_children() {
    let fixture = Fixture::new(false).await;
    let mut processes = Vec::new();
    for _ in 0..20 {
        let plan = fixture.plan("cat", None, false).await;
        processes.push(
            bounded(fixture.client.spawn_duplex(plan, duplex()))
                .await
                .unwrap(),
        );
    }
    let pids: Vec<_> = processes
        .iter()
        .map(rsi_process::ManagedDuplexProcess::pid)
        .collect();
    bounded(fixture.client.resolve("cat")).await.unwrap();
    bounded(fixture.connection.heartbeat()).await.unwrap();
    drop(processes);
    bounded(async {
        while pids
            .iter()
            .any(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists())
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    fixture.close().await;
}

#[tokio::test]
async fn stalled_native_stdin_reports_capacity_and_keeps_connection_and_siblings_alive() {
    let fixture = Fixture::new(false).await;
    let plan = fixture
        .plan(
            "shell",
            Some("while [ ! -f drain ]; do /bin/sleep 0.02; done; exec /bin/cat"),
            false,
        )
        .await;
    let process = fixture.client.spawn_duplex(plan, duplex()).await.unwrap();
    let bytes = vec![b'q'; 64 * 1024];
    let input = process.stdin();
    let mut accepted = 0;
    let mut pressured = false;
    for _ in 0..64 {
        match tokio::time::timeout(Duration::from_secs(2), input.write(&bytes))
            .await
            .expect("native flow control must settle before the RPC deadline")
        {
            Ok(count) => {
                assert!(count > 0 && count <= bytes.len());
                accepted += count;
            }
            Err(ProcessError::Capacity) => {
                pressured = true;
                break;
            }
            other => panic!("unexpected input result: {other:?}"),
        }
    }
    assert!(pressured, "the real native pipe must fill");
    let sibling_plan = fixture
        .plan("shell", Some("printf sibling; touch drain"), false)
        .await;
    let sibling = fixture
        .client
        .spawn(sibling_plan, batch(0, 1024), vec![])
        .await
        .unwrap();
    assert_eq!(bounded(sibling.wait()).await.unwrap().exit_code, Some(0));
    assert_eq!(sibling.stdout().read_from(0).unwrap().bytes, b"sibling");
    // Resume the same pipe after pressure and prove a fresh frame follows exactly
    // the previously acknowledged prefixes, with no hidden bytes from the timeout.
    let marker = b"after-capacity\0frame";
    let output = process.stdout();
    let reading = tokio::spawn(async move {
        let mut received = Vec::new();
        loop {
            let chunk = output.read(4096).await.unwrap();
            received.extend(chunk.bytes);
            if chunk.eof {
                break;
            }
        }
        received
    });
    let mut offset = 0;
    while offset < marker.len() {
        offset += bounded(input.write(&marker[offset..])).await.unwrap();
    }
    input.close().await.unwrap();
    let received = bounded(reading).await.unwrap();
    let mut expected = vec![b'q'; accepted];
    expected.extend_from_slice(marker);
    assert_eq!(received, expected);
    assert_eq!(bounded(process.wait()).await.unwrap().exit_code, Some(0));
    drop((process, sibling));
    fixture.close().await;
}

#[tokio::test]
async fn missing_sandbox_rejects_restricted_target_preparation_without_running_command() {
    let fixture = Fixture::new(false).await;
    let mut request = fixture
        .preparation("shell", Some("touch must-not-run"), false)
        .await;
    request.mode = SandboxMode::ReadOnly;
    assert!(fixture.client.prepare(request.clone()).await.is_err());
    request.mode = SandboxMode::WorkspaceWrite;
    assert!(fixture.client.prepare(request).await.is_err());
    assert!(!fixture.root.path().join("must-not-run").exists());
    // Unsupported confinement does not poison the connection or imply full access.
    assert!(fixture.client.resolve("shell").await.is_ok());
    fixture.close().await;
}
