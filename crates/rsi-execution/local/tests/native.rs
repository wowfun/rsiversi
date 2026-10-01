#![cfg(target_os = "linux")]
use rsi_execution::{ExecutionAdmission, ExecutionLease, ExecutionOperation, ResolvedProgram};
use rsi_execution_local::NativeCapabilities;
use rsi_files_protocol::{FileKind, FilesBinding, FilesCaller, RelativePath};
use rsi_meta::{FiberHandle, ResolvedFactory, Runtime, UpdateMode};
use rsi_process::{
    DuplexProcessContract, DuplexProcessSpec, ProcessContract, ProcessError, ProcessSpec,
    PtyProcessContract, PtyProcessSpec, PtySize,
};
use rsi_process_local::ProcessLocalFactory;
use rsi_sandbox::{ProcessRequest, ProcessStdio, SandboxContract, SandboxMode};
use rsi_sandbox_local::SandboxLocalFactory;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct Gate {
    revoked: AtomicBool,
    active: Arc<AtomicUsize>,
}
#[derive(Debug)]
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.revoked.load(Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionOperation::new(Permit(self.active.clone())))
    }
}
struct Fixture {
    _runtime: Runtime,
    fibers: Vec<FiberHandle>,
    root: tempfile::TempDir,
    gate: Arc<Gate>,
    lease: ExecutionLease,
}
impl Fixture {
    async fn new(restricted: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::default();
        let sandbox = ResolvedFactory::linked(
            "sandbox",
            "test",
            UpdateMode::Replayable,
            Arc::new(SandboxLocalFactory::default()),
        );
        let process = ResolvedFactory::linked(
            "process",
            "test",
            UpdateMode::Replayable,
            Arc::new(ProcessLocalFactory),
        );
        let fibers = vec![runtime.root().apply(sandbox, json!({"bubblewrap": if restricted { vec!["/usr/bin/bwrap"] } else { vec![] }, "landlock": []})).await.unwrap(),
            runtime.root().apply(process, json!({})).await.unwrap()];
        let capabilities = NativeCapabilities {
            sandbox: runtime.root().lookup_local::<SandboxContract>().unwrap(),
            process: runtime.root().lookup_local::<ProcessContract>().unwrap(),
            duplex: runtime
                .root()
                .lookup_local::<DuplexProcessContract>()
                .unwrap(),
            pty: runtime.root().lookup_local::<PtyProcessContract>().unwrap(),
            files: Arc::new(rsi_files::LocalFiles::new().unwrap()),
        };
        let programs = BTreeMap::from([
            (
                "shell".into(),
                ResolvedProgram {
                    program: "/bin/sh".into(),
                    environment: vec![("FIXTURE".into(), "owned-local".into())],
                },
            ),
            (
                "shell-alternative".into(),
                ResolvedProgram {
                    program: "/bin/sh".into(),
                    environment: vec![("FIXTURE".into(), "different-policy".into())],
                },
            ),
            (
                "cat".into(),
                ResolvedProgram {
                    program: "/bin/cat".into(),
                    environment: vec![],
                },
            ),
        ]);
        let provider = rsi_execution_local::provider(
            rsi_execution::HostEpoch::from_bytes([17; 16]),
            capabilities,
            programs,
        )
        .await
        .unwrap();
        let gate = Arc::new(Gate::default());
        let lease = provider.lease(gate.clone()).unwrap();
        Self {
            _runtime: runtime,
            fibers,
            root,
            gate,
            lease,
        }
    }
    async fn plan(
        &self,
        selector: &str,
        script: Option<&str>,
        stdio: ProcessStdio,
    ) -> rsi_execution::PreparedProcess {
        let program = self.lease.resolve_program(selector).await.unwrap();
        let coordinates = self
            .lease
            .canonicalize(self.root.path().to_str().unwrap())
            .await
            .unwrap();
        self.lease
            .prepare(ProcessRequest {
                stdio,
                mode: if stdio == ProcessStdio::Pty {
                    SandboxMode::WorkspaceWrite
                } else {
                    SandboxMode::DangerFullAccess
                },
                program,
                arguments: script.map_or_else(Vec::new, |script| vec!["-c".into(), script.into()]),
                cwd: coordinates.path().into(),
                workspace: coordinates.path().into(),
            })
            .await
            .unwrap()
    }
    async fn close(self) {
        for fiber in self.fibers.into_iter().rev() {
            assert!(fiber.dispose().await.is_clean());
        }
    }
}

