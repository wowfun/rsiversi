use super::*;
use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionContext, ContributionKind, ContributionRegistration,
    ContributionResult, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_sandbox::{SandboxGeneration, WorkspaceReadRequest, WorkspaceReadScope};
use rsi_tools_protocol::ToolRegistrarContract;

#[derive(Debug)]
struct ReadSecurity {
    decision: ApprovalDecision,
    reviews: Mutex<Vec<ApprovalRequest>>,
    scopes: Mutex<Vec<WorkspaceReadScope>>,
    generation: SandboxGeneration,
}

#[async_trait]
impl Approval for ReadSecurity {
    async fn ask(
        &self,
        request: ApprovalRequest,
        _: CancellationToken,
    ) -> rsi_approval_protocol::Result<ApprovalOutcome> {
        self.reviews.lock().unwrap().push(request);
        Ok(ApprovalOutcome {
            decision: self.decision,
            answerer: "files-test".into(),
            reason: None,
        })
    }
}

#[async_trait]
impl Sandbox for ReadSecurity {
    async fn workspace_read(
        &self,
        request: WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<WorkspaceReadScope> {
        let scope = WorkspaceReadScope::new(request, self.generation.clone())?;
        self.scopes.lock().unwrap().push(scope.clone());
        Ok(scope)
    }
    async fn confine(&self, _: ProcessRequest) -> rsi_sandbox::Result<ConfinedProcess> {
        panic!("workspace reads do not start processes")
    }
}

#[derive(Debug)]
struct ReadFixtureFactory(Arc<dyn ToolRegistrar>, Arc<ReadSecurity>);

#[async_trait]
impl PluginFactory for ReadFixtureFactory {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan
            .context()
            .provide_local::<ToolRegistrarContract>(self.0.clone())?;
        let approval = plan
            .context()
            .provide_local::<ApprovalContract>(self.1.clone())?;
        let sandbox = plan
            .context()
            .provide_local::<SandboxContract>(self.1.clone())?;
        plan.defer(
            "withdraw Files fixtures",
            Box::new(move || {
                Box::pin(async move {
                    drop((registrar, approval, sandbox));
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct ReadPolicy {
    mode: SandboxMode,
    decision: ToolPolicyDecision,
    calls: AtomicUsize,
}

#[async_trait]
impl ToolPolicy for ReadPolicy {
    async fn decide(
        &self,
        _: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        assert!(matches!(request.name, "file_read" | "directory_list"));
        assert_eq!(request.sandbox, self.mode);
        self.calls.fetch_add(1, Ordering::AcqRel);
        Ok(self.decision.clone())
    }
}

#[derive(Clone, Copy, Debug)]
enum Requirement {
    Header,
    Contribution,
    Deny,
}

#[tokio::test]
async fn files_obey_header_approval_in_every_mode() {
    exercise(Requirement::Header).await;
}

#[tokio::test]
async fn files_obey_frozen_contribution_approval_in_every_mode() {
    exercise(Requirement::Contribution).await;
}

#[tokio::test]
async fn files_policy_denial_precedes_approval_and_read_in_every_mode() {
    exercise(Requirement::Deny).await;
}

async fn exercise(requirement: Requirement) {
    for mode in [
        SandboxMode::ReadOnly,
        SandboxMode::WorkspaceWrite,
        SandboxMode::DangerFullAccess,
    ] {
        for decision in [ApprovalDecision::Deny, ApprovalDecision::AllowOnce] {
            for tool in ["file_read", "directory_list"] {
                scenario(requirement, mode, decision, tool).await;
            }
        }
    }
}

#[allow(clippy::too_many_lines)] // One public-seam scenario keeps its admission and durable assertions together.
async fn scenario(
    requirement: Requirement,
    mode: SandboxMode,
    decision: ApprovalDecision,
    tool: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(root.join("sample"), b"a\0\xffz").unwrap();
    let stack = BaseStack::activate().await;
    assert!(stack.security_fiber.dispose().await.is_clean());
    let security = Arc::new(ReadSecurity {
        decision,
        reviews: Mutex::default(),
        scopes: Mutex::default(),
        generation: SandboxGeneration::default(),
    });
    let fixture = activate_fixture(
        &stack.runtime,
        "test.files-security",
        "1",
        Arc::new(ReadFixtureFactory(
            stack.tool_registrar.clone(),
            security.clone(),
        )),
    )
    .await;
    let files = activate_fixture(
        &stack.runtime,
        "test.files",
        "1",
        Arc::new(rsi_files::FilesFactory),
    )
    .await;
    let tools = activate_fixture(
        &stack.runtime,
        "test.files-tools",
        "1",
        Arc::new(rsi_files_tools::FilesToolsFactory),
    )
    .await;
    let policy = Arc::new(ReadPolicy {
        mode,
        calls: AtomicUsize::new(0),
        decision: match requirement {
            Requirement::Header => ToolPolicyDecision::Abstain,
            Requirement::Contribution => ToolPolicyDecision::RequireApproval,
            Requirement::Deny => ToolPolicyDecision::Deny {
                reason: "fixture denies workspace reads".into(),
            },
        },
    });
    let callbacks = contributions::install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.files-policy").unwrap(),
            0,
            ContributionKind::ToolPolicy(policy.clone()),
        )],
    )
    .await;
    let args = if tool == "file_read" {
        r#"{"path":"sample"}"#
    } else {
        "{}"
    };
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("read-1", tool, args)])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::default(),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack.activate_language("test.language", language).await;
    // The fresh Session captures its catalog before a replacement removes the policy.
    let header = SessionHeader::new(
        SessionId::new("files-session").unwrap(),
        1,
        root.to_str().unwrap(),
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            mode,
            matches!(requirement, Requirement::Header) || mode == SandboxMode::DangerFullAccess,
        )
        .unwrap(),
    )
    .unwrap();
    let session = stack.fresh(header).await;
    *stack.composition.contributions.lock().unwrap() = ContributionCatalog::default();
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session,
            text: "read workspace".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let executor = stack.activate_executor("files-executor").await;
    let outcome = wait_for_outcome(&turns, &submitted).await;
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert_eq!(policy.calls.load(Ordering::Acquire), 1);
    let policy_denied = matches!(requirement, Requirement::Deny);
    assert_eq!(
        security.reviews.lock().unwrap().len(),
        usize::from(!policy_denied)
    );
    if !policy_denied && decision == ApprovalDecision::AllowOnce {
        assert_eq!(outcome, TurnOutcome::Completed);
        let scopes = security.scopes.lock().unwrap();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].generation(), &security.generation);
        assert_eq!(scopes[0].mode(), mode);
        assert_eq!(scopes[0].cwd(), root);
        assert_eq!(scopes[0].workspace(), root);
        let result = facts
            .iter()
            .find_map(|fact| match fact.body() {
                SessionFactBody::ToolResult { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        assert!(!result.is_error);
        assert!(result.enforcement.is_empty());
        if tool == "file_read" {
            assert_eq!(result.value["bytes_hex"], "6100ff7a");
        } else {
            assert_eq!(result.value["entries"][0]["name"], "sample");
        }
        assert!(facts.iter().any(|fact| matches!(fact.body(),
            SessionFactBody::ToolIntent { approval: Some(approval), .. }
                if approval.decision == ApprovalDecision::AllowOnce)));
    } else {
        let expected = if policy_denied {
            "policy.denied"
        } else {
            "approval.denied"
        };
        assert!(matches!(outcome, TurnOutcome::Failed { ref code, .. } if code == expected));
        assert!(security.scopes.lock().unwrap().is_empty());
        assert!(
            facts
                .iter()
                .any(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
        );
        assert!(!facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolIntent { .. }
                | SessionFactBody::ToolStarted { .. }
                | SessionFactBody::ToolResult { .. }
        )));
    }
    assert!(executor.dispose().await.is_clean());
    assert!(language_fiber.dispose().await.is_clean());
    assert!(tools.dispose().await.is_clean());
    assert!(files.dispose().await.is_clean());
    assert!(fixture.dispose().await.is_clean());
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose_services().await;
}
