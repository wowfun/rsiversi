use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_persists_intent_and_start_before_model_and_tool_io() {
    let stack = BaseStack::activate().await;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: tool_calls.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-1").await;
    let (submitted, outcome) = stack
        .submit_and_wait_with_sandbox("call echo", Some(SandboxMode::DangerFullAccess))
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    assert_eq!(tool_calls.load(Ordering::Acquire), 1);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let kinds = page
        .facts
        .iter()
        .map(|fact| fact_kind(fact.body()))
        .collect::<Vec<_>>();
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent {
            approval: Some(approval),
            ..
        } if approval.decision == ApprovalDecision::AllowOnce
    )));
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolResult { result, .. } if result.enforcement.len() == 1
    )));
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["model_intent", "model_started"])
    );
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["tool_intent", "tool_started"])
    );
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["tool_started", "tool_result"])
    );
    assert_eq!(kinds.last(), Some(&"terminal"));
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| { message.role() == rsi_ai_protocol::MessageRole::Tool })
        );
    }
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adjacent_parallel_safe_tools_overlap_but_publish_results_in_source_order() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let rendezvous = Arc::new(Barrier::new(2));
    let release_first = Arc::new(Notify::new());
    let observed_lane_parking_authority = Arc::new(AtomicBool::new(false));
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new(
                "parallel",
                "parallel fixture",
                json!({"type":"object"}),
            )
            .unwrap()
            .with_scheduling(ToolScheduling::ParallelSafe),
            timeout_ms: 1_000,
            executor: Arc::new(ParallelTool {
                rendezvous,
                release_first,
                observed_lane_parking_authority: Arc::clone(&observed_lane_parking_authority),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[
                ("parallel-call-1", "parallel", r#"{"position":"first"}"#),
                ("parallel-call-2", "parallel", r#"{"position":"second"}"#),
            ])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parallel-tools", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-parallel-tools").await;

    let (submitted, outcome) = stack.submit_and_wait("run both tools").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let intents = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent { parallel_safe, .. } => Some(*parallel_safe),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(intents, vec![true, true]);
    let effect_markers = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent { effect_id, .. } => Some(("intent", effect_id.as_str())),
            SessionFactBody::ToolStarted { effect_id, .. } => Some(("started", effect_id.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        effect_markers
            .iter()
            .map(|(marker, _)| *marker)
            .collect::<Vec<_>>(),
        vec!["intent", "intent", "started", "started"]
    );
    assert_eq!(effect_markers[0].1, effect_markers[2].1);
    assert_eq!(effect_markers[1].1, effect_markers[3].1);
    let results = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolResult { result, .. } => {
                result.value.get("position").and_then(Value::as_str)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results, vec!["first", "second"]);
    assert!(!observed_lane_parking_authority.load(Ordering::Acquire));

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_batch_publishes_successful_siblings_before_propagating_a_failure() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let failed_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("fail", "fail", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout_ms: 1_000,
            executor: Arc::new(FailingTool {
                store: stack.store.clone(),
            }),
        })
        .unwrap();
    let successful_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[
                ("failed-call", "fail", r#"{"position":"failed"}"#),
                ("successful-call", "echo", r#"{"position":"successful"}"#),
            ],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parallel-partial-failure", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor("executor-parallel-partial-failure")
        .await;

    let (submitted, outcome) = stack.submit_and_wait("run both tools").await;
    assert!(matches!(outcome, TurnOutcome::Failed { .. }));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let successful_result = facts.iter().position(|fact| {
        matches!(
            fact.body(),
            SessionFactBody::ToolResult { result, .. }
                if result.value.get("position") == Some(&Value::String("successful".into()))
        )
    });
    let terminal = facts
        .iter()
        .position(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { .. }));
    assert!(
        successful_result
            .zip(terminal)
            .is_some_and(|(result, terminal)| result < terminal)
    );

    drop((failed_lease, successful_lease));
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_publication_failure_does_not_drop_a_later_settled_sibling() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("uneven", "uneven", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout_ms: 1_000,
            executor: Arc::new(UnevenParallelResultTool),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[
                ("large-call", "uneven", r#"{"position":"first"}"#),
                ("small-call", "uneven", r#"{"position":"second"}"#),
            ],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parallel-publication-failure", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor("executor-parallel-publication-failure")
        .await;
    let budget = TurnBudget::new(1_800_000, 64, 256, 65_536, 16 * 1024).unwrap();

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("run uneven tools", None, header_with_budget(budget))
        .await;

    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::GeneratedFactBytes,
            ..
        }
    ));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert!(facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolResult { result, .. }
            if result.value.get("position") == Some(&Value::String("second".into()))
    )));

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_tool_is_a_durable_barrier_between_parallel_safe_runs() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let calls = Arc::new(AtomicUsize::new(0));
    let parallel_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("read", "read", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let exclusive_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("write", "write", json!({"type":"object"})).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[
                ("read-call", "read", r#"{"position":"read"}"#),
                ("write-call", "write", r#"{"position":"write"}"#),
                ("read-call-2", "read", r#"{"position":"read-2"}"#),
            ])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-barrier", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-barrier").await;

    let (submitted, outcome) = stack.submit_and_wait("read write read").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::Acquire), 3);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let relevant = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent { name, .. } => Some(format!("intent:{name}")),
            SessionFactBody::ToolResult { result, .. } => result
                .value
                .get("position")
                .and_then(Value::as_str)
                .map(|position| format!("result:{position}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relevant,
        vec![
            "intent:read",
            "result:read",
            "intent:write",
            "result:write",
            "intent:read",
            "result:read-2",
        ]
    );

    drop((parallel_lease, exclusive_lease));
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_final_tool_is_rejected_before_effects_when_not_last_in_source_order() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let calls = Arc::new(AtomicUsize::new(0));
    let wait_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("wait_agent", "wait", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ExclusiveFinal),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let echo_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            timeout_ms: 1_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[
                ("wait-call", "wait_agent", r"{}"),
                ("echo-call", "echo", r"{}"),
            ],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.wait-order", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-wait-order").await;

    let (submitted, outcome) = stack.submit_and_wait("invalid wait ordering").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "tool.exclusive_final_not_last"
    ));
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    assert!(
        !page
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent { .. }))
    );

    drop((wait_lease, echo_lease));
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tool_is_rejected_before_any_effect_is_published() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[("missing-call", "missing_tool", r"{}")],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.missing-tool", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-missing-tool").await;

    let (submitted, outcome) = stack.submit_and_wait("call a missing tool").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "tool.not_found"
    ));
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    assert!(
        !page
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent { .. }))
    );

    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_attempt_budget_stops_a_model_tool_loop_with_durable_evidence() {
    let stack = BaseStack::activate().await;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&tool_calls),
            }),
        })
        .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::clone(&starts),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.budget", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-budget").await;
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();
    let (submitted, outcome) = stack
        .submit_and_wait_with_header("keep calling tools", None, header_with_budget(budget))
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::ProviderAttempts,
            consumed: 2,
            limit: 1,
        }
    );
    assert_eq!(starts.load(Ordering::Acquire), 1);
    assert_eq!(tool_calls.load(Ordering::Acquire), 1);
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let kinds = page
        .facts
        .iter()
        .map(|fact| fact_kind(fact.body()))
        .collect::<Vec<_>>();
    assert!(kinds.ends_with(&["budget_exhausted", "terminal"]));

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalizer_failure_wins_before_any_budget_marker_is_published() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.finalizer-budget", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let finalizer_lease = finalization
        .register("failing-test-finalizer".into(), Arc::new(FailingFinalizer))
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-finalizer-budget").await;
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("fail finalization", None, header_with_budget(budget))
        .await;

    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "test.finalization"
    ));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::BudgetExhausted { .. }))
    );

    drop(finalizer_lease);
    drop(finalization);
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_blocker_replaces_only_an_otherwise_successful_outcome() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.completion-blocker", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let finalizer_lease = finalization
        .register(
            "completion-blocker".into(),
            Arc::new(CompletionBlockerFinalizer),
        )
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-completion-blocker").await;

    let (_, outcome) = stack.submit_and_wait("finish successfully").await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            code: "jobs.unreported".into(),
            message: "background output was not collected".into(),
        }
    );

    drop(finalizer_lease);
    drop(finalization);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_budget_failure_retires_the_retained_identity_after_terminal_durability() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-result-budget", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-result-budget").await;
    let budget = TurnBudget::new(1_800_000, 64, 256, 8, 67_108_864).unwrap();

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("budget the result", None, header_with_budget(budget))
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::GeneratedFacts,
            consumed: 9,
            limit: 8,
        }
    );
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("durable ToolStarted identity");
    assert_eq!(
        stack.tool_runtime().query(&identity).unwrap(),
        RetainedToolResult::Absent
    );

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

