use rsi_agent_composition_protocol::{AgentComposition, AgentCompositionPin, PreparedFreshSession};
use rsi_agent_kernel::{AgentKernel, KernelWorkers};
use rsi_agent_session_protocol::{
    FrozenAgentSettings, SessionHeader, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    ExecutorLease, SessionProjections, SubmitSession, SubmitTurn, TurnClaim, TurnExecution,
    TurnService,
};
use rsi_sandbox::{Sandbox, SandboxMode};
use rsi_tools_protocol::{
    ToolCall, ToolExecutionExtensions, ToolExecutionPolicy, ToolResult, ToolStart,
};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(super) struct Session {
    kernel: AgentKernel,
    workers: KernelWorkers,
    executor: ExecutorLease,
    claim: TurnClaim,
}
impl Session {
    pub(super) async fn start(
        source: Arc<dyn AgentComposition>,
        pin: AgentCompositionPin,
        root: &Path,
    ) -> Self {
        let kernel = AgentKernel::recover(Arc::new(rsi_agent_testkit::MemoryStore::new()), source)
            .await
            .unwrap();
        let workers = kernel.start_workers();
        let header = SessionHeader::new_local(
            SessionId::new("native-generation").unwrap(),
            1,
            root.to_str().unwrap(),
            pin.preset_id().clone(),
            FrozenAgentSettings::new(
                "default",
                "fixture",
                rsi_ai_protocol::ModelRef::new("fixture", "unused").unwrap(),
                SandboxMode::DangerFullAccess,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        kernel
            .submit(SubmitTurn {
                session: SubmitSession::Fresh(PreparedFreshSession::new(header, pin).unwrap()),
                turn_id: TurnId::new("first").unwrap(),
                text: "native replacement".into(),
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let executor = kernel.register("native-proof".into()).unwrap();
        let claim = kernel
            .claim("native-proof", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        Self {
            kernel,
            workers,
            executor,
            claim,
        }
    }
    pub(super) async fn replace(&self, old: &AgentCompositionPin, next: &AgentCompositionPin) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let gate = Arc::new(Gate::default());
        let mut old_call = tokio::spawn(invoke(
            old.clone(),
            "held-old",
            "native_confine",
            gate.clone(),
        ));
        tokio::select! {
            () = gate.entered.notified() => {}
            result = &mut old_call => panic!("native call ended before its planner gate: {result:?}"),
        }
        let prepared = self
            .kernel
            .prepare_composition(&self.claim)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            prepared.composition().selection(),
            next.selection()
        );
        let selected = prepared.commit().await.unwrap();
        assert_eq!(selected.selection(), next.selection());
        gate.release.notify_one();
        let result = old_call.await.unwrap();
        assert_eq!(result.value["implementation"], "Native fixture tool");
        let result = invoke(selected, "new", "native_echo", Arc::new(Gate::default())).await;
        assert_eq!(
            result.value["implementation"],
            "Native fixture tool revision B"
        );
        }).await.expect("native generation replacement deadline");
    }
    pub(super) async fn restore(self, expected: &AgentCompositionPin) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            self.kernel
                .finish_turn(&self.claim, &TurnOutcome::Completed)
                .await
                .unwrap();
            assert!(matches!(
                self.kernel
                    .resident_composition(self.claim.session_id())
                    .unwrap(),
                rsi_agent_turn_protocol::ResidentComposition::NotResident
            ));
            let prepared = self
                .kernel
                .prepare_resume(self.claim.session_id())
                .await
                .unwrap();
            self.kernel
                .submit(SubmitTurn {
                    session: SubmitSession::Resume(prepared),
                    turn_id: TurnId::new("second").unwrap(),
                    text: "cold resume".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: None,
                })
                .await
                .unwrap();
            let claim = self
                .kernel
                .claim("native-proof", CancellationToken::new())
                .await
                .unwrap()
                .unwrap();
            let restored = self.kernel.composition(&claim).unwrap();
            assert_eq!(restored.selection(), expected.selection());
            let result = invoke(restored, "cold", "native_echo", Arc::new(Gate::default())).await;
            assert_eq!(
                result.value["implementation"],
                "Native fixture tool revision B"
            );
            self.kernel
                .finish_turn(&claim, &TurnOutcome::Completed)
                .await
                .unwrap();
            drop(self.executor);
            self.kernel.shutdown(self.workers).await.unwrap();
        })
        .await
        .expect("native cold restore deadline");
    }
}

async fn invoke(pin: AgentCompositionPin, id: &str, name: &str, sandbox: Arc<Gate>) -> ToolResult {
    let tools = pin.tools();
    let prepared = tools
        .prepare(
            id,
            ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: serde_json::json!({}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    let result = prepared
        .start(ToolStart {
            cancellation: CancellationToken::new(),
            policy: ToolExecutionPolicy {
                mode: SandboxMode::DangerFullAccess,
                cwd: "/workspace".into(),
                workspace: "/workspace".into(),
            },
            sandbox,
            job_scope: None,
            extensions: ToolExecutionExtensions::default(),
        })
        .await
        .unwrap();
    tools.commit(&identity).unwrap();
    result
}

#[derive(Debug, Default)]
struct Gate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl Sandbox for Gate {
    async fn workspace_read(
        &self,
        request: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        Err(rsi_sandbox::SandboxError::Unsupported(request.mode))
    }
    async fn confine(
        &self,
        request: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(rsi_sandbox::ConfinedProcess {
            owner: None,
            stdio: rsi_sandbox::ProcessStdio::Pipes,
            program: request.program,
            arguments: request.arguments.into_iter().map(Into::into).collect(),
            cwd: request.cwd,
            stamp: rsi_sandbox::EnforcementStamp {
                requested: request.mode,
                backend: rsi_sandbox::SandboxBackend::Unconfined,
                workspace: request.workspace,
                filesystem: rsi_sandbox::SandboxFileSystem::Unconfined,
                scratch: rsi_sandbox::SandboxScratch::Host,
                network: rsi_sandbox::SandboxNetwork::Host,
            },
        })
    }
}
