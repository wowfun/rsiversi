use super::*;

fn truncated(mut script: Vec<LanguageEvent>) -> Vec<LanguageEvent> {
    *script.last_mut().unwrap() = LanguageEvent::Finished {
        reason: FinishReason::MaxTokens,
        replay: None,
    };
    script
}

fn provider(stack: &BaseStack, scripts: Vec<Vec<LanguageEvent>>) -> Arc<LanguageFixture> {
    Arc::new(LanguageFixture {
        outcomes: Mutex::new(scripts.into_iter().map(StartOutcome::Stream).collect()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    })
}

#[tokio::test]
async fn truncated_tool_batch_is_diagnostic_only_and_stops_without_replanning() {
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let fixture = provider(
        &stack,
        vec![
            truncated(tool_calls_script(&[
                ("complete-sibling", "echo", "{}"),
                ("partial", "echo", "{\"unfinished\":"),
            ])),
            answer_script(),
        ],
    );
    let language = stack
        .activate_language("test.output.truncated", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-truncated").await;
    let (first, outcome) = stack
        .submit_and_wait_with_header("write the result", None, header_with_output_cap())
        .await;
    assert_eq!(outcome, TurnOutcome::MaxTokens);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
    let facts = stack
        .store
        .read_facts(
            &first.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent { .. }))
    );
    let settlements: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ModelAttemptSettled { attempt: log, .. } => Some(log),
            _ => None,
        })
        .collect();
    assert_eq!(settlements.len(), 1);
    assert!(settlements[0].events().any(|event| matches!(event.unwrap().1,
        LanguageEvent::ContentDelta { delta: ContentDelta::ToolArguments(ref text), .. } if text.contains("unfinished"))));
    let (_, outcome) = stack
        .resume_and_wait("another user task", first.session_id)
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    {
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|request| request.settings().max_output_tokens() == Some(9000))
        );
        let next = serde_json::to_string(&requests[1]).unwrap();
        assert!(!next.contains("complete-sibling"));
        assert!(!next.contains("unfinished"));
    }
    drop(lease);
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn max_tokens_retains_safe_partial_text_for_the_next_user_turn() {
    for script in [
        truncated(answer_script()),
        vec![LanguageEvent::Finished {
            reason: FinishReason::MaxTokens,
            replay: None,
        }],
    ] {
        let has_text = script.len() > 1;
        let stack = BaseStack::activate().await;
        let fixture = provider(&stack, vec![script, answer_script()]);
        let language = stack
            .activate_language("test.output.partial", fixture.clone())
            .await;
        let executor = stack.activate_executor("output-partial").await;
        let (first, outcome) = stack.submit_and_wait("finish the result").await;
        assert_eq!(outcome, TurnOutcome::MaxTokens);
        assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
        let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
        assert_eq!(outcome, TurnOutcome::Completed);
        let next = serde_json::to_string(&fixture.requests.lock().unwrap()[1]).unwrap();
        if has_text {
            assert!(next.contains("done"));
            assert!(next.contains("Interrupted model attempt"));
        }
        stack.dispose(language, executor).await;
    }
}

#[tokio::test]
async fn controlled_cancellation_settles_started_attempt_before_step_and_turn_terminal() {
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([gated_answer(
            &entered,
            &Arc::new(Notify::new()),
        )])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.output.cancel", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-cancel").await;
    let (result, ()) = tokio::join!(stack.submit_and_wait("finish"), async {
        entered.notified().await;
        stack.cancel_current().await;
    });
    assert_eq!(result.1, TurnOutcome::Cancelled);
    let facts = stack
        .store
        .read_facts(
            &result.0.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let settlements: Vec<_> = facts
        .iter()
        .filter(|fact| matches!(fact.body(), SessionFactBody::ModelAttemptSettled { .. }))
        .collect();
    assert_eq!(settlements.len(), 1);
    for fact in &facts {
        if matches!(
            fact.body(),
            SessionFactBody::StepEnded { .. } | SessionFactBody::TurnTerminal { .. }
        ) {
            assert!(settlements[0].seq() < fact.seq());
        }
    }
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn more_than_65536_language_events_use_one_durable_settlement() {
    let stack = BaseStack::activate().await;
    let mut script = vec![LanguageEvent::ContentStarted {
        index: 0,
        content: ContentStart::Text,
    }];
    script.extend((0..70_000).map(|_| LanguageEvent::ContentDelta {
        index: 0,
        delta: ContentDelta::Text("界".into()),
    }));
    script.push(LanguageEvent::ContentFinished { index: 0 });
    script.push(LanguageEvent::Finished {
        reason: FinishReason::Stop,
        replay: None,
    });
    let fixture = provider(&stack, vec![script]);
    let language = stack
        .activate_language("test.output.fragments", fixture)
        .await;
    let executor = stack.activate_executor("output-fragments").await;
    let (submitted, outcome) = stack.submit_and_wait("large stream").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let logs: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ModelAttemptSettled { attempt: log, .. } => Some(log),
            _ => None,
        })
        .collect();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].events().count(), 70_003);
    assert!(facts.len() < 16);
    stack.dispose(language, executor).await;
}
#[tokio::test]
async fn selected_cap_above_model_capacity_fails_before_prepare() {
    let stack = BaseStack::activate().await;
    let fixture = provider(&stack, vec![]);
    let language = stack
        .activate_language("test.output.capacity", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-capacity").await;
    let mut wire = serde_json::to_value(header_with_output_cap()).unwrap();
    wire["settings"]["output_budgets"][0]["max_output_tokens"] = json!(10001);
    let (_, outcome) = stack
        .submit_and_wait_with_header("task", None, serde_json::from_value(wire).unwrap())
        .await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, .. } if code == "model.settings"));
    assert!(fixture.requests.lock().unwrap().is_empty());
    stack.dispose(language, executor).await;
}
