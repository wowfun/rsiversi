use super::*;

#[tokio::test]
async fn elapsed_budget_bounds_a_provider_prepare_that_never_returns() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(PendingLanguage {
        entered: Arc::new(Notify::new()),
    });
    let language_fiber = stack
        .runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "test.language.pending-prepare",
                "language",
                UpdateMode::Replayable,
                Arc::new(PendingLanguageFactory { fixture }),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-elapsed-budget").await;
    let budget = TurnBudget::new(20, 64, 256, 65_536, 67_108_864).unwrap();

    let (_, outcome) = stack
        .submit_and_wait_with_header("never prepare", None, header_with_budget(budget))
        .await;

    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            consumed,
            limit: 20,
        } if consumed >= 20
    ));
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Keep admission, terminal eviction, retained settlement, and final generation release in one public scenario.
async fn elapsed_budget_retires_an_admitted_tool_after_it_settles() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
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
        .activate_language("test.language.elapsed-tool", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-elapsed-tool").await;
    let budget = TurnBudget::new(50, 64, 256, 65_536, 67_108_864).unwrap();
    let fresh = stack.fresh(header_with_budget(budget)).await;
    let tool_runtime = stack.tool_runtime();

    let turn = tokio::spawn({
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        async move {
            let submitted = turns
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: fresh,
                    text: "delay the tool".into(),
                    model: None,
                    sandbox: None,
                })
                .await
                .unwrap();
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break (submitted, outcome);
                }
                tokio::task::yield_now().await;
            }
        }
    });
    entered.notified().await;
    drop(stack.composition.pin.lock().unwrap().take());
    let (submitted, outcome) = tokio::time::timeout(std::time::Duration::from_secs(2), turn)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            limit: 50,
            ..
        }
    ));
    assert_eq!(
        stack.composition.owner_drops.load(Ordering::Acquire),
        0,
        "a retained Tool must keep its exact generation alive after the resident session is terminal"
    );
    let identity = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .find_map(|fact| match fact.into_body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity),
            _ => None,
        })
        .expect("durable ToolStarted identity");

    release.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if tool_runtime.query(&identity).unwrap() == RetainedToolResult::Absent {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("elapsed terminal must retire the later-settled retained Tool identity");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the retained Tool's final pin must release after settlement");

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Keep executor replacement, durable recovery, terminal eviction, and generation ownership in one scenario.
async fn recovered_pending_tool_keeps_its_generation_pin_through_elapsed_retirement() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
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
        .activate_language("test.language.recovered-tool", fixture)
        .await;
    let first_executor = stack.activate_executor("executor-before-recovery").await;
    let budget = TurnBudget::new(2_000, 64, 256, 65_536, 67_108_864).unwrap();
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header_with_budget(budget)).await,
            text: "recover the pending tool".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .expect("the Tool did not start before its elapsed recovery fixture deadline");
    assert!(first_executor.dispose().await.is_clean());

    let second_executor = stack.activate_executor("executor-after-recovery").await;
    let tool_runtime = stack.tool_runtime();
    drop(stack.composition.pin.lock().unwrap().take());
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the inherited elapsed deadline did not terminate recovery");
    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            limit: 2_000,
            ..
        }
    ));
    assert_eq!(
        stack.composition.owner_drops.load(Ordering::Acquire),
        0,
        "terminal eviction released the recovered Tool's generation while it was pending"
    );

    let identity = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .find_map(|fact| match fact.into_body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity),
            _ => None,
        })
        .expect("durable recovered ToolStarted identity");
    release.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while tool_runtime.query(&identity).unwrap() != RetainedToolResult::Absent {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovered Tool identity was not retired after settlement");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovered Tool's final generation pin was not released after settlement");

    drop((tool_runtime, turns, tool_lease, tools));
    stack.dispose(language_fiber, second_executor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successfully_recovered_tool_releases_its_tracking_pin_after_commit() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
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
        .activate_language("test.language.successfully-recovered-tool", fixture)
        .await;
    let first_executor = stack
        .activate_executor("executor-before-successful-recovery")
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "recover and finish the pending tool".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    entered.notified().await;
    assert!(first_executor.dispose().await.is_clean());

    let second_executor = stack
        .activate_executor("executor-after-successful-recovery")
        .await;
    drop(stack.composition.pin.lock().unwrap().take());
    release.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successfully recovered Tool did not complete the turn");
    assert_eq!(outcome, TurnOutcome::Completed);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a committed recovered Tool left its tracking generation pin resident");

    drop((turns, tool_lease, tools));
    stack.dispose(language_fiber, second_executor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_tool_retirement_does_not_block_the_next_claim() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 60_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
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
        .activate_language("test.language.nonblocking-retirement", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id":"executor-nonblocking-retirement",
            "retained_tool_wait_ms":200
        }))
        .await;
    let budget = TurnBudget::new(50, 64, 256, 65_536, 67_108_864).unwrap();

    let first = tokio::spawn({
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let fresh = stack.fresh(header_with_budget(budget)).await;
        async move {
            let submitted = turns
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: fresh,
                    text: "delay the tool".into(),
                    model: None,
                    sandbox: None,
                })
                .await
                .unwrap();
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break (submitted, outcome);
                }
                tokio::task::yield_now().await;
            }
        }
    });
    entered.notified().await;
    let (submitted, first_outcome) = first.await.unwrap();
    assert!(matches!(
        first_outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            ..
        }
    ));

    let (_, second_outcome) = stack
        .resume_and_wait("the next claim must run", submitted.session_id)
        .await;
    assert_eq!(second_outcome, TurnOutcome::Completed);

    drop(stack.composition.pin.lock().unwrap().take());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the retirement deadline must release its exact generation pin");

    release.cancel();
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
async fn hanging_finalizer_becomes_a_durable_bounded_failure() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.hanging-finalizer", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let entered = Arc::new(Notify::new());
    let finalizer_lease = finalization
        .register(
            "hanging-test-finalizer".into(),
            Arc::new(HangingFinalizer {
                entered: Arc::clone(&entered),
            }),
        )
        .unwrap();
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id": "executor-finalization-timeout",
            "finalization_wait_ms": 10
        }))
        .await;

    let (_, outcome) = stack.submit_and_wait("finish with a stuck finalizer").await;
    entered.notified().await;
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "turn.finalization_timeout"
    ));

    drop(finalizer_lease);
    drop(finalization);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The three-turn public-seam interleaving is clearer as one linear timeline.
