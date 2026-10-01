use async_trait::async_trait;
use rsi_execution::*;
use rsi_files_protocol::{Files, *};
use rsi_process::*;
use rsi_sandbox::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

type PResult<T> = rsi_process::Result<T>;
#[derive(Debug, Default)]
struct Gate {
    revoked: AtomicBool,
    active: Arc<AtomicUsize>,
    admits: AtomicUsize,
    capacity: AtomicBool,
    scope_full: AtomicBool,
    operation_full: AtomicBool,
    api_error: Mutex<Option<rsi_api_protocol::ApiError>>,
}
#[derive(Debug)]
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ExecutionAdmission for Gate {
    fn admit(&self, kind: rsi_execution::ExecutionAdmissionKind) -> PResult<ExecutionOperation> {
        if self.revoked.load(Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        if kind != ExecutionAdmissionKind::Publication && self.capacity.load(Ordering::SeqCst) {
            return Err(ProcessError::Capacity);
        }
        if (kind == ExecutionAdmissionKind::Scope && self.scope_full.load(Ordering::SeqCst))
            || (kind == ExecutionAdmissionKind::Operation
                && self.operation_full.load(Ordering::SeqCst))
        {
            return Err(ProcessError::Capacity);
        }
        if let Some(error) = self.api_error.lock().unwrap().clone() {
            return Err(ProcessError::Api(error));
        }
        self.admits.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionOperation::new(Permit(self.active.clone())))
    }
}
#[derive(Debug, Default)]
struct Events {
    starts: AtomicUsize,
    prepares: AtomicUsize,
    writes: AtomicUsize,
    resizes: AtomicUsize,
    reads: AtomicUsize,
    terminated: AtomicBool,
    settled: Notify,
    entered: Notify,
    resume: Notify,
    delay_start: AtomicBool,
    delay_write: AtomicBool,
    delay_read: AtomicBool,
    read_entered: Notify,
    read_resume: Notify,
    written: Notify,
    file_released: Notify,
    callers_released: AtomicUsize,
    fail_open: AtomicBool,
}
#[derive(Debug)]
struct Backend(Arc<Events>);
#[derive(Debug)]
struct Child {
    events: Arc<Events>,
    start: Mutex<Option<ExecutionPin>>,
}
impl Child {
    fn terminate(&self) {
        self.events.terminated.store(true, Ordering::SeqCst);
    }
    fn settle(&self) -> ProcessOutcome {
        self.start.lock().unwrap().take();
        self.events.settled.notify_one();
        ProcessOutcome {
            exit_code: Some(0),
            signal: None,
        }
    }
}
#[derive(Debug)]
struct Port(Arc<Events>);
impl ProcessOutput for Port {
    fn read_from(&self, _: u64) -> PResult<ProcessRead> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        Ok(ProcessRead {
            bytes: vec![],
            oldest_offset: 0,
            next_offset: 0,
            lossy: false,
            full_output: None,
        })
    }
    fn peek_tail(&self, _: usize) -> PResult<ProcessRead> {
        self.read_from(0)
    }
}
#[async_trait]
impl DuplexInput for Port {
    async fn write(&self, bytes: &[u8]) -> PResult<usize> {
        self.0.writes.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        if self.0.delay_write.load(Ordering::SeqCst) {
            self.0.resume.notified().await;
        }
        self.0.written.notify_one();
        Ok(bytes.len())
    }
    async fn close(&self) -> PResult<()> {
        Ok(())
    }
}
#[async_trait]
impl DuplexOutput for Port {
    async fn read(&self, _: usize) -> PResult<DuplexRead> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        self.0.read_entered.notify_one();
        if self.0.delay_read.load(Ordering::SeqCst) {
            self.0.read_resume.notified().await;
        }
        Ok(DuplexRead {
            bytes: b"x".to_vec(),
            eof: false,
        })
    }
}
#[async_trait]
impl ProcessControl for Child {
    fn pid(&self) -> u32 {
        1
    }
    fn stdout(&self) -> Arc<dyn ProcessOutput> {
        Arc::new(Port(self.events.clone()))
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        Arc::new(Port(self.events.clone()))
    }
    fn terminate(&self) {
        self.terminate();
    }
    async fn wait(&self) -> PResult<ProcessOutcome> {
        Ok(self.settle())
    }
}
#[async_trait]
impl DuplexControl for Child {
    fn pid(&self) -> u32 {
        1
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        Arc::new(Port(self.events.clone()))
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        Arc::new(Port(self.events.clone()))
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        Arc::new(Port(self.events.clone()))
    }
    fn terminate(&self) {
        self.terminate();
    }
    async fn wait(&self) -> PResult<ProcessOutcome> {
        Ok(self.settle())
    }
    async fn wait_settlement(&self) -> PResult<()> {
        self.settle();
        Ok(())
    }
}
#[async_trait]
impl PtyControl for Child {
    fn pid(&self) -> u32 {
        1
    }
    async fn read(&self) -> PResult<PtyRead> {
        let read = DuplexOutput::read(&Port(self.events.clone()), 1).await?;
        Ok(PtyRead {
            bytes: read.bytes,
            eof: read.eof,
        })
    }
    async fn write(&self, bytes: &[u8]) -> PResult<usize> {
        Port(self.events.clone()).write(bytes).await
    }
    async fn resize(&self, _: PtySize) -> PResult<()> {
        self.events.resizes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn terminate(&self) {
        self.terminate();
    }
    async fn wait(&self) -> PResult<ProcessOutcome> {
        Ok(self.settle())
    }
}
impl Backend {
    async fn child(&self, plan: BackendPlan, start: ExecutionPin) -> PResult<Arc<Child>> {
        plan.take::<()>()?;
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        if self.0.delay_start.load(Ordering::SeqCst) {
            self.0.resume.notified().await;
        }
        Ok(Arc::new(Child {
            events: self.0.clone(),
            start: Mutex::new(Some(start)),
        }))
    }
}
fn request() -> ProcessRequest {
    ProcessRequest {
        stdio: ProcessStdio::Pipes,
        mode: SandboxMode::DangerFullAccess,
        program: "/configured/program".into(),
        arguments: vec![],
        cwd: "/project".into(),
        workspace: "/project".into(),
    }
}
fn stamp() -> EnforcementStamp {
    EnforcementStamp {
        requested: SandboxMode::DangerFullAccess,
        backend: SandboxBackend::Unconfined,
        workspace: "/project".into(),
        filesystem: SandboxFileSystem::Unconfined,
        scratch: SandboxScratch::Host,
        network: SandboxNetwork::Host,
    }
}
#[async_trait]
impl ExecutionBackend for Backend {
    async fn canonicalize(&self, path: &str) -> PResult<String> {
        Ok(path.into())
    }
    async fn resolve_program(&self, _: &str) -> PResult<ResolvedProgram> {
        Ok(ResolvedProgram {
            program: "/configured/program".into(),
            environment: vec![],
        })
    }
    async fn prepare(
        &self,
        _: ProcessRequest,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> PResult<BackendPlan> {
        self.0.prepares.fetch_add(1, Ordering::SeqCst);
        Ok(BackendPlan::new((), stamp(), environment))
    }
    async fn spawn(
        &self,
        spec: ProcessSpec<BackendPlan>,
        start: ExecutionPin,
    ) -> PResult<ManagedProcess> {
        Ok(ManagedProcess::new(self.child(spec.process, start).await?))
    }
    async fn prepare_source_reader(
        &self,
        request: ProcessRequest,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> PResult<BackendPlan> {
        // Deliberately reports ordinary unconfined evidence to test the wrapper's rejection.
        self.prepare(request, environment).await
    }
    async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<BackendPlan>,
        start: ExecutionPin,
    ) -> PResult<ManagedDuplexProcess> {
        Ok(ManagedDuplexProcess::new(
            self.child(spec.process, start).await?,
        ))
    }
    async fn spawn_pty(
        &self,
        spec: PtyProcessSpec<BackendPlan>,
        start: ExecutionPin,
    ) -> PResult<ManagedPtyProcess> {
        Ok(ManagedPtyProcess::new(
            self.child(spec.process, start).await?,
        ))
    }
    async fn workspace_read(&self, request: WorkspaceReadRequest) -> PResult<WorkspaceReadScope> {
        WorkspaceReadScope::new(request, SandboxGeneration::default())
            .map_err(|_| ProcessError::Unsupported)
    }
    fn files(&self) -> Arc<dyn Files> {
        Arc::new(FileReader(self.0.clone()))
    }
}
#[derive(Debug)]
struct FileReader(Arc<Events>);
#[async_trait]
impl Files for FileReader {
    fn release_caller(&self, _: &FilesCaller) {
        self.0.callers_released.fetch_add(1, Ordering::SeqCst);
    }
    fn describe(&self, _: &FilesBinding, _: &FileToken) -> rsi_files_protocol::Result<OpenedFile> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        Err(FilesError::Unavailable)
    }
    async fn open(
        &self,
        _: FilesBinding,
        _: RelativePath,
        _: FileKind,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<OpenedFile> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        let failed = self.0.fail_open.load(Ordering::SeqCst);
        if self.0.delay_start.load(Ordering::SeqCst) {
            self.0.resume.notified().await;
        }
        if failed {
            return Err(FilesError::Unavailable);
        }
        Ok(OpenedFile {
            executable: false,
            path: RelativePath::default(),
            token: FileToken::try_from("a".repeat(32)).unwrap(),
            kind: FileKind::Directory,
            length: 0,
        })
    }
    async fn read(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: u64,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<FilePage> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        Err(FilesError::Unavailable)
    }
    async fn list(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: usize,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<DirectoryPage> {
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        Err(FilesError::Unavailable)
    }
    fn release(&self, _: &FilesBinding, _: &FileToken) -> rsi_files_protocol::Result<()> {
        self.0.file_released.notify_one();
        Ok(())
    }
}

#[tokio::test]
async fn failed_files_opens_do_not_exhaust_private_caller_capacity() {
    let events = Arc::new(Events::default());
    events.fail_open.store(true, Ordering::SeqCst);
    let lease = provider(&events, None, 0, 0)
        .lease(Arc::new(Gate::default()))
        .unwrap();
    let files = lease.files().unwrap();
    for _ in 0..128 {
        let binding = FilesBinding::new(
            FilesCaller::default(),
            "session",
            "header",
            "/project".into(),
        )
        .unwrap();
        assert_eq!(
            files
                .open(
                    binding,
                    RelativePath::default(),
                    FileKind::Directory,
                    CancellationToken::new()
                )
                .await,
            Err(FilesError::Unavailable)
        );
    }
    events.fail_open.store(false, Ordering::SeqCst);
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "session",
        "header",
        "/project".into(),
    )
    .unwrap();
    let opened = files
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    files.release(&binding, &opened.token).unwrap();
}

#[tokio::test]
async fn a_failed_concurrent_open_preserves_the_same_callers_published_token() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let events = Arc::new(Events::default());
    events.fail_open.store(true, Ordering::SeqCst);
    events.delay_start.store(true, Ordering::SeqCst);
    let lease = provider(&events, None, 0, 0)
        .lease(Arc::new(Gate::default()))
        .unwrap();
    let files = lease.files().unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "session",
        "header",
        "/project".into(),
    )
    .unwrap();
    let mut failed = Box::pin(files.open(
        binding.clone(),
        RelativePath::default(),
        FileKind::Directory,
        CancellationToken::new(),
    ));
    assert!(matches!(
        failed
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    notified(&events.entered).await;
    events.fail_open.store(false, Ordering::SeqCst);
    events.delay_start.store(false, Ordering::SeqCst);
    let opened = files
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    events.resume.notify_one();
    assert_eq!(failed.await, Err(FilesError::Unavailable));
    let before = events.reads.load(Ordering::SeqCst);
    assert_eq!(
        files.describe(&binding, &opened.token),
        Err(FilesError::Unavailable)
    );
    assert_eq!(
        events.reads.load(Ordering::SeqCst),
        before + 1,
        "the successful sibling still reaches its backend"
    );
    files.release_caller(binding.caller());
    assert_eq!(
        files.describe(&binding, &opened.token),
        Err(FilesError::Binding)
    );
}
fn provider(
    events: &Arc<Events>,
    target: Option<char>,
    revision: u64,
    epoch: u64,
) -> ExecutionProvider {
    let location = target.map_or(ExecutionLocation::Local, |ch| ExecutionLocation::Ssh {
        target: ExecutionTargetId::parse(ch.to_string().repeat(32)).unwrap(),
    });
    ExecutionProvider::new(
        rsi_execution::HostEpoch::from_bytes([17; 16]),
        location,
        revision,
        epoch,
        Arc::new(Backend(events.clone())),
    )
    .unwrap()
}
fn batch(process: PreparedProcess) -> ProcessSpec<PreparedProcess> {
    ProcessSpec {
        process,
        stdin: vec![],
        environment: vec![],
        stdout_max_bytes: 100,
        stderr_max_bytes: 100,
        termination_grace_ms: 50,
    }
}
fn duplex(process: PreparedProcess) -> DuplexProcessSpec<PreparedProcess> {
    DuplexProcessSpec {
        process,
        environment: vec![],
        stdout_buffer_bytes: 100,
        stderr_max_bytes: 100,
        termination_grace_ms: 50,
    }
}
fn pty(process: PreparedProcess) -> PtyProcessSpec<PreparedProcess> {
    PtyProcessSpec {
        process,
        environment: vec![],
        size: PtySize {
            rows: 24,
            columns: 80,
        },
        termination_grace_ms: 50,
    }
}
async fn notified(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(3), notify.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn serialized_plan_evidence_retains_issuing_host_epoch() {
    let events = Arc::new(Events::default());
    for bytes in [[7; 16], [9; 16]] {
        let epoch = rsi_execution::HostEpoch::from_bytes(bytes);
        let provider = ExecutionProvider::new(
            epoch.clone(),
            ExecutionLocation::Local,
            0,
            0,
            Arc::new(Backend(events.clone())),
        )
        .unwrap();
        let lease = provider.lease(Arc::new(Gate::default())).unwrap();
        let plan = prepare(&lease).await;
        assert_eq!(lease.binding().host_epoch(), &epoch);
        assert_eq!(plan.identity().binding().host_epoch(), &epoch);
        let evidence = serde_json::to_value(plan.identity()).unwrap();
        assert_eq!(evidence["binding"]["host_epoch"], epoch.as_str());
        assert_eq!(evidence["sequence"], 1);
    }
    assert_eq!(events.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn foreign_location_revision_epoch_generation_and_lease_reject_before_admission_or_io() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let original = provider(&events, Some('a'), 3, 5);
    let lease = original.lease(gate.clone()).unwrap();
    let other_providers = [
        provider(&events, None, 0, 0),
        provider(&events, Some('b'), 3, 5),
        provider(&events, Some('a'), 4, 5),
        provider(&events, Some('a'), 3, 6),
        provider(&events, Some('a'), 3, 5),
    ];
    let mut others = vec![original.lease(gate.clone()).unwrap()];
    others.extend(
        other_providers
            .iter()
            .map(|provider| provider.lease(gate.clone()).unwrap()),
    );
    for other in others {
        for kind in 0..3 {
            let plan = prepare(&lease).await;
            let admitted = gate.admits.load(Ordering::SeqCst);
            let failure = match kind {
                0 => other.spawn(batch(plan)).await.unwrap_err(),
                1 => other.spawn_duplex(duplex(plan)).await.unwrap_err(),
                _ => other.spawn_pty(pty(plan)).await.unwrap_err(),
            };
            assert!(matches!(failure, ProcessError::InvalidInput(_)));
            assert_eq!(gate.admits.load(Ordering::SeqCst), admitted);
            assert_eq!(events.starts.load(Ordering::SeqCst), 0);
        }
    }
    // Cloning the genuine lease preserves authority rather than manufacturing another seal.
    let plan = prepare(&lease).await;
    let process = lease.clone().spawn(batch(plan)).await.unwrap();
    process.wait().await.unwrap();
    assert_eq!(events.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn environment_substitution_is_rejected_before_backend_or_grant_admission() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let mut spec = batch(prepare(&lease).await);
    spec.environment
        .push(("HOME".into(), "/service/home".into()));
    let admitted = gate.admits.load(Ordering::SeqCst);
    assert!(matches!(
        lease.spawn(spec).await,
        Err(ProcessError::InvalidInput(_))
    ));
    assert_eq!(events.starts.load(Ordering::SeqCst), 0);
    assert_eq!(gate.admits.load(Ordering::SeqCst), admitted);
}

#[tokio::test]
async fn lost_start_waiter_retains_permit_then_terminates_and_joins_each_unpublished_child() {
    for kind in 0..3 {
        let events = Arc::new(Events::default());
        events.delay_start.store(true, Ordering::SeqCst);
        let gate = Arc::new(Gate::default());
        let backend = Arc::new(Backend(events.clone()));
        let weak = Arc::downgrade(&backend);
        let provider = ExecutionProvider::new(
            rsi_execution::HostEpoch::from_bytes([17; 16]),
            ExecutionLocation::Local,
            0,
            0,
            backend,
        )
        .unwrap();
        let lease = provider.lease(gate.clone()).unwrap();
        let plan = prepare(&lease).await;
        drop(provider);
        let waiter = tokio::spawn(async move {
            match kind {
                0 => {
                    lease.spawn(batch(plan)).await.unwrap();
                }
                1 => {
                    lease.spawn_duplex(duplex(plan)).await.unwrap();
                }
                _ => {
                    lease.spawn_pty(pty(plan)).await.unwrap();
                }
            }
        });
        notified(&events.entered).await;
        waiter.abort();
        let _ = waiter.await;
        gate.revoked.store(true, Ordering::SeqCst);
        assert_eq!(gate.active.load(Ordering::SeqCst), 1);
        assert!(weak.upgrade().is_some());
        events.resume.notify_one();
        notified(&events.settled).await;
        assert_eq!(events.starts.load(Ordering::SeqCst), 1);
        assert!(events.terminated.load(Ordering::SeqCst));
        assert_eq!(gate.active.load(Ordering::SeqCst), 0);
        assert!(weak.upgrade().is_none());
    }
}

#[tokio::test]
async fn revoked_grant_denies_new_ports_and_files_but_allows_cleanup() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let batch = lease.spawn(batch(prepare(&lease).await)).await.unwrap();
    let duplex = lease
        .spawn_duplex(duplex(prepare(&lease).await))
        .await
        .unwrap();
    let pty = lease.spawn_pty(pty(prepare(&lease).await)).await.unwrap();
    let files = lease.files().unwrap();
    let pending = prepare(&lease).await;
    gate.revoked.store(true, Ordering::SeqCst);
    assert!(lease.spawn(super_batch(pending)).await.is_err());
    assert_eq!(
        duplex.stdin().write(b"x").await,
        Err(ProcessError::ShuttingDown)
    );
    assert_eq!(
        duplex.stdout().read(1).await,
        Err(ProcessError::ShuttingDown)
    );
    assert_eq!(batch.stdout().read_from(0), Err(ProcessError::ShuttingDown));
    assert_eq!(pty.write(b"x").await, Err(ProcessError::ShuttingDown));
    assert_eq!(
        pty.resize(PtySize {
            rows: 30,
            columns: 90
        })
        .await,
        Err(ProcessError::ShuttingDown)
    );
    assert_eq!(pty.read().await, Err(ProcessError::ShuttingDown));
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "session",
        "header",
        "/project".into(),
    )
    .unwrap();
    let token = FileToken::try_from("a".repeat(32)).unwrap();
    assert_eq!(
        files
            .open(
                binding.clone(),
                RelativePath::default(),
                FileKind::Directory,
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Cancelled)
    );
    assert_eq!(
        files
            .read(
                binding.clone(),
                token.clone(),
                0,
                1,
                CancellationToken::new()
            )
            .await,
        Err(FilesError::Cancelled)
    );
    assert_eq!(files.describe(&binding, &token), Err(FilesError::Cancelled));
    assert_eq!(events.starts.load(Ordering::SeqCst), 3);
    assert_eq!(events.writes.load(Ordering::SeqCst), 0);
    assert_eq!(events.reads.load(Ordering::SeqCst), 0);
    assert_eq!(events.resizes.load(Ordering::SeqCst), 0);
    duplex.stdin().close().await.unwrap();
    files.release(&binding, &token).unwrap();
    batch.terminate();
    duplex.terminate();
    pty.terminate();
    batch.wait().await.unwrap();
    duplex.wait_settlement().await.unwrap();
    pty.wait().await.unwrap();
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
}
fn super_batch(plan: PreparedProcess) -> ProcessSpec<PreparedProcess> {
    batch(plan)
}

#[tokio::test]
async fn accepted_write_outlives_waiter_and_revocation_without_replay() {
    let events = Arc::new(Events::default());
    events.delay_write.store(true, Ordering::SeqCst);
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let process = lease
        .spawn_duplex(duplex(prepare(&lease).await))
        .await
        .unwrap();
    notified(&events.entered).await; // Consume the completed start notification.
    let input = process.stdin();
    let waiter = tokio::spawn(async move { input.write(b"once").await });
    notified(&events.entered).await;
    waiter.abort();
    let _ = waiter.await;
    gate.revoked.store(true, Ordering::SeqCst);
    assert_eq!(gate.active.load(Ordering::SeqCst), 1);
    events.resume.notify_one();
    notified(&events.written).await;
    assert_eq!(events.writes.load(Ordering::SeqCst), 1);
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    process.terminate();
    process.wait_settlement().await.unwrap();
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn abandoning_a_queued_start_reply_still_reaps_the_unpublished_process() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let plan = prepare(&lease).await;
    let mut waiting = Box::pin(lease.spawn(batch(plan)));
    assert!(matches!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    // The backend completes and the oneshot now owns a handle, but the caller
    // never polls its ready reply. Merely checking Sender::send failure misses this case.
    notified(&events.entered).await;
    drop(waiting);
    notified(&events.settled).await;
    assert!(events.terminated.load(Ordering::SeqCst));
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
}

#[test]
#[expect(
    clippy::async_yields_async,
    reason = "the test must move a pending waiter outside its runtime before dropping it"
)]
fn queued_start_reply_can_be_abandoned_outside_its_runtime() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    for runtime_shutdown in [false, true] {
        for kind in 0..3 {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let events = Arc::new(Events::default());
            let gate = Arc::new(Gate::default());
            let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
            let waiting = runtime.block_on(async {
                let plan = prepare(&lease).await;
                let mut waiting = Box::pin(async move {
                    match kind {
                        0 => {
                            lease.spawn(batch(plan)).await.unwrap();
                        }
                        1 => {
                            lease.spawn_duplex(duplex(plan)).await.unwrap();
                        }
                        _ => {
                            lease.spawn_pty(pty(plan)).await.unwrap();
                        }
                    }
                });
                assert!(matches!(
                    waiting
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
                notified(&events.entered).await;
                waiting
            });
            assert_eq!(gate.active.load(Ordering::SeqCst), 1);
            if runtime_shutdown {
                drop(runtime);
                drop(waiting);
                // A stopped executor cannot reap, but Drop must still request termination.
                assert!(events.terminated.load(Ordering::SeqCst));
            } else {
                std::thread::spawn(move || drop(waiting)).join().unwrap();
                assert!(events.terminated.load(Ordering::SeqCst));
                assert_eq!(gate.active.load(Ordering::SeqCst), 1);
                runtime.block_on(notified(&events.settled));
                assert_eq!(gate.active.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[tokio::test]
async fn files_open_retains_admission_and_releases_both_inflight_and_queued_unpublished_tokens() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    for delayed in [true, false] {
        let events = Arc::new(Events::default());
        events.delay_start.store(delayed, Ordering::SeqCst);
        let gate = Arc::new(Gate::default());
        let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
        let files = lease.files().unwrap();
        let binding = FilesBinding::new(
            FilesCaller::default(),
            "session",
            "header",
            "/project".into(),
        )
        .unwrap();
        let mut waiting = Box::pin(files.open(
            binding,
            RelativePath::default(),
            FileKind::Directory,
            CancellationToken::new(),
        ));
        assert!(matches!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        notified(&events.entered).await;
        drop(waiting);
        gate.revoked.store(true, Ordering::SeqCst);
        if delayed {
            assert_eq!(gate.active.load(Ordering::SeqCst), 1);
            events.resume.notify_one();
        }
        notified(&events.file_released).await;
        assert_eq!(gate.active.load(Ordering::SeqCst), 0);
        assert_eq!(events.reads.load(Ordering::SeqCst), 1);
    }
}

async fn prepare(lease: &ExecutionLease) -> PreparedProcess {
    let program = lease.resolve_program("fixture").await.unwrap();
    lease
        .prepare(request().map_program(|_| program))
        .await
        .unwrap()
}

#[tokio::test]
async fn source_reader_rejects_write_pty_foreign_programs_and_false_backend_evidence() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let first = provider(&events, Some('a'), 1, 1)
        .lease(gate.clone())
        .unwrap();
    let second = provider(&events, Some('a'), 1, 2).lease(gate).unwrap();
    for (mode, stdio, foreign) in [
        (SandboxMode::DangerFullAccess, ProcessStdio::Pipes, false),
        (SandboxMode::ReadOnly, ProcessStdio::Pty, false),
        (SandboxMode::ReadOnly, ProcessStdio::Pipes, true),
    ] {
        let program = if foreign { &second } else { &first }
            .resolve_program("fixture")
            .await
            .unwrap();
        let mut request = request().map_program(|_| program);
        request.mode = mode;
        request.stdio = stdio;
        assert!(matches!(
            first.prepare_source_reader(request).await,
            Err(ProcessError::InvalidInput(_))
        ));
    }
    assert_eq!(events.prepares.load(Ordering::SeqCst), 0);
    let program = first.resolve_program("fixture").await.unwrap();
    let mut request = request().map_program(|_| program);
    request.mode = SandboxMode::ReadOnly;
    assert!(matches!(
        first.prepare_source_reader(request).await,
        Err(ProcessError::Unsupported)
    ));
    assert_eq!(events.prepares.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn foreign_resolved_program_rejects_before_admission_or_preparation() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let first = provider(&events, Some('a'), 1, 1)
        .lease(gate.clone())
        .unwrap();
    let second = provider(&events, Some('a'), 1, 2)
        .lease(gate.clone())
        .unwrap();
    let program = first.resolve_program("fixture").await.unwrap();
    let admitted = gate.admits.load(Ordering::SeqCst);
    assert!(matches!(
        second.prepare(request().map_program(|_| program)).await,
        Err(ProcessError::InvalidInput(_))
    ));
    assert_eq!(gate.admits.load(Ordering::SeqCst), admitted);
    assert_eq!(events.prepares.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn retained_files_use_current_caller_and_reject_replacement_provider_before_io() {
    let events = Arc::new(Events::default());
    let provider = provider(&events, Some('a'), 1, 1);
    let creator = Arc::new(Gate::default());
    let first = provider.lease(creator.clone()).unwrap();
    let resource = first.retain_files().unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "session",
        "header",
        "/project".into(),
    )
    .unwrap();
    let opened = resource
        .view(&first)
        .unwrap()
        .open(
            binding.clone(),
            RelativePath::default(),
            FileKind::Directory,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(first);
    creator.revoked.store(true, Ordering::SeqCst);
    let current = Arc::new(Gate::default());
    let second = provider.lease(current.clone()).unwrap();
    let view = resource.view(&second).unwrap();
    let reads = events.reads.load(Ordering::SeqCst);
    assert_eq!(
        view.describe(&binding, &opened.token),
        Err(FilesError::Unavailable)
    );
    assert_eq!(events.reads.load(Ordering::SeqCst), reads + 1);
    drop(view);
    let view = resource.view(&second).unwrap();
    current.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        view.describe(&binding, &opened.token),
        Err(FilesError::Cancelled)
    );
    assert_eq!(events.reads.load(Ordering::SeqCst), reads + 1);
    let replacement = self::provider(&events, Some('a'), 1, 2)
        .lease(Arc::new(Gate::default()))
        .unwrap();
    assert!(matches!(
        resource.view(&replacement),
        Err(ProcessError::InvalidInput(_))
    ));
    assert_eq!(events.reads.load(Ordering::SeqCst), reads + 1);
    view.release(&binding, &opened.token).unwrap();
    notified(&events.file_released).await;
}

#[tokio::test]
async fn idle_stream_read_does_not_hold_revocation_open_or_publish_after_revocation() {
    let events = Arc::new(Events::default());
    events.delay_read.store(true, Ordering::SeqCst);
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let process = lease
        .spawn_duplex(duplex(prepare(&lease).await))
        .await
        .unwrap();
    let output = process.stdout();
    let waiter = tokio::spawn(async move { output.read(1).await });
    notified(&events.read_entered).await;
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    gate.revoked.store(true, Ordering::SeqCst);
    events.read_resume.notify_one();
    assert_eq!(waiter.await.unwrap(), Err(ProcessError::ShuttingDown));
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    process.terminate();
    process.wait_settlement().await.unwrap();
}

#[tokio::test]
async fn remote_lease_rejects_local_configuration_before_admission_or_backend_io() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, Some('a'), 1, 1)
        .lease(gate.clone())
        .unwrap();
    let before = gate.admits.load(Ordering::SeqCst);
    assert!(matches!(
        lease
            .resolve_local_program(ResolvedProgram {
                program: "/service-only/program".into(),
                environment: vec![("SERVICE_SECRET".into(), "test".into())],
            })
            .await,
        Err(ProcessError::InvalidInput(_))
    ));
    assert_eq!(gate.admits.load(Ordering::SeqCst), before);
    assert_eq!(events.prepares.load(Ordering::SeqCst), 0);
    assert_eq!(events.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn retained_terminal_uses_each_callers_lease_and_rejects_replacement_provider() {
    let events = Arc::new(Events::default());
    let provider = provider(&events, Some('a'), 1, 7);
    let creator_gate = Arc::new(Gate::default());
    let creator = provider.lease(creator_gate.clone()).unwrap();
    let other = provider.lease(Arc::new(Gate::default())).unwrap();
    let resource = creator
        .spawn_terminal(pty(prepare(&creator).await))
        .await
        .unwrap();
    creator_gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        resource.view(&creator),
        Err(ProcessError::ShuttingDown)
    ));
    let view = resource.view(&other).unwrap();
    assert_eq!(view.write(b"other-authorized-caller").await.unwrap(), 23);
    assert_eq!(events.writes.load(Ordering::SeqCst), 1);
    drop(view);
    resource
        .view(&other)
        .unwrap()
        .resize(rsi_process::PtySize {
            rows: 30,
            columns: 90,
        })
        .await
        .unwrap();
    // Private draining belongs to accepted bounded projection, not the revoked caller's access.
    resource.read_output().await.unwrap();
    let replacement = ExecutionProvider::new(
        HostEpoch::from_bytes([3; 16]),
        other.binding().location().clone(),
        1,
        8,
        Arc::new(Backend(events.clone())),
    )
    .unwrap()
    .lease(Arc::new(Gate::default()))
    .unwrap();
    assert!(matches!(
        resource.view(&replacement),
        Err(ProcessError::InvalidInput(_))
    ));
    assert_eq!(events.writes.load(Ordering::SeqCst), 1);
    resource.terminate();
    resource.wait().await.unwrap();
}

#[tokio::test]
async fn duplex_exchange_retains_actual_caller_and_rejects_revoked_or_foreign_new_work() {
    let events = Arc::new(Events::default());
    let provider = provider(&events, Some('a'), 1, 7);
    let creator_gate = Arc::new(Gate::default());
    let creator = provider.lease(creator_gate.clone()).unwrap();
    let gate = Arc::new(Gate::default());
    let current = provider.lease(gate.clone()).unwrap();
    let resource = creator
        .spawn_duplex_server(duplex(prepare(&creator).await))
        .await
        .unwrap();
    creator_gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        resource.exchange(&creator),
        Err(ProcessError::ShuttingDown)
    ));
    let exchange = Arc::new(resource.exchange(&current).unwrap());
    notified(&events.entered).await; // The completed spawn has its own notification.
    events.delay_write.store(true, Ordering::SeqCst);
    let worker = {
        let exchange = exchange.clone();
        tokio::spawn(async move { exchange.write(b"accepted").await })
    };
    events.entered.notified().await;
    gate.revoked.store(true, Ordering::SeqCst);
    worker.abort();
    let _ = worker.await;
    assert!(matches!(
        resource.exchange(&current),
        Err(ProcessError::ShuttingDown)
    ));
    assert_eq!(gate.active.load(Ordering::SeqCst), 1);
    drop(exchange);
    assert_eq!(gate.active.load(Ordering::SeqCst), 1);
    events.resume.notify_one();
    events.written.notified().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while gate.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let foreign = ExecutionProvider::new(
        HostEpoch::from_bytes([3; 16]),
        current.binding().location().clone(),
        1,
        8,
        Arc::new(Backend(events.clone())),
    )
    .unwrap()
    .lease(Arc::new(Gate::default()))
    .unwrap();
    assert!(matches!(
        resource.exchange(&foreign),
        Err(ProcessError::InvalidInput(_))
    ));
    assert_eq!(events.writes.load(Ordering::SeqCst), 1);
    resource.read_output(1).await.unwrap();
    events.delay_write.store(false, Ordering::SeqCst);
    assert_eq!(resource.protocol_reply(b"ack").await.unwrap(), 3);
    assert_eq!(
        gate.active.load(Ordering::SeqCst),
        0,
        "protocol maintenance does not borrow revoked caller authority"
    );
    resource.terminate();
    resource.wait_settlement().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn duplex_exchange_has_one_frame_and_finite_time_budget() {
    let events = Arc::new(Events::default());
    let provider = provider(&events, Some('a'), 1, 7);
    let lease = provider.lease(Arc::new(Gate::default())).unwrap();
    let resource = lease
        .spawn_duplex_server(duplex(prepare(&lease).await))
        .await
        .unwrap();
    let exchange = resource.exchange(&lease).unwrap();
    let bytes = vec![0; MAXIMUM_DUPLEX_CHUNK_BYTES];
    for _ in 0..1024 * 1024 / MAXIMUM_DUPLEX_CHUNK_BYTES {
        exchange.write(&bytes).await.unwrap();
    }
    exchange.write(b"\n").await.unwrap();
    assert!(matches!(
        exchange.write(b"x").await,
        Err(ProcessError::Capacity)
    ));
    let exchange = resource.exchange(&lease).unwrap();
    tokio::time::advance(Duration::from_secs(30)).await;
    let writes = events.writes.load(Ordering::SeqCst);
    assert!(matches!(
        exchange.write(b"x").await,
        Err(ProcessError::Capacity)
    ));
    assert_eq!(events.writes.load(Ordering::SeqCst), writes);
    resource.terminate();
    resource.wait_settlement().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn idle_and_blocked_duplex_exchanges_expire_without_another_write() {
    for blocked in [false, true] {
        let events = Arc::new(Events::default());
        let gate = Arc::new(Gate::default());
        let lease = provider(&events, Some('a'), 1, 7)
            .lease(gate.clone())
            .unwrap();
        let resource = lease
            .spawn_duplex_server(duplex(prepare(&lease).await))
            .await
            .unwrap();
        let exchange = Arc::new(resource.exchange(&lease).unwrap());
        notified(&events.entered).await;
        let worker = if blocked {
            events.delay_write.store(true, Ordering::SeqCst);
            let exchange = exchange.clone();
            let worker = tokio::spawn(async move { exchange.write(b"blocked").await });
            events.entered.notified().await;
            Some(worker)
        } else {
            None
        };
        assert_eq!(gate.active.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(31)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(events.terminated.load(Ordering::SeqCst));
        assert_eq!(gate.active.load(Ordering::SeqCst), 0);
        if let Some(worker) = worker {
            assert!(worker.is_finished());
            assert!(matches!(
                worker.await.unwrap(),
                Err(ProcessError::OutcomeUnknown)
            ));
        }
        assert!(matches!(
            exchange.write(b"late").await,
            Err(ProcessError::Capacity)
        ));
    }
}

#[tokio::test]
async fn stream_publication_preserves_consumed_bytes_when_effect_capacity_fills() {
    let events = Arc::new(Events::default());
    events.delay_read.store(true, Ordering::SeqCst);
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let process = lease
        .spawn_duplex(duplex(prepare(&lease).await))
        .await
        .unwrap();
    let output = process.stdout();
    let waiter = tokio::spawn(async move { output.read(1).await });
    notified(&events.read_entered).await;
    gate.capacity.store(true, Ordering::SeqCst);
    assert!(matches!(lease.admit(), Err(ProcessError::Capacity)));
    events.read_resume.notify_one();
    assert_eq!(waiter.await.unwrap().unwrap().bytes, b"x");
    process.terminate();
    process.wait_settlement().await.unwrap();
}

#[tokio::test]
async fn pty_publication_preserves_consumed_bytes_when_effect_capacity_fills() {
    let events = Arc::new(Events::default());
    events.delay_read.store(true, Ordering::SeqCst);
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let process = lease.spawn_pty(pty(prepare(&lease).await)).await.unwrap();
    let reader = process.clone();
    let waiter = tokio::spawn(async move { reader.read().await });
    notified(&events.read_entered).await;
    gate.capacity.store(true, Ordering::SeqCst);
    events.read_resume.notify_one();
    assert_eq!(waiter.await.unwrap().unwrap().bytes, b"x");
    process.terminate();
    process.wait().await.unwrap();
}

#[test]
fn file_admission_preserves_nested_capacity_and_unknown_outcomes_without_io() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, None, 0, 0).lease(gate.clone()).unwrap();
    let files = lease.files().unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "session",
        "header",
        "/project".into(),
    )
    .unwrap();
    let token = FileToken::try_from("a".repeat(32)).unwrap();
    for (error, expected) in [
        (rsi_api_protocol::ApiError::Capacity, FilesError::Capacity),
        (
            rsi_api_protocol::ApiError::OutcomeUnknown,
            FilesError::OutcomeUnknown,
        ),
    ] {
        *gate.api_error.lock().unwrap() = Some(error);
        assert_eq!(files.describe(&binding, &token), Err(expected));
    }
    assert_eq!(events.reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn only_last_retained_files_owner_releases_private_callers() {
    let events = Arc::new(Events::default());
    let lease = provider(&events, None, 0, 0)
        .lease(Arc::new(Gate::default()))
        .unwrap();
    let resource = lease.retain_files().unwrap();
    let first = resource.view(&lease).unwrap();
    let last = resource.view(&lease).unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "session",
        "header",
        "/project".into(),
    )
    .unwrap();
    first
        .open(
            binding,
            RelativePath::default(),
            FileKind::Directory,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop((lease, first, resource));
    assert_eq!(events.callers_released.load(Ordering::SeqCst), 0);
    drop(last);
    assert_eq!(events.callers_released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn duplex_exchanges_use_operation_capacity_independently_of_caller_scopes() {
    let events = Arc::new(Events::default());
    let gate = Arc::new(Gate::default());
    let lease = provider(&events, Some('a'), 1, 7)
        .lease(gate.clone())
        .unwrap();
    let resource = lease
        .spawn_duplex_server(duplex(prepare(&lease).await))
        .await
        .unwrap();
    gate.scope_full.store(true, Ordering::SeqCst);
    assert!(matches!(lease.admit(), Err(ProcessError::Capacity)));
    let active = gate.active.load(Ordering::SeqCst);
    let exchange = resource.exchange(&lease).unwrap();
    exchange.write(b"request\n").await.unwrap();
    assert_eq!(gate.active.load(Ordering::SeqCst), active + 1);
    drop(exchange);
    assert_eq!(gate.active.load(Ordering::SeqCst), active);
    gate.scope_full.store(false, Ordering::SeqCst);
    gate.operation_full.store(true, Ordering::SeqCst);
    let scope = lease.admit().unwrap();
    assert!(matches!(
        resource.exchange(&lease),
        Err(ProcessError::Capacity)
    ));
    drop(scope);
    resource.terminate();
    resource.wait_settlement().await.unwrap();
}
