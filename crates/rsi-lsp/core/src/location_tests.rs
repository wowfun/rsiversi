//! Exact provider routing with a deterministic stdio peer; no live SSH claim.
use super::tests::{Fixture, TestSandbox};
use super::*;
use async_trait::async_trait;
use rsi_execution::*;
use rsi_files_protocol::Files;
use rsi_process::*;
use rsi_sandbox::*;
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Target {
    duplex: Arc<dyn DuplexProcess>,
    files: Arc<dyn Files>,
    expected: TargetProgram,
    resolves: AtomicUsize,
    starts: AtomicUsize,
}
#[async_trait]
impl ExecutionBackend for Target {
    async fn canonicalize(&self, _: &str) -> rsi_process::Result<String> {
        panic!("caller already fixed coordinates")
    }
    async fn resolve_program(&self, _: &str) -> rsi_process::Result<ResolvedProgram> {
        panic!("explicit LSP configuration required")
    }
    async fn resolve_target_program(
        &self,
        program: TargetProgram,
    ) -> rsi_process::Result<ResolvedProgram> {
        assert_eq!(program, self.expected);
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Ok(ResolvedProgram {
            program: "/usr/bin/python3".into(),
            environment: program
                .environment
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        })
    }
    async fn prepare(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
    ) -> rsi_process::Result<BackendPlan> {
        assert_eq!(request.program, std::path::Path::new("/usr/bin/python3"));
        let plan = TestSandbox.confine(request).await.unwrap();
        Ok(BackendPlan::new(plan.clone(), plan.stamp, environment))
    }
    async fn spawn(
        &self,
        _: ProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedProcess> {
        panic!("LSP must use duplex")
    }
    async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> rsi_process::Result<ManagedDuplexProcess> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.duplex
            .spawn(spec.try_map_process(|p| Ok(pin.retain_native(p.take::<ConfinedProcess>()?)))?)
            .await
    }
    async fn spawn_pty(
        &self,
        _: PtyProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedPtyProcess> {
        panic!("LSP must use duplex")
    }
    async fn workspace_read(
        &self,
        request: WorkspaceReadRequest,
    ) -> rsi_process::Result<WorkspaceReadScope> {
        Ok(TestSandbox.workspace_read(request).await.unwrap())
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
#[derive(Debug)]
struct NoNativeSpawn;
#[async_trait]
impl DuplexProcess for NoNativeSpawn {
    async fn spawn(&self, _: DuplexProcessSpec) -> rsi_process::Result<ManagedDuplexProcess> {
        panic!("remote LSP fell back to native provider")
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "one exact lease reuse, replacement and revocation scenario"
)]
async fn equal_paths_keep_exact_lease_pools_and_revocation_blocks_queries_and_source_reads() {
    let fixture = Fixture::new("normal").await;
    let mut config = fixture.config.clone();
    let policy = TargetProgram {
        command: "python3".into(),
        environment: config.environment.clone().into_iter().collect(),
    };
    config.remote_program = Some(policy.clone());
    // These deliberately unusable Local values must never enter remote source/process paths.
    config.program = "/unavailable/service/python".into();
    config.environment = [("HOME".into(), "/service-only".into())].into();
    let backend = Arc::new(Target {
        duplex: fixture
            .runtime
            .root()
            .lookup_local::<DuplexProcessContract>()
            .unwrap(),
        files: fixture.files.clone(),
        expected: policy,
        resolves: AtomicUsize::new(0),
        starts: AtomicUsize::new(0),
    });
    let location = ExecutionLocation::Ssh {
        target: ExecutionTargetId::parse("b".repeat(32)).unwrap(),
    };
    let coordinates =
        ExecutionCoordinates::new(location.clone(), fixture.workspace.to_str().unwrap()).unwrap();
    assert!(LanguageWorkspace::new(coordinates.clone(), None).is_err());
    let gate = Arc::new(Gate::default());
    let first = ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        location.clone(),
        1,
        1,
        backend.clone(),
    )
    .unwrap()
    .lease(gate.clone())
    .unwrap();
    let other = ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        location,
        1,
        2,
        backend.clone(),
    )
    .unwrap()
    .lease(Arc::new(Gate::default()))
    .unwrap();
    let authority = LanguageWorkspace::new(coordinates.clone(), Some(first)).unwrap();
    let replacement = LanguageWorkspace::new(coordinates, Some(other)).unwrap();
    let native_files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    native_files.close().await;
    let owner = LanguageService::new(
        config,
        Arc::new(NoNativeSpawn),
        Arc::new(TestSandbox),
        native_files,
        fixture.runtime.execution().clone(),
    )
    .unwrap();
    for source in [&authority, &authority, &replacement] {
        owner
            .query(
                source.clone(),
                Fixture::query(Operation::Hover),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
    assert_eq!(backend.starts.load(Ordering::SeqCst), 2);
    assert_eq!(backend.resolves.load(Ordering::SeqCst), 2);
    assert!(
        owner
            .current_file(
                authority.clone(),
                "main.rs".into(),
                CancellationToken::new()
            )
            .await
            .unwrap()
            .contains("target")
    );
    gate.0.store(true, Ordering::SeqCst);
    assert_eq!(
        owner
            .query(
                authority.clone(),
                Fixture::query(Operation::Hover),
                CancellationToken::new()
            )
            .await,
        Err(Error::Unavailable)
    );
    assert_eq!(
        owner
            .current_file(authority, "main.rs".into(), CancellationToken::new())
            .await,
        Err(Error::Unavailable)
    );
    assert_eq!(backend.starts.load(Ordering::SeqCst), 2);
    owner.close().await.unwrap();
    fixture.close().await;
}
