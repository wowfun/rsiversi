use super::*;
use rsi_agent_session_protocol::{FrozenAgentSettings, ModelSelection, TurnId};
use rsi_agent_turn_protocol::TurnClaimIssuer;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::{
    ConfinedProcess, ProcessRequest, Sandbox, SandboxMode, WorkspaceReadRequest, WorkspaceReadScope,
};
use rsi_tools_protocol::{
    ToolExecutionExtensions, ToolExecutionPolicy, ToolResultIdentity, ToolStart,
};

#[derive(Debug)]
struct UnusedSandbox;
#[async_trait]
impl Sandbox for UnusedSandbox {
    async fn workspace_read(
        &self,
        _: WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<WorkspaceReadScope> {
        unreachable!("application tool executes on its listener")
    }
    async fn confine(&self, _: ProcessRequest) -> rsi_sandbox::Result<ConfinedProcess> {
        unreachable!("application tool executes on its listener")
    }
}

#[tokio::test]
async fn independent_sessions_keep_all_started_calls_until_their_own_settlement() {
    let (_socket, endpoint) = endpoint().await;
    let issuer = TurnClaimIssuer::new();
    let model = ModelRef::new("deployment", "model").unwrap();
    let mut calls = Vec::new();
    for session in ["first", "second"] {
        let id = SessionId::new(session).unwrap();
        let header = SessionHeader::new_local(
            id.clone(),
            1,
            "/workspace",
            AgentPresetId::new("agent").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                model.clone(),
                SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let claim = issuer.issue(
            "executor".into(),
            1,
            id.clone(),
            TurnId::new("turn").unwrap(),
            Arc::new(header),
            1,
            1,
            1,
        );
        let mut view = captured_view(endpoint.clone());
        view.session = id;
        let body = Arc::new(Body {
            view: Arc::new(view),
            name: "read".into(),
            registration: "registration".into(),
        });
        for ordinal in 0..10 {
            let call = format!("{session}-{ordinal}");
            let identity = ToolResultIdentity::new("owner", &call, &call, "a".repeat(64)).unwrap();
            let caller = issuer
                .tool_caller(
                    &claim,
                    EffectId::new(&call).unwrap(),
                    identity,
                    ModelSelection {
                        model: model.clone(),
                        reasoning_effort: None,
                    },
                    None,
                )
                .unwrap();
            let (execution, _) = ToolExecution::from_start(
                call,
                ToolStart {
                    cancellation: CancellationToken::new(),
                    policy: ToolExecutionPolicy {
                        mode: SandboxMode::WorkspaceWrite,
                        cwd: "/workspace".into(),
                        workspace: "/workspace".into(),
                    },
                    sandbox: Arc::new(UnusedSandbox),
                    job_scope: None,
                    extensions: ToolExecutionExtensions::default()
                        .with(Arc::new(caller))
                        .unwrap(),
                },
            )
            .unwrap();
            let body = body.clone();
            let mut request = Box::pin(async move { body.execute(json!({}), execution).await });
            assert!(
                futures_util::poll!(&mut request).is_pending(),
                "each admitted call must reach the listener"
            );
            calls.push(request);
        }
    }
    assert_eq!(endpoint.started.lock().unwrap().len(), 20);
    calls.truncate(10);
    {
        let remaining = endpoint.started.lock().unwrap();
        assert_eq!(remaining.len(), 10);
        assert!(
            remaining
                .iter()
                .all(|caller| caller.session_id().as_str() == "first")
        );
    }
    drop(calls);
    assert!(endpoint.started.lock().unwrap().is_empty());
}
