use super::*;
use rsi_agent_composition_protocol::{
    DomainBaseline, DomainCatalog, DomainDefinition, DomainReferencePolicy,
};
use rsi_agent_session_protocol::{
    ApprovalPromptPolicy, DomainIdentity, SessionPermission, ToolRejection,
};

#[derive(Debug, Default)]
struct Allow(AtomicUsize);

#[derive(Debug, Default)]
struct CountCalls(AtomicUsize);

#[async_trait]
impl ToolExecutor for CountCalls {
    async fn execute(&self, arguments: Value, _: ToolExecution) -> ToolResultType<ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolResult::new(arguments, vec![], false)
    }
}

#[async_trait]
impl Approval for Allow {
    async fn ask(
        &self,
        _: ApprovalRequest,
        _: CancellationToken,
    ) -> rsi_approval_protocol::Result<ApprovalOutcome> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ApprovalOutcome {
            decision: ApprovalDecision::AllowOnce,
            answerer: "fixture".into(),
            reason: None,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_access_denies_only_required_approvals_in_its_session_after_restart() {
    let approval = Arc::new(Allow::default());
    let mut stack = BaseStack::activate_with_answerer(approval.clone()).await;
    let definition = DomainDefinition::new(
        DomainIdentity::new(SessionPermission::DOMAIN, SessionPermission::VERSION).unwrap(),
        &None::<SessionPermission>,
        |_| Ok(()),
        DomainReferencePolicy::None,
    )
    .unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let selected = |preset: &str, sandbox, approval| {
        let mut selected = DomainBaseline::new(domains.clone()).unwrap();
        selected
            .apply(
                &domains
                    .bind(&definition)
                    .unwrap()
                    .propose(
                        Default::default(),
                        &Some(SessionPermission {
                            preset: preset.into(),
                            sandbox,
                            approval,
                        }),
                    )
                    .unwrap(),
            )
            .unwrap();
        selected
    };
    let full = selected(
        "danger-full-access",
        SandboxMode::DangerFullAccess,
        ApprovalPromptPolicy::Never,
    );
    let restricted = selected(
        "read-only",
        SandboxMode::ReadOnly,
        ApprovalPromptPolicy::Ask,
    );
    *stack.composition.domains.lock().unwrap() = domains;
    let bodies = Arc::new(CountCalls::default());
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            run_policy: rsi_tools_protocol::ToolRunPolicy::execution(
                std::time::Duration::from_secs(2),
            ),
            executor: bodies.clone(),
        })
        .unwrap();
    let scripted = Arc::new(LanguageFixture {
        outcomes: Mutex::new(
            [
                "first",
                "other",
                "reopened",
                "safe",
                "restricted",
                "child",
                "global",
            ]
            .into_iter()
            .flat_map(|call_id| {
                [
                    StartOutcome::Stream(tool_calls_script(&[(call_id, "echo", r#"{"value":1}"#)])),
                    StartOutcome::Stream(answer_script()),
                ]
            })
            .collect(),
        ),
        requests: Mutex::default(),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let mut language = stack
        .activate_language("test.language", scripted.clone())
        .await;
    let mut executor = stack.activate_executor("permission").await;
    for (index, (id, required, expected_asks, expected_bodies)) in [
        ("selected", true, 0, 0),
        ("other", true, 1, 1),
        ("selected", true, 1, 1),
        ("safe", false, 1, 2),
        ("restricted", true, 2, 3),
        ("child", true, 2, 3),
        ("global", true, 2, 3),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 2 {
            assert!(executor.dispose().await.is_clean());
            assert!(language.dispose().await.is_clean());
            stack.restart_kernel().await;
            language = stack
                .activate_language("test.language", scripted.clone())
                .await;
            executor = stack.activate_executor("permission-restarted").await;
        }
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let header = header_for_session(id)
            .with_execution_policy(SandboxMode::WorkspaceWrite, required)
            .unwrap();
        let header = if id == "global" {
            let mut wire = serde_json::to_value(header).unwrap();
            wire["settings"]["sandbox"] =
                json!(SessionPermission::standard("danger-full-access").unwrap());
            serde_json::from_value(wire).unwrap()
        } else {
            header
        };
        let session = if index == 2 {
            SubmitSession::Resume(turns.prepare_resume(header.session_id()).await.unwrap())
        } else if id == "child" {
            use rsi_agent_turn_protocol::{CreatePluginChild, PluginChildSource};
            let parent = header_for_session("restricted")
                .with_execution_policy(SandboxMode::WorkspaceWrite, true)
                .unwrap();
            let child = turns
                .create_plugin_child(CreatePluginChild {
                    source: PluginChildSource::Session(Box::new(parent)),
                    directory: None,
                    preset: None,
                    session_id: header.session_id().clone(),
                    task_name: "reader".into(),
                    plugin: "example.reader".into(),
                })
                .await
                .unwrap();
            assert_eq!(child.settings().sandbox(), SandboxMode::ReadOnly);
            let pin = stack
                .composition
                .pin(child.agent_preset_id(), None)
                .await
                .unwrap();
            let prepared = PreparedFreshSession::new(child.clone(), pin.clone()).unwrap();
            let refused = turns
                .submit(SubmitTurn {
                    session: SubmitSession::Fresh(prepared),
                    turn_id: client_turn_id(),
                    text: "echo".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: Some(SandboxMode::WorkspaceWrite),
                })
                .await;
            assert!(matches!(
                refused,
                Err(rsi_agent_turn_protocol::TurnError::Invalid(_))
            ));
            SubmitSession::Fresh(PreparedFreshSession::new(child, pin).unwrap())
        } else {
            let pin = stack
                .composition
                .pin(header.agent_preset_id(), None)
                .await
                .unwrap();
            let fresh = PreparedFreshSession::new(header, pin).unwrap();
            SubmitSession::Fresh(match id {
                "other" | "global" => fresh,
                "restricted" => fresh.with_baseline(restricted.clone()).unwrap(),
                _ => fresh.with_baseline(full.clone()).unwrap(),
            })
        };
        let turn = turns
            .submit(SubmitTurn {
                session,
                turn_id: client_turn_id(),
                text: "echo".into(),
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_for_outcome(&turns, &turn).await,
            TurnOutcome::Completed
        );
        assert_eq!(approval.0.load(Ordering::SeqCst), expected_asks);
        assert_eq!(bodies.0.load(Ordering::SeqCst), expected_bodies);
        let facts = stack
            .store
            .read_facts(&turn.session_id, 0, 64, FACT_PAGE_BYTES)
            .await
            .unwrap()
            .facts;
        let expected_mode = match id {
            "other" => SandboxMode::WorkspaceWrite,
            "restricted" | "child" => SandboxMode::ReadOnly,
            _ => SandboxMode::DangerFullAccess,
        };
        assert!(facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::TurnAccepted {
            turn_id, sandbox, require_approval, ..
        } if turn_id == &turn.turn_id && *sandbox == expected_mode && *require_approval == required)));
        if matches!(id, "selected" | "child" | "global") {
            assert!(facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { turn_id,
                rejection: ToolRejection::ApprovalDenied { outcome }, .. } if turn_id == &turn.turn_id && outcome.answerer == "rsi.permission.never")));
        }
    }
    drop(lease);
    stack.dispose(language, executor).await;
}