#[tokio::test]
#[ignore = "requires native Linux Bubblewrap"]
async fn source_reader_lease_keeps_tmp_ancestors_visible_and_read_only() {
    let fixture = Fixture::new(true).await;
    let workspace = fixture.root.path().join("child");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(fixture.root.path().join("ancestor"), b"ancestor-proof").unwrap();
    let program = fixture.lease.resolve_program("shell").await.unwrap();
    let plan = fixture
        .lease
        .prepare_source_reader(ProcessRequest {
            stdio: ProcessStdio::Pipes,
            mode: SandboxMode::ReadOnly,
            program,
            arguments: vec![
                "-c".into(),
                "cat ../ancestor; if printf changed > ../ancestor; then exit 42; fi".into(),
            ],
            cwd: workspace.clone(),
            workspace,
        })
        .await
        .unwrap();
    let environment = plan.environment().to_vec();
    let child = fixture
        .lease
        .spawn(ProcessSpec {
            process: plan,
            stdin: vec![],
            environment,
            stdout_max_bytes: 1024,
            stderr_max_bytes: 1024,
            termination_grace_ms: 100,
        })
        .await
        .unwrap();
    assert_eq!(child.wait().await.unwrap().exit_code, Some(0));
    assert_eq!(
        child.stdout().read_from(0).unwrap().bytes,
        b"ancestor-proof"
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("ancestor")).unwrap(),
        b"ancestor-proof"
    );
    drop(child);
    fixture.close().await;
}

#[tokio::test]
async fn native_batch_and_files_use_the_fixed_directory_environment_without_retaining_start_admission()
 {
    let fixture = Fixture::new(false).await;
    let plan = fixture
        .plan(
            "shell",
            Some("printf '%s:%s' \"$FIXTURE\" \"${HOME-unset}\"; printf file-proof > proof"),
            ProcessStdio::Pipes,
        )
        .await;
    let environment = plan.environment().to_vec();
    let process = fixture
        .lease
        .spawn(ProcessSpec {
            process: plan,
            environment,
            stdin: vec![],
            stdout_max_bytes: 1024,
            stderr_max_bytes: 1024,
            termination_grace_ms: 50,
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap()
            .exit_code,
        Some(0)
    );
    // Retained output handles must not retain an already-settled operation grant.
    assert_eq!(fixture.gate.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        process.stdout().read_from(0).unwrap().bytes,
        b"owned-local:unset"
    );
    let files = fixture.lease.files().unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "fixture",
        "header",
        fixture.root.path().canonicalize().unwrap(),
    )
    .unwrap();
    let opened = files
        .open(
            binding.clone(),
            RelativePath::new(b"proof").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let page = files
        .read(
            binding.clone(),
            opened.token.clone(),
            0,
            32,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(page.bytes_hex, "66696c652d70726f6f66");
    fixture.gate.revoked.store(true, Ordering::SeqCst);
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
    drop(process);
    drop(files);
    fixture.close().await;
}

#[tokio::test]
async fn native_duplex_retains_the_plan_and_denies_new_input_after_revocation() {
    let fixture = Fixture::new(false).await;
    let plan = fixture.plan("cat", None, ProcessStdio::Pipes).await;
    let environment = plan.environment().to_vec();
    let process = fixture
        .lease
        .spawn_duplex(DuplexProcessSpec {
            process: plan,
            environment,
            stdout_buffer_bytes: 1024,
            stderr_max_bytes: 1024,
            termination_grace_ms: 50,
        })
        .await
        .unwrap();
    let pid = process.pid();
    assert_eq!(fixture.gate.active.load(Ordering::SeqCst), 0);
    assert_eq!(process.stdin().write(b"exact-bytes\n").await.unwrap(), 12);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), process.stdout().read(64))
            .await
            .unwrap()
            .unwrap()
            .bytes,
        b"exact-bytes\n"
    );
    fixture.gate.revoked.store(true, Ordering::SeqCst);
    assert_eq!(fixture.gate.active.load(Ordering::SeqCst), 0);
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(
        process.stdin().write(b"forbidden").await,
        Err(ProcessError::ShuttingDown)
    );
    process.terminate();
    tokio::time::timeout(Duration::from_secs(5), process.wait_settlement())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.gate.active.load(Ordering::SeqCst), 0);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    drop(process);
    fixture.close().await;
}

