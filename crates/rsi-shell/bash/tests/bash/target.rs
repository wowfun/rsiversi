use super::*;
use rsi_execution::{
    ExecutionLocation, ExecutionProvider, ExecutionTargetId, HostEpoch, ResolvedProgram,
};
use rsi_tools_protocol::ToolError;
#[path = "../../../../../fixtures/rsi/execution/process.rs"]
mod target_fixture;

#[derive(Debug)]
struct WrongSandbox;
#[async_trait]
impl Sandbox for WrongSandbox {
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("target Tool reached native Sandbox")
    }
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        panic!("target Tool reached native Files scope")
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Preparation, substitution and revocation share the exact frozen target and Jobs fixture.
async fn reviewed_target_plan_is_consumed_once_by_foreground_and_jobs() {
    let fixture = Fixture::activate().await;
    let files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    let backend = Arc::new(target_fixture::Backend {
        sandbox: fixture.sandbox.clone(),
        process: fixture
            .runtime
            .root()
            .lookup_local::<rsi_process::ProcessContract>()
            .unwrap(),
        files: files.clone(),
        program: ResolvedProgram {
            program: "/bin/bash".into(),
            environment: vec![("VISIBLE".into(), "target".into())],
        },
        resolved: 0.into(),
        prepared: 0.into(),
        spawned: 0.into(),
    });
    let provider = ExecutionProvider::new(
        HostEpoch::from_bytes([1; 16]),
        ExecutionLocation::Ssh {
            target: ExecutionTargetId::parse("1".repeat(32)).unwrap(),
        },
        7,
        19,
        backend.clone(),
    )
    .unwrap();
    let gate = Arc::new(target_fixture::Gate::default());
    let lease = provider.lease(gate.clone()).unwrap();
    let start = || ToolStart {
        cancellation: CancellationToken::new(),
        policy: ToolExecutionPolicy {
            mode: SandboxMode::DangerFullAccess,
            cwd: fixture.workspace.path().canonicalize().unwrap(),
            workspace: fixture.workspace.path().canonicalize().unwrap(),
        },
        sandbox: Arc::new(WrongSandbox),
        job_scope: None,
        extensions: ToolExecutionExtensions::default()
            .with(Arc::new(lease.clone()))
            .unwrap(),
    };
    for (index, jobs) in [false, true].into_iter().enumerate() {
        let id = format!("target-{index}");
        let mut prepared = fixture
            .tools
            .prepare(
                &id,
                ToolCall {
                    id: id.clone(),
                    name: "bash".into(),
                    arguments: json!({"command":"printf %s \"$VISIBLE\""}),
                },
            )
            .unwrap();
        let review = prepared.prepare_execution(start()).await.unwrap().unwrap();
        assert_eq!(review.binding(), lease.binding());
        assert!(review.plan_sequence().is_some());
        assert_eq!(backend.prepared.load(Ordering::SeqCst), index + 1);
        assert_eq!(backend.spawned.load(Ordering::SeqCst), index);
        let mut input = start();
        if jobs {
            input.job_scope = Some(fixture.scope.clone());
        }
        let result = prepared.start(input).await.unwrap();
        assert_eq!(result.value["stdout"]["text"], "target");
        assert_eq!(backend.spawned.load(Ordering::SeqCst), index + 1);
        assert_eq!(backend.resolved.load(Ordering::SeqCst), index + 1);
        assert_eq!(result.enforcement.len(), 1);
    }
    for changed in ["lease", "policy", "revoked"] {
        let mut prepared = fixture
            .tools
            .prepare(
                changed,
                ToolCall {
                    id: changed.into(),
                    name: "bash".into(),
                    arguments: json!({"command":"printf should-not-run"}),
                },
            )
            .unwrap();
        prepared.prepare_execution(start()).await.unwrap();
        let mut input = start();
        match changed {
            "lease" => {
                input.extensions = ToolExecutionExtensions::default()
                    .with(Arc::new(provider.lease(gate.clone()).unwrap()))
                    .unwrap();
            }
            "policy" => input.policy.mode = SandboxMode::ReadOnly,
            _ => gate.0.store(true, Ordering::SeqCst),
        }
        let error = prepared.start(input).await.unwrap_err();
        assert!(matches!(
            error,
            ToolError::Execution(_) | ToolError::ShuttingDown
        ));
        assert_eq!(backend.spawned.load(Ordering::SeqCst), 2);
    }
    drop((lease, provider, backend));
    files.close().await;
    fixture.shutdown().await;
}
