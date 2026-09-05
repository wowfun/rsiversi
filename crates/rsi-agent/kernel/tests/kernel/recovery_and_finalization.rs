use super::*;

#[tokio::test]
async fn recovery_appends_interrupted_for_a_started_external_effect_and_never_requeues_it() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery").unwrap();
    let turn = TurnId::new("turn-recovery").unwrap();
    let effect = EffectId::new("effect-recovery").unwrap();
    let facts = vec![
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "hello".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
        SessionFact::new(
            2,
            2,
            SessionFactBody::ModelIntent {
                turn_id: turn.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::ModelStarted {
                turn_id: turn.clone(),
                effect_id: effect,
            },
        )
        .unwrap(),
    ];
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-recovery")),
            facts,
        })
        .await
        .unwrap();
    let kernel = kernel(store.clone()).await;
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Interrupted {
            effect: Some(EffectKind::Model),
            reason: "Kernel recovery found a turn without a durable terminal Fact".into(),
        })
    );
    let repaired = store.read_facts(&session, 3, 8).await.unwrap();
    assert_eq!(repaired.facts.len(), 1);
    assert!(matches!(
        repaired.facts[0].body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Interrupted {
                effect: Some(EffectKind::Model),
                ..
            },
            ..
        }
    ));
    let _lease = kernel.register("executor".into()).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(
        kernel
            .claim("executor", cancellation)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn startup_recovery_repairs_open_turns_without_resolving_the_preset() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-unavailable-preset").unwrap();
    let turn = TurnId::new("turn-recovery-unavailable-preset").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![accepted_fact(1, &turn)],
        })
        .await
        .unwrap();
    let composition = Arc::new(MutableComposition::new('a'));
    composition.set_unavailable();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();

    let kernel =
        SessionKernel::recover_with_clock(store, composition_contract, Arc::new(FixedClock))
            .await
            .expect("startup repair must not require an executable Agent preset");

    assert!(matches!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Interrupted { effect: None, .. })
    ));
    assert_eq!(composition.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn recovery_preserves_a_durable_cancellation_classification() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-cancelled").unwrap();
    let turn = TurnId::new("turn-recovery-cancelled").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-recovery-cancelled")),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn.clone(),
                        text: "hello".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::CancelRequested {
                        turn_id: turn.clone(),
                        reason: Some("stop".into()),
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();

    let kernel = kernel(store.clone()).await;
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    let repaired = store.read_facts(&session, 2, 8).await.unwrap();
    assert!(matches!(
        repaired.facts.as_slice(),
        [fact]
            if matches!(
                fact.body(),
                SessionFactBody::TurnTerminal {
                    outcome: TurnOutcome::Cancelled,
                    ..
                }
            )
    ));
}

#[tokio::test]
async fn recovery_rejects_usage_and_markers_that_exceed_the_frozen_budget() {
    let overused = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-budget-usage").unwrap();
    let turn = TurnId::new("turn-recovery-budget-usage").unwrap();
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();
    let bounded_header = SessionHeader::new(
        session.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new_with_budget(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
            budget,
        )
        .unwrap(),
    )
    .unwrap();
    overused
        .append(AppendBatch {
            session_id: session,
            expected_seq: 0,
            header: Some(bounded_header),
            facts: {
                let first = EffectId::new("effect-one").unwrap();
                let second = EffectId::new("effect-two").unwrap();
                vec![
                    accepted_fact(1, &turn),
                    model_intent_fact(2, &turn, &first),
                    model_started_fact(3, &turn, &first),
                    model_finished_fact(4, &turn, &first),
                    model_intent_fact(5, &turn, &second),
                ]
            },
        })
        .await
        .unwrap();
    let overused_store: Arc<dyn SessionStore> = overused;
    assert!(
        SessionKernel::recover_with_clock(overused_store, composition(), Arc::new(FixedClock),)
            .await
            .is_err(),
        "recovery must apply the immutable provider-attempt limit"
    );

    let mismatched = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-budget-marker").unwrap();
    let turn = TurnId::new("turn-recovery-budget-marker").unwrap();
    mismatched
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![
                accepted_fact(1, &turn),
                budget_fact(2, &turn, BudgetDimension::ProviderAttempts, 1, 1),
            ],
        })
        .await
        .unwrap();
    let mismatched_store: Arc<dyn SessionStore> = mismatched;
    assert!(
        SessionKernel::recover_with_clock(mismatched_store, composition(), Arc::new(FixedClock),)
            .await
            .is_err(),
        "a durable exhaustion marker must match the immutable budget"
    );
}

