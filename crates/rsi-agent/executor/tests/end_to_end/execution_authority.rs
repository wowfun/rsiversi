use super::*;
#[path = "../../../../../fixtures/rsi/execution/metadata.rs"]
mod tuple;

#[derive(Debug)]
struct RevokeTool(Arc<tuple::Gate>);
#[async_trait]
impl ToolExecutor for RevokeTool {
    async fn execute(&self, _: Value, _: ToolExecution) -> ToolResultType<ToolResult> {
        self.0.revoked.store(true, Ordering::SeqCst);
        ToolResult::new(json!({"settled":true}), vec![], false)
    }
}

#[tokio::test]
async fn revocation_during_tool_settles_result_and_stops_next_provider_attempt() {
    let stack = BaseStack::activate().await;
    let gate = Arc::new(tuple::Gate::default());
    let _tool = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new(
                "echo",
                "revoke during admitted work",
                json!({"type":"object"}),
            )
            .unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(RevokeTool(gate.clone())),
        })
        .unwrap();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.language.revoke", provider.clone())
        .await;
    let executor = stack.activate_executor("executor-revoke").await;
    let local = header();
    let coordinates = rsi_execution::ExecutionCoordinates::new(
        rsi_execution::ExecutionLocation::Ssh {
            target: rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        "/workspace",
    )
    .unwrap();
    let execution = tuple::lease(coordinates.location().clone(), gate, 1);
    let header = SessionHeader::new(
        local.session_id().clone(),
        1,
        coordinates,
        local.agent_preset_id().clone(),
        local.settings().clone(),
    )
    .unwrap();
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: stack.fresh(header).await.with_execution(execution).unwrap(),
            text: "call echo once".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let outcome = wait_for_outcome(&turns, &submitted).await;
    assert!(
        matches!(outcome, TurnOutcome::Interrupted { .. }),
        "{outcome:?}"
    );
    assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
            .count(),
        1
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
            .count(),
        1
    );
    stack.dispose(language, executor).await;
}