fn fact_kind(body: &SessionFactBody) -> &'static str {
    match body {
        SessionFactBody::TurnAccepted { .. } => "accepted",
        SessionFactBody::MessageTurnAccepted { .. } => "message_accepted",
        SessionFactBody::StepStarted { .. } => "step_started",
        SessionFactBody::InputMessageEntered { .. } => "input_message_entered",
        SessionFactBody::StepEnded { .. } => "step_ended",
        SessionFactBody::WorkspaceTouched { .. } => "workspace_touched",
        SessionFactBody::ImageRequested { .. } => "image_requested",
        SessionFactBody::ModelIntent { .. } => "model_intent",
        SessionFactBody::ModelStarted { .. } => "model_started",
        SessionFactBody::ModelEvent { .. } => "model_event",
        SessionFactBody::ImageIntent { .. } => "image_intent",
        SessionFactBody::ImageStarted { .. } => "image_started",
        SessionFactBody::ImageOutput { .. } => "image_output",
        SessionFactBody::ToolIntent { .. } => "tool_intent",
        SessionFactBody::ToolStarted { .. } => "tool_started",
        SessionFactBody::ToolResult { .. } => "tool_result",
        SessionFactBody::TurnTerminal { .. } => "terminal",
        SessionFactBody::CancelRequested { .. } => "cancel",
        SessionFactBody::BudgetExhausted { .. } => "budget_exhausted",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_tool_result_is_retired_after_the_terminal_fact_is_durable() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "fail", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(FailingTool {
                store: stack.store.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-failure", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-failure").await;
    let (submitted, outcome) = stack.submit_and_wait("fail the tool").await;
    assert!(matches!(outcome, TurnOutcome::Failed { .. }));

    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("durable ToolStarted identity");
    assert_eq!(
        stack.tool_runtime().query(&identity).unwrap(),
        RetainedToolResult::Absent,
        "terminal durability must release the process-local retained slot"
    );

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}
