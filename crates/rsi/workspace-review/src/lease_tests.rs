//! Routing fixture: real private Git, recorded target selection, no SSH transport claim.
use super::*;
use async_trait::async_trait;
use rsi_execution::*;
use rsi_files_protocol::*;
use rsi_process::*;
use rsi_sandbox::*;
use std::{
    ffi::OsString,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug)]
struct TargetFiles {
    inner: rsi_files::LocalFiles,
    unavailable: AtomicBool,
}
#[async_trait]
impl Files for TargetFiles {
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        stop: CancellationToken,
    ) -> rsi_files_protocol::Result<OpenedFile> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(FilesError::Unavailable);
        }
        self.inner.open(binding, path, kind, stop).await
    }
    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        stop: CancellationToken,
    ) -> rsi_files_protocol::Result<FilePage> {
        self.inner.read(binding, token, offset, maximum, stop).await
    }
    async fn list(
        &self,
        _: FilesBinding,
        _: FileToken,
        _: usize,
        _: usize,
        _: CancellationToken,
    ) -> rsi_files_protocol::Result<DirectoryPage> {
        panic!("inventory must use target Git");
    }
    fn describe(
        &self,
        binding: &FilesBinding,
        token: &FileToken,
    ) -> rsi_files_protocol::Result<OpenedFile> {
        self.inner.describe(binding, token)
    }
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> rsi_files_protocol::Result<()> {
        self.inner.release(binding, token)
    }
    fn release_caller(&self, caller: &FilesCaller) {
        self.inner.release_caller(caller);
    }
}
#[derive(Debug)]
struct Backend {
    process: Arc<dyn Process>,
    files: Arc<TargetFiles>,
    resolved: AtomicUsize,
}
#[async_trait]
impl ExecutionBackend for Backend {
    async fn canonicalize(&self, _: &str) -> rsi_process::Result<String> {
        panic!("claim workspace already canonical");
    }
    async fn resolve_program(&self, selector: &str) -> rsi_process::Result<ResolvedProgram> {
        assert_eq!(selector, "review_git");
        self.resolved.fetch_add(1, Ordering::SeqCst);
        Ok(ResolvedProgram {
            program: "/usr/bin/git".into(),
            environment: SOURCE_GIT_ENVIRONMENT
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        })
    }
    async fn prepare(
        &self,
        _: ProcessRequest,
        _: Vec<(OsString, OsString)>,
    ) -> rsi_process::Result<BackendPlan> {
        panic!("inventory requires source-reader capability");
    }
    async fn prepare_source_reader(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
    ) -> rsi_process::Result<BackendPlan> {
        assert_eq!(request.mode, SandboxMode::ReadOnly);
        assert_eq!(request.stdio, ProcessStdio::Pipes);
        let mut plan = crate::tests::TestSandbox.confine(request).await.unwrap();
        // Recorded evidence exercises lease validation; this fixture does not claim native confinement.
        plan.stamp.filesystem = SandboxFileSystem::ReadOnly;
        plan.stamp.network = SandboxNetwork::Isolated;
        Ok(BackendPlan::new(plan.clone(), plan.stamp, environment))
    }
    async fn spawn(
        &self,
        spec: ProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> rsi_process::Result<ManagedProcess> {
        self.process
            .spawn(spec.try_map_process(|p| Ok(pin.retain_native(p.take::<ConfinedProcess>()?)))?)
            .await
    }
    async fn spawn_duplex(
        &self,
        _: DuplexProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedDuplexProcess> {
        panic!("batch only");
    }
    async fn spawn_pty(
        &self,
        _: PtyProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedPtyProcess> {
        panic!("batch only");
    }
    async fn workspace_read(
        &self,
        request: WorkspaceReadRequest,
    ) -> rsi_process::Result<WorkspaceReadScope> {
        Ok(WorkspaceReadScope::new(request, SandboxGeneration::default()).unwrap())
    }
    fn files(&self) -> Arc<dyn Files> {
        self.files.clone()
    }
}
#[derive(Debug, Default)]
struct Gate(AtomicBool);
impl ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.0.load(Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        Ok(ExecutionOperation::new(()))
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one causal routing, mode, failure and revocation scenario"
)]
async fn target_inventory_bytes_and_modes_use_original_lease_and_failures_are_not_deletions() {
    use std::os::unix::fs::PermissionsExt as _;
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("target");
    std::fs::create_dir(&workspace).unwrap();
    crate::tests::command(&workspace, &["init", "--quiet"]);
    let path = workspace.join("run.sh");
    std::fs::write(&path, "before\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let runtime = rsi_meta::Runtime::default();
    let process_owner = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "process",
                "test",
                rsi_meta::UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    let process = runtime.root().lookup_local::<ProcessContract>().unwrap();
    let files = Arc::new(TargetFiles {
        inner: rsi_files::LocalFiles::new().unwrap(),
        unavailable: AtomicBool::new(false),
    });
    let backend = Arc::new(Backend {
        process: process.clone(),
        files: files.clone(),
        resolved: AtomicUsize::new(0),
    });
    let gate = Arc::new(Gate::default());
    let execution = ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        ExecutionLocation::Ssh {
            target: ExecutionTargetId::parse("b".repeat(32)).unwrap(),
        },
        1,
        7,
        backend.clone(),
    )
    .unwrap()
    .lease(gate.clone())
    .unwrap();
    let local_files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    local_files.close().await; // Any accidental native fallback fails the capture.
    let git = Git {
        process,
        sandbox: Arc::new(crate::tests::TestSandbox),
        files: local_files,
        program: "/usr/bin/git".into(),
        quota: Arc::new(Semaphore::new(1024 * 1024 * 1024)),
        tasks: tokio_util::task::TaskTracker::new(),
    };
    let root = crate::scratch_root::Root::open(temporary.path().join("scratch"))
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let mut scratch = git.initialize(&root.path, &stop).await.unwrap();
    let before = git
        .capture(&workspace, &mut scratch, &stop, Some(&execution))
        .await;
    assert!(before.omissions.is_empty());
    assert_eq!(before.files["run.sh"].1, 0o100_644);
    std::fs::write(&path, "after\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let after = git
        .capture(&workspace, &mut scratch, &stop, Some(&execution))
        .await;
    assert_eq!(after.files["run.sh"].1, 0o100_755);
    let comparison = git.compare(&scratch, &before, &after, &stop).await.unwrap();
    let diff = git
        .diff(&scratch, &comparison, &comparison.files[0], &stop)
        .await
        .unwrap();
    assert!(diff.contains("-before") && diff.contains("+after"));
    assert!(diff.contains("old mode 100644") && diff.contains("new mode 100755"));
    files.unavailable.store(true, Ordering::SeqCst);
    let unreadable = git
        .capture(&workspace, &mut scratch, &stop, Some(&execution))
        .await;
    assert!(
        unreadable
            .omissions
            .iter()
            .any(|row| row.kind == OmissionKind::Unreadable)
    );
    assert!(
        git.compare(&scratch, &before, &unreadable, &stop)
            .await
            .unwrap()
            .files
            .is_empty()
    );
    let resolved = backend.resolved.load(Ordering::SeqCst);
    gate.0.store(true, Ordering::SeqCst);
    let disconnected = git
        .capture(&workspace, &mut scratch, &stop, Some(&execution))
        .await;
    assert!(!disconnected.listed_complete);
    assert!(!disconnected.omissions.is_empty());
    assert_eq!(backend.resolved.load(Ordering::SeqCst), resolved);
    drop(scratch);
    git.tasks.close();
    git.tasks.wait().await;
    files.inner.close().await;
    assert!(process_owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
