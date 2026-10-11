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
async fn truncated_batch_executes_no_calls_and_replans_with_frozen_cap() {
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
            tool_script(),
            answer_script(),
            answer_script(),
        ],
    );
    let language = stack
        .activate_language("test.output.recovery", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-recovery").await;
    let (first, outcome) = stack
        .submit_and_wait_with_header(
            "write the complete result",
            None,
            header_with_output_budget(2),
        )
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "neither truncated sibling may execute"
    );
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.settings().max_output_tokens() == Some(9000))
    );
    let recovery = serde_json::to_string(&requests[1]).unwrap();
    assert!(recovery.contains("split large files"));
    assert!(!recovery.contains("complete-sibling"));
    assert!(!recovery.contains("unfinished"));
    let facts = stack
        .store
        .read_facts(&first.session_id, 0, 512)
        .await
        .unwrap()
        .facts;
    let marker = facts
        .iter()
        .find(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::ModelOutputRecovery { ordinal: 1, .. }
            )
        })
        .unwrap();
    let next = facts
        .iter()
        .find(|fact| {
            fact.seq() > marker.seq() && matches!(fact.body(), SessionFactBody::ModelIntent { .. })
        })
        .unwrap();
    assert!(marker.seq() < next.seq());
    let (_, outcome) = stack
        .resume_and_wait("another user task", first.session_id)
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(
        !serde_json::to_string(fixture.requests.lock().unwrap().last().unwrap())
            .unwrap()
            .contains("split large files")
    );
    drop(lease);
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn repeated_text_or_empty_truncation_exhausts_the_turn_allowance() {
    for limit in [0, 2, 16] {
        let stack = BaseStack::activate().await;
        let fixture = provider(
            &stack,
            (0..=limit)
                .map(|index| {
                    if index % 2 == 0 {
                        truncated(answer_script())
                    } else {
                        vec![LanguageEvent::Finished {
                            reason: FinishReason::MaxTokens,
                            replay: None,
                        }]
                    }
                })
                .collect(),
        );
        let language = stack
            .activate_language("test.output.exhaustion", fixture.clone())
            .await;
        let executor = stack.activate_executor("output-exhaustion").await;
        let (submitted, outcome) = stack
            .submit_and_wait_with_header(
                "finish the result",
                None,
                header_with_output_budget(limit),
            )
            .await;
        assert_eq!(
            outcome,
            TurnOutcome::BudgetExceeded {
                dimension: BudgetDimension::OutputRecoveries,
                consumed: limit,
                limit
            }
        );
        assert_eq!(fixture.starts.load(Ordering::SeqCst) as u64, limit + 1);
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 512)
            .await
            .unwrap()
            .facts;
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact.body(), SessionFactBody::ModelOutputRecovery { .. }))
                .count() as u64,
            limit
        );
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent { .. }))
        );
        stack.dispose(language, executor).await;
    }
}

#[tokio::test]
async fn selected_cap_above_model_capacity_fails_before_prepare() {
    let stack = BaseStack::activate().await;
    let fixture = provider(&stack, vec![]);
    let language = stack
        .activate_language("test.output.capacity", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-capacity").await;
    let mut wire = serde_json::to_value(header_with_output_budget(2)).unwrap();
    wire["settings"]["output_budgets"][0]["max_output_tokens"] = json!(10001);
    let (_, outcome) = stack
        .submit_and_wait_with_header("task", None, serde_json::from_value(wire).unwrap())
        .await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, .. } if code == "model.settings"));
    assert!(fixture.requests.lock().unwrap().is_empty());
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn tools_and_undispatched_retries_do_not_reset_output_recovery_allowance() {
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
    let retry = || {
        StartOutcome::Error(
            AiError::new(
                ErrorKind::RateLimited,
                ErrorPhase::Connect,
                DispatchStatus::NotDispatched,
                "retry",
            )
            .unwrap(),
        )
    };
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(truncated(answer_script())),
            retry(),
            StartOutcome::Stream(tool_calls_script(&[("first-valid", "echo", "{}")])),
            StartOutcome::Stream(truncated(answer_script())),
            retry(),
            StartOutcome::Stream(tool_calls_script(&[("second-valid", "echo", "{}")])),
            StartOutcome::Stream(truncated(answer_script())),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language = stack
        .activate_language("test.output.global-budget", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-global-budget").await;
    let (submitted, outcome) = stack
        .submit_and_wait_with_header("finish", None, header_with_output_budget(2))
        .await;
    assert_eq!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::OutputRecoveries,
            consumed: 2,
            limit: 2
        }
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 7);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 512)
        .await
        .unwrap()
        .facts;
    let ordinals: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ModelOutputRecovery { ordinal, .. } => Some(*ordinal),
            _ => None,
        })
        .collect();
    assert_eq!(ordinals, [1, 2]);
    drop(lease);
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn replacement_reconciles_output_recovery_allowance_after_a_pending_tool() {
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(NonCooperativeTool {
                entered: entered.clone(),
                release: release.clone(),
            }),
        })
        .unwrap();
    let fixture = provider(
        &stack,
        vec![
            truncated(answer_script()),
            tool_script(),
            truncated(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.output.replacement", fixture.clone())
        .await;
    let first_executor = stack.activate_executor("before-output-replacement").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: stack.fresh(header_with_output_budget(1)).await,
            text: "finish the result".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .expect("the post-recovery Tool must start before replacing the executor");
    assert!(first_executor.dispose().await.is_clean());
    let second_executor = stack.activate_executor("after-output-replacement").await;
    release.cancel();
    assert_eq!(
        wait_for_outcome(&turns, &submitted).await,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::OutputRecoveries,
            consumed: 1,
            limit: 1,
        }
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 3);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 256)
        .await
        .unwrap()
        .facts;
    let ordinals: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ModelOutputRecovery { ordinal, .. } => Some(*ordinal),
            _ => None,
        })
        .collect();
    assert_eq!(ordinals, [1]);
    drop(lease);
    stack.dispose(language, second_executor).await;
}

#[tokio::test]
async fn cancellation_of_replanned_call_ends_without_another_recovery() {
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(truncated(answer_script())),
            StartOutcome::GatedStream {
                events: truncated(answer_script()),
                waiting_after_first: entered.clone(),
                release: Arc::new(Notify::new()),
            },
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.output.cancel", fixture.clone())
        .await;
    let executor = stack.activate_executor("output-cancel").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: stack.fresh(header_with_output_budget(2)).await,
            text: "finish".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    turns
        .cancel(&submitted.session_id, &submitted.turn_id, None)
        .await
        .unwrap();
    assert_eq!(
        wait_for_outcome(&turns, &submitted).await,
        TurnOutcome::Cancelled
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 2);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 512)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ModelOutputRecovery { .. }))
            .count(),
        1
    );
    drop(turns);
    stack.dispose(language, executor).await;
}