#[tokio::test]
#[ignore = "requires native Linux Bubblewrap and user namespaces; run explicitly"]
async fn native_pty_executes_and_resizes_through_the_same_lease() {
    let fixture = Fixture::new(true).await;
    let plan = fixture
        .plan(
            "shell",
            Some("printf 'PTY_READY\\n'; read value; printf 'PTY_%s\\n' \"$value\""),
            ProcessStdio::Pty,
        )
        .await;
    let environment = plan.environment().to_vec();
    let process = fixture
        .lease
        .spawn_pty(PtyProcessSpec {
            process: plan,
            environment,
            size: PtySize {
                rows: 24,
                columns: 80,
            },
            termination_grace_ms: 50,
        })
        .await
        .unwrap();
    let mut output = vec![];
    tokio::time::timeout(Duration::from_secs(5), async {
        while !output.windows(9).any(|bytes| bytes == b"PTY_READY") {
            output.extend(process.read().await.unwrap().bytes);
            assert!(output.len() < 4096);
        }
        process
            .resize(PtySize {
                rows: 30,
                columns: 100,
            })
            .await
            .unwrap();
        assert_eq!(process.write(b"OK\n").await.unwrap(), 3);
        loop {
            let read = process.read().await.unwrap();
            output.extend(read.bytes);
            assert!(output.len() < 4096);
            if read.eof {
                break;
            }
        }
        assert_eq!(process.wait().await.unwrap().exit_code, Some(0));
    })
    .await
    .unwrap();
    assert!(output.windows(6).any(|bytes| bytes == b"PTY_OK"));
    assert_eq!(fixture.gate.active.load(Ordering::SeqCst), 0);
    drop(process);
    fixture.close().await;
}

#[tokio::test]
async fn equal_program_paths_preserve_distinct_selected_environments_without_ambient_fallback() {
    let fixture = Fixture::new(false).await;
    assert_eq!(
        fixture.lease.resolve_program("shell").await.unwrap().path(),
        fixture
            .lease
            .resolve_program("shell-alternative")
            .await
            .unwrap()
            .path()
    );
    assert!(matches!(
        fixture.lease.resolve_program("sh").await,
        Err(ProcessError::Unsupported)
    ));
    for (selector, expected) in [
        ("shell", b"owned-local".as_slice()),
        ("shell-alternative", b"different-policy".as_slice()),
    ] {
        let plan = fixture
            .plan(
                selector,
                Some("printf '%s' \"$FIXTURE\""),
                ProcessStdio::Pipes,
            )
            .await;
        let environment = plan.environment().to_vec();
        let process = fixture
            .lease
            .spawn(ProcessSpec {
                process: plan,
                environment,
                stdin: vec![],
                stdout_max_bytes: 1024,
                stderr_max_bytes: 1024,
                termination_grace_ms: 50,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(process.stdout().read_from(0).unwrap().bytes, expected);
    }
    fixture.close().await;
}

#[tokio::test]
async fn explicit_local_program_keeps_contribution_environment_and_exact_lease() {
    let fixture = Fixture::new(false).await;
    let program = fixture
        .lease
        .resolve_local_program(ResolvedProgram {
            program: "/bin/sh".into(),
            environment: vec![("FIXTURE".into(), "contribution-owned".into())],
        })
        .await
        .unwrap();
    let plan = fixture
        .lease
        .prepare(ProcessRequest {
            stdio: ProcessStdio::Pipes,
            mode: SandboxMode::DangerFullAccess,
            program,
            arguments: vec!["-c".into(), "printf %s \"$FIXTURE\"".into()],
            cwd: fixture.root.path().canonicalize().unwrap(),
            workspace: fixture.root.path().canonicalize().unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(plan.identity().binding(), fixture.lease.binding());
    let environment = plan.environment().to_vec();
    let process = fixture
        .lease
        .spawn(ProcessSpec {
            process: plan,
            environment,
            stdin: vec![],
            stdout_max_bytes: 128,
            stderr_max_bytes: 128,
            termination_grace_ms: 50,
        })
        .await
        .unwrap();
    assert_eq!(process.wait().await.unwrap().exit_code, Some(0));
    assert_eq!(
        process.stdout().read_from(0).unwrap().bytes,
        b"contribution-owned"
    );
    assert!(
        fixture
            .lease
            .resolve_local_program(ResolvedProgram {
                program: "relative".into(),
                environment: vec![]
            })
            .await
            .is_err()
    );
    assert!(
        fixture
            .lease
            .resolve_local_program(ResolvedProgram {
                program: "/bin/sh".into(),
                environment: vec![
                    ("duplicate".into(), "a".into()),
                    ("duplicate".into(), "b".into())
                ]
            })
            .await
            .is_err()
    );
    drop(process);
    fixture.close().await;
}