async fn checkpoint_after_a_later_acceptance_cannot_cross_the_claim_acceptance_fence() {
    let stack = BaseStack::activate().await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "second private".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let third = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "third private".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();

    let execution = stack
        .runtime
        .root()
        .lookup_local::<TurnExecutionContract>()
        .unwrap();
    let lease = execution.register("checkpoint-builder".into()).unwrap();
    let first_claim = execution
        .claim("checkpoint-builder", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id(), &first.turn_id);
    let terminal = match execution
        .publish(
            &first_claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
    {
        PublishAttempt::Published(facts) => facts,
        PublishAttempt::FlushRequired { .. } => panic!("terminal unexpectedly required a flush"),
    };
    execution
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    let mut fold =
        ContextFold::with_limits(first_claim.header().clone(), ContextLimits::default()).unwrap();
    loop {
        let after_seq = fold.through_seq();
        let page = execution
            .read_checkpoint_facts(
                &first_claim,
                after_seq,
                rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
            )
            .await
            .unwrap()
            .unwrap();
        if page.through_seq == after_seq {
            break;
        }
        fold.apply_page(&page.facts, page.through_seq).unwrap();
    }
    assert!(fold.through_seq() >= third.accepted_seq);
    assert!(
        execution
            .write_context_checkpoint(
                &first_claim,
                ContextCheckpoint {
                    header_fingerprint: first_claim.header().fingerprint().unwrap(),
                    through_seq: fold.through_seq(),
                    fact_prefix_sha256: fold.fact_prefix_sha256(),
                    bytes: fold.checkpoint_bytes().unwrap(),
                },
            )
            .await
            .unwrap()
    );
    drop(lease);

    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.checkpoint-claim-fence", fixture.clone())
        .await;
    let executor_fiber = stack
        .activate_executor("executor-checkpoint-claim-fence")
        .await;

    for submitted in [&second, &third] {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break outcome;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
    }
    {
        let requests = fixture.requests.lock().unwrap();
        let second_request = serde_json::to_string(&requests[0]).unwrap();
        assert!(second_request.contains("second private"));
        assert!(!second_request.contains("third private"));
    }
    drop(execution);
    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
async fn context_checkpoint_reads_only_suffix_and_corruption_falls_back_equivalently() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.context-checkpoint", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-context-checkpoint").await;

    let (first, first_outcome) = stack.submit_and_wait("first").await;
    assert_eq!(first_outcome, TurnOutcome::Completed);
    let first_checkpoint = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(checkpoint) = stack
                .store
                .read_context_checkpoint(&first.session_id)
                .await
                .unwrap()
            {
                break checkpoint;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stack.store.take_fact_read_cursors();

    let (_, second_outcome) = stack
        .resume_and_wait("second", first.session_id.clone())
        .await;
    assert_eq!(second_outcome, TurnOutcome::Completed);
    let suffix_cursors = stack.store.take_fact_read_cursors();
    assert_eq!(
        suffix_cursors.iter().filter(|cursor| **cursor == 0).count(),
        1,
        "only the provider fixture's durability assertion should scan from zero"
    );

    let second_checkpoint = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let checkpoint = stack
                .store
                .read_context_checkpoint(&first.session_id)
                .await
                .unwrap()
                .unwrap();
            if checkpoint.through_seq > first_checkpoint.through_seq {
                break checkpoint;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stack
        .store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: first.session_id.clone(),
            expected_durable_seq: second_checkpoint.through_seq,
            checkpoint: StoredContextCheckpoint {
                header_fingerprint: second_checkpoint.header_fingerprint,
                through_seq: second_checkpoint.through_seq,
                fact_prefix_sha256: second_checkpoint.fact_prefix_sha256,
                bytes: Arc::from(b"corrupt-context-checkpoint".as_slice()),
            },
        })
        .await
        .unwrap();
    stack.store.take_fact_read_cursors();

    let (_, third_outcome) = stack
        .resume_and_wait("third", first.session_id.clone())
        .await;
    assert_eq!(third_outcome, TurnOutcome::Completed);
    let fallback_cursors = stack.store.take_fact_read_cursors();
    assert!(
        fallback_cursors
            .iter()
            .filter(|cursor| **cursor == 0)
            .count()
            >= 2
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let third_request = serde_json::to_string(&requests[2]).unwrap();
        assert!(third_request.contains("first"));
        assert!(third_request.contains("second"));
        assert!(third_request.contains("third"));
    }
    stack.dispose(language_fiber, executor_fiber).await;
}
