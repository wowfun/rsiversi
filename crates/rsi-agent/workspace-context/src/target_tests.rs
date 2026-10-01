use super::*;
use rsi_execution::{
    BackendPlan, ExecutionAdmission, ExecutionAdmissionKind, ExecutionBackend, ExecutionOperation,
    ExecutionPin, ExecutionProvider, HostEpoch, ResolvedProgram,
};
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FileToken, Files, FilesBinding, FilesCaller, OpenedFile,
    RelativePath,
};
use rsi_process::*;
use rsi_sandbox::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

type PResult<T> = rsi_process::Result<T>;
#[derive(Debug)]
struct Gate;
impl ExecutionAdmission for Gate {
    fn admit(&self, _: ExecutionAdmissionKind) -> PResult<ExecutionOperation> {
        Ok(ExecutionOperation::new(()))
    }
}
#[derive(Debug, Default)]
struct Wire {
    input: Mutex<Vec<u8>>,
    output: Mutex<std::collections::VecDeque<u8>>,
    closed: AtomicBool,
    terminated: AtomicBool,
    settled: AtomicBool,
    spawned: AtomicUsize,
    stall: AtomicBool,
    reading: tokio::sync::Notify,
    bad_exit: bool,
}
#[async_trait]
impl DuplexInput for Wire {
    async fn write(&self, bytes: &[u8]) -> PResult<usize> {
        let count = bytes.len().min(7); // Exercise acknowledged partial writes.
        self.input
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    async fn close(&self) -> PResult<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait]
impl DuplexOutput for Wire {
    async fn read(&self, maximum: usize) -> PResult<DuplexRead> {
        assert!(self.closed.load(Ordering::SeqCst));
        self.reading.notify_one();
        if self.stall.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let mut output = self.output.lock().unwrap();
        let count = output.len().min(maximum);
        let bytes = output.drain(..count).collect();
        Ok(DuplexRead {
            bytes,
            eof: output.is_empty(),
        })
    }
}
#[derive(Debug)]
struct Child {
    wire: Arc<Wire>,
    pin: Mutex<Option<ExecutionPin>>,
}
#[async_trait]
impl DuplexControl for Child {
    fn pid(&self) -> u32 {
        1
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.wire.clone()
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.wire.clone()
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        panic!("context must not publish helper stderr")
    }
    fn terminate(&self) {
        self.wire.terminated.store(true, Ordering::SeqCst);
    }
    async fn wait(&self) -> PResult<ProcessOutcome> {
        self.wait_settlement().await?;
        Ok(ProcessOutcome {
            exit_code: Some(i32::from(self.wire.bad_exit)),
            signal: None,
        })
    }
    async fn wait_settlement(&self) -> PResult<()> {
        self.pin.lock().unwrap().take();
        self.wire.settled.store(true, Ordering::SeqCst);
        Ok(())
    }
}
#[derive(Debug)]
struct Backend {
    wire: Arc<Wire>,
    network: SandboxNetwork,
    filesystem: SandboxFileSystem,
}
#[async_trait]
impl ExecutionBackend for Backend {
    async fn canonicalize(&self, _: &str) -> PResult<String> {
        panic!("no Service path lookup")
    }
    async fn resolve_program(&self, selector: &str) -> PResult<ResolvedProgram> {
        assert_eq!(selector, "workspace_context");
        Ok(ResolvedProgram {
            program: "/helper".into(),
            environment: vec![],
        })
    }
    async fn prepare(
        &self,
        _: ProcessRequest,
        _: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> PResult<BackendPlan> {
        panic!("ordinary confinement hides host scratch")
    }
    async fn prepare_source_reader(
        &self,
        request: ProcessRequest,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> PResult<BackendPlan> {
        assert_eq!(request.program, PathBuf::from("/helper"));
        assert_eq!(request.arguments, [project::MARKER]);
        assert_eq!(request.cwd, PathBuf::from("/remote/project"));
        Ok(BackendPlan::new(
            (),
            EnforcementStamp {
                requested: request.mode,
                backend: SandboxBackend::Bubblewrap {
                    sha256: "a".repeat(64),
                },
                workspace: request.workspace,
                filesystem: self.filesystem,
                scratch: SandboxScratch::Host,
                network: self.network,
            },
            environment,
        ))
    }
    async fn spawn(&self, _: ProcessSpec<BackendPlan>, _: ExecutionPin) -> PResult<ManagedProcess> {
        panic!("context requires lossless output")
    }
    async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> PResult<ManagedDuplexProcess> {
        spec.process.take::<()>()?;
        self.wire.spawned.fetch_add(1, Ordering::SeqCst);
        Ok(ManagedDuplexProcess::new(Arc::new(Child {
            wire: self.wire.clone(),
            pin: Mutex::new(Some(pin)),
        })))
    }
    async fn spawn_pty(
        &self,
        _: PtyProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> PResult<ManagedPtyProcess> {
        panic!("context must not launch a terminal")
    }
    async fn workspace_read(&self, _: WorkspaceReadRequest) -> PResult<WorkspaceReadScope> {
        panic!("context uses target reader")
    }
    fn files(&self) -> Arc<dyn rsi_files_protocol::Files> {
        Arc::new(NoFiles)
    }
}
fn lease(
    wire: Arc<Wire>,
    network: SandboxNetwork,
    filesystem: SandboxFileSystem,
) -> ExecutionLease {
    ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        rsi_execution::ExecutionLocation::Ssh {
            target: rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        1,
        1,
        Arc::new(Backend {
            wire,
            network,
            filesystem,
        }),
    )
    .unwrap()
    .lease(Arc::new(Gate))
    .unwrap()
}
fn request() -> Request {
    Request::Skills {
        id: None,
        audience: SkillAudience::Model,
    }
}
fn encoded_capture(context: project::Context) -> Vec<u8> {
    serde_json::to_vec(&project::Reply::Captured {
        capture: Capture::Context(context),
    })
    .unwrap()
}
#[tokio::test]
async fn remote_exchange_bounds_output_checks_exit_and_validates_capture() {
    let mut hostile = project::Context::default();
    hostile
        .instructions
        .push(("AGENTS.md".into(), "not requested by skill listing".into()));
    for (output, bad_exit, expected) in [
        (encoded_capture(project::Context::default()), false, "ok"),
        (encoded_capture(project::Context::default()), true, "failed"),
        (encoded_capture(hostile), false, "invalid"),
        (
            b"{\"result\":\"captured\",\"extra\":true}".to_vec(),
            false,
            "invalid",
        ),
        (
            vec![b' '; project::MAXIMUM_RESPONSE_BYTES + 1],
            false,
            "capacity",
        ),
    ] {
        let wire = Arc::new(Wire {
            output: Mutex::new(output.into()),
            bad_exit,
            ..Wire::default()
        });
        let lease = lease(
            wire.clone(),
            SandboxNetwork::Isolated,
            SandboxFileSystem::ReadOnly,
        );
        let result = exchange(
            &lease,
            Path::new("/remote/project"),
            &request(),
            CancellationToken::new(),
        )
        .await;
        match expected {
            "ok" => {
                result.unwrap();
            }
            "failed" => assert!(matches!(result, Err(WorkspaceContextError::Failed(_)))),
            "invalid" => assert!(matches!(result, Err(WorkspaceContextError::Invalid(_)))),
            "capacity" => assert_eq!(result.unwrap_err(), WorkspaceContextError::Capacity),
            _ => unreachable!(),
        }
        assert_eq!(
            *wire.input.lock().unwrap(),
            serde_json::to_vec(&request()).unwrap()
        );
        assert!(wire.settled.load(Ordering::SeqCst));
        if matches!(expected, "failed" | "capacity") {
            assert!(wire.terminated.load(Ordering::SeqCst));
        }
    }
}
#[tokio::test]
async fn remote_exchange_rejects_false_enforcement_before_spawn_and_reaps_on_cancel() {
    for (network, filesystem) in [
        (SandboxNetwork::Host, SandboxFileSystem::ReadOnly),
        (SandboxNetwork::Isolated, SandboxFileSystem::Unconfined),
    ] {
        let wire = Arc::new(Wire::default());
        assert!(
            exchange(
                &lease(wire.clone(), network, filesystem),
                Path::new("/remote/project"),
                &request(),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert_eq!(wire.spawned.load(Ordering::SeqCst), 0);
    }
    let wire = Arc::new(Wire::default());
    wire.stall.store(true, Ordering::SeqCst);
    let lease = lease(
        wire.clone(),
        SandboxNetwork::Isolated,
        SandboxFileSystem::ReadOnly,
    );
    let stop = CancellationToken::new();
    let waiting = tokio::spawn({
        let stop = stop.clone();
        async move { exchange(&lease, Path::new("/remote/project"), &request(), stop).await }
    });
    tokio::time::timeout(Duration::from_secs(2), wire.reading.notified())
        .await
        .unwrap();
    stop.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        WorkspaceContextError::Closed
    );
    assert!(wire.terminated.load(Ordering::SeqCst));
    assert!(wire.settled.load(Ordering::SeqCst));
}

#[derive(Debug)]
struct NoFiles;
#[async_trait]
impl Files for NoFiles {
    fn release_caller(&self, _: &FilesCaller) {}
    fn describe(&self, _: &FilesBinding, _: &FileToken) -> rsi_files_protocol::Result<OpenedFile> {
        unreachable!("metadata described a file")
    }
    async fn open(
        &self,
        _: FilesBinding,
        _: RelativePath,
        _: FileKind,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<OpenedFile> {
        unreachable!("metadata opened a file")
    }
    async fn read(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: u64,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<FilePage> {
        unreachable!("metadata read a file")
    }
    async fn list(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: usize,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<DirectoryPage> {
        unreachable!("metadata listed files")
    }
    fn release(&self, _: &FilesBinding, _: &FileToken) -> rsi_files_protocol::Result<()> {
        unreachable!("metadata released an unopened file")
    }
}