#[tokio::test]
async fn recovery_preserves_a_valid_durable_budget_classification() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-valid-budget").unwrap();
    let turn = TurnId::new("turn-recovery-valid-budget").unwrap();
    let effect = EffectId::new("effect-recovery-valid-budget").unwrap();
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();
    let bounded_header = SessionHeader::new(
        session.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new_with_budget(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
            budget,
        )
        .unwrap(),
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(bounded_header),
            facts: vec![
                accepted_fact(1, &turn),
                model_intent_fact(2, &turn, &effect),
                model_started_fact(3, &turn, &effect),
                model_finished_fact(4, &turn, &effect),
                budget_fact(5, &turn, BudgetDimension::ProviderAttempts, 2, 1),
            ],
        })
        .await
        .unwrap();

    let kernel = kernel(store.clone()).await;
    let expected = TurnOutcome::BudgetExceeded {
        dimension: BudgetDimension::ProviderAttempts,
        consumed: 2,
        limit: 1,
    };
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(expected.clone())
    );
    assert!(matches!(
        store.read_facts(&session, 5, 8).await.unwrap().facts.as_slice(),
        [fact]
            if matches!(
                fact.body(),
                SessionFactBody::TurnTerminal { outcome, .. } if outcome == &expected
            )
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The plugin lifecycle regression keeps its complete dependency activation and withdrawal visible.
async fn ordinary_factory_waits_for_store_and_withdraws_all_turn_contracts() {
    let runtime = Runtime::default();
    let kernel_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.kernel",
                "kernel",
                UpdateMode::Replayable,
                Arc::new(KernelFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    let store = Arc::new(MemoryStore::new());
    let store_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.store.memory",
                "store",
                UpdateMode::Replayable,
                Arc::new(MemoryStoreFactory::new(store)),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    let composition_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "test.agent.composition",
                "composition",
                UpdateMode::Replayable,
                Arc::new(TestCompositionFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    let workspace_context_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.workspace-context",
                "workspace-context",
                UpdateMode::Replayable,
                Arc::new(WorkspaceContextFactory),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_some()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .is_some()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnFinalizationContract>()
            .is_some()
    );
    assert!(kernel_fiber.dispose().await.is_clean());
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnFinalizationContract>()
            .is_none()
    );
    assert!(store_fiber.dispose().await.is_clean());
    assert!(composition_fiber.dispose().await.is_clean());
    assert!(workspace_context_fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn finalizers_are_effect_owned_concurrent_and_resolve_failures_by_registration_order() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let make = |name, fail| {
        Arc::new(RecordingFinalizer {
            name,
            calls: calls.clone(),
            fail,
        }) as Arc<dyn TurnFinalizer>
    };
    let first = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "first".into(),
        make("first", false),
    )
    .unwrap();
    let failing = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "failing".into(),
        make("failing", true),
    )
    .unwrap();
    let _never = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "never".into(),
        make("never", false),
    )
    .unwrap();
    assert!(matches!(
        rsi_agent_turn_protocol::TurnFinalization::register(
            &kernel,
            "first".into(),
            make("duplicate", false)
        ),
        Err(TurnFinalizationError::Invalid(_))
    ));

    let session = SessionId::new("session-finalizers").unwrap();
    let turn = TurnId::new("turn-finalizers").unwrap();
    let context = TurnFinalizationContext {
        session_id: session,
        turn_id: turn,
        job_scope: None,
    };
    assert_eq!(
        rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context).await,
        Err(TurnFinalizationError::Failed {
            code: "test.failed".into(),
            message: "test finalizer failed".into(),
        })
    );
    let mut observed = calls.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, vec!["failing", "first", "never"]);

    calls.lock().unwrap().clear();
    drop(failing);
    let _replacement = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "failing".into(),
        make("replacement", false),
    )
    .unwrap();
    rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context)
        .await
        .unwrap();
    let mut observed = calls.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, vec!["first", "never", "replacement"]);

    calls.lock().unwrap().clear();
    drop(first);
    rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context)
        .await
        .unwrap();
    let mut observed = calls.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, vec!["never", "replacement"]);
}

#[tokio::test]
async fn finalizer_snapshot_starts_every_hook_before_waiting_and_contains_panics() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let entered = Arc::new(AtomicUsize::new(0));
    let entered_changed = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let make = |fail| {
        Arc::new(CoordinatedFinalizer {
            entered: Arc::clone(&entered),
            entered_changed: Arc::clone(&entered_changed),
            release: Arc::clone(&release),
            fail,
        }) as Arc<dyn TurnFinalizer>
    };
    let one =
        rsi_agent_turn_protocol::TurnFinalization::register(&kernel, "one".into(), make(false))
            .unwrap();
    let two =
        rsi_agent_turn_protocol::TurnFinalization::register(&kernel, "two".into(), make(true))
            .unwrap();
    let three =
        rsi_agent_turn_protocol::TurnFinalization::register(&kernel, "three".into(), make(false))
            .unwrap();
    let context = TurnFinalizationContext {
        session_id: SessionId::new("session-concurrent-finalizers").unwrap(),
        turn_id: TurnId::new("turn-concurrent-finalizers").unwrap(),
        job_scope: None,
    };
    let concurrent_kernel = kernel.clone();
    let concurrent_context = context.clone();
    let finalization = tokio::spawn(async move {
        rsi_agent_turn_protocol::TurnFinalization::finalize(&concurrent_kernel, &concurrent_context)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let notified = entered_changed.notified();
            if entered.load(Ordering::Acquire) == 3 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("all finalizers must start concurrently");
    release.notify_waiters();
    assert!(matches!(
        finalization.await.unwrap(),
        Err(TurnFinalizationError::Failed { code, .. }) if code == "test.concurrent_failure"
    ));

    drop((one, two, three));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _panic = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "panic".into(),
        Arc::new(PanickingFinalizer),
    )
    .unwrap();
    let _after = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "after-panic".into(),
        Arc::new(RecordingFinalizer {
            name: "after-panic",
            calls: Arc::clone(&calls),
            fail: false,
        }),
    )
    .unwrap();
    assert!(matches!(
        rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context).await,
        Err(TurnFinalizationError::Failed { code, .. }) if code == "turn.finalizer_panic"
    ));
    assert_eq!(*calls.lock().unwrap(), vec!["after-panic"]);
}
