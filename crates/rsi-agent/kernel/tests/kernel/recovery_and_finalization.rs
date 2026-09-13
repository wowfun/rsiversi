use super::*;

#[derive(Debug)]
struct NamedFailure(&'static str);

#[async_trait]
impl TurnFinalizer for NamedFailure {
    async fn finalize(
        &self,
        _: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        Err(TurnFinalizationError::Failed {
            code: self.0.into(),
            message: self.0.into(),
        })
    }
}

#[tokio::test]
async fn finalizer_reorder_changes_adjudication_without_restarting_contributors() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let first_position = root.child_position().unwrap();
    let second_position = root.child_position().unwrap();
    let (first, first_context) = rsi_agent_testkit::activate_contribution_owner(
        &root.with_child_position(&first_position).unwrap(),
    )
    .await
    .unwrap();
    let (second, second_context) = rsi_agent_testkit::activate_contribution_owner(
        &root.with_child_position(&second_position).unwrap(),
    )
    .await
    .unwrap();
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let _second_lease = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &second_context.registration_context().unwrap(),
        "second".into(),
        Arc::new(NamedFailure("second")),
    )
    .unwrap();
    let first_lease = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &first_context.registration_context().unwrap(),
        "first".into(),
        Arc::new(NamedFailure("first")),
    )
    .unwrap();
    let context = TurnFinalizationContext {
        session_id: SessionId::new("session-order").unwrap(),
        turn_id: TurnId::new("turn-order").unwrap(),
        job_scope: None,
    };
    let check = async |name: &str| {
        assert_eq!(
            rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context).await,
            Err(TurnFinalizationError::Failed {
                code: name.into(),
                message: name.into()
            }),
        );
    };
    check("first").await;
    let original = second.snapshot().generation;
    root.reorder_children(&[second_position.clone(), first_position.clone()])
        .unwrap();
    check("second").await;
    assert!(first.dispose().await.is_clean());
    let (_, replacement) = rsi_agent_testkit::activate_contribution_owner(
        &root.with_child_position(&first_position).unwrap(),
    )
    .await
    .unwrap();
    let _replacement_lease = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &replacement.registration_context().unwrap(),
        "first".into(),
        Arc::new(NamedFailure("replacement")),
    )
    .unwrap();
    drop(first_lease);
    root.reorder_children(&[first_position, second_position])
        .unwrap();
    check("replacement").await;
    assert_eq!(second.snapshot().generation, original);
    assert!(first_context.registration_context().is_err());
    assert!(second.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn finalizer_registry_rejects_foreign_runtime_capacity_and_shutdown() {
    let runtime = Runtime::default();
    let other = Runtime::default();
    let (_, owner) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let (_, foreign) = rsi_agent_testkit::activate_contribution_owner(&other.root())
        .await
        .unwrap();
    let credential = owner.registration_context().unwrap();
    let foreign = foreign.registration_context().unwrap();
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let register = |credential: &rsi_meta::RegistrationContext, name: String| {
        rsi_agent_turn_protocol::TurnFinalization::register(
            &kernel,
            credential,
            name,
            Arc::new(NamedFailure("failure")),
        )
    };
    let mut leases = vec![register(&credential, "first".into()).unwrap()];
    assert!(register(&foreign, "foreign".into()).is_err());
    for i in 1..rsi_agent_turn_protocol::MAXIMUM_TURN_FINALIZERS {
        leases.push(register(&credential, format!("hook-{i}")).unwrap());
    }
    assert!(register(&credential, "overflow".into()).is_err());
    drop(leases.pop());
    leases.push(register(&credential, "replacement".into()).unwrap());
    drop(leases);
    // Emptying a live registry does not transfer it to another Runtime.
    assert!(register(&foreign, "foreign".into()).is_err());
    let worker = kernel.start_workers();
    kernel.shutdown(worker).await.unwrap();
    assert!(register(&credential, "after-shutdown".into()).is_err());
    assert!(runtime.shutdown().await.is_clean());
    assert!(other.shutdown().await.is_clean());
}

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
                purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
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
            facts: (facts).into_iter().map(Into::into).collect(),
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
            facts: (vec![accepted_fact(1, &turn)])
                .into_iter()
                .map(Into::into)
                .collect(),
        })
        .await
        .unwrap();
    let composition = Arc::new(MutableComposition::new('a'));
    composition.set_unavailable();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();

    let kernel = AgentKernel::recover_with_clock(store, composition_contract, Arc::new(FixedClock))
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
            facts: (vec![
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
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
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
            facts: ({
                let first = EffectId::new("effect-one").unwrap();
                let second = EffectId::new("effect-two").unwrap();
                vec![
                    accepted_fact(1, &turn),
                    model_intent_fact(2, &turn, &first),
                    model_started_fact(3, &turn, &first),
                    model_finished_fact(4, &turn, &first),
                    model_intent_fact(5, &turn, &second),
                ]
            })
            .into_iter()
            .map(Into::into)
            .collect(),
        })
        .await
        .unwrap();
    let overused_store: Arc<dyn SessionStore> = overused;
    assert!(
        AgentKernel::recover_with_clock(overused_store, composition(), Arc::new(FixedClock),)
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
            facts: (vec![
                accepted_fact(1, &turn),
                budget_fact(2, &turn, BudgetDimension::ProviderAttempts, 1, 1),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
        })
        .await
        .unwrap();
    let mismatched_store: Arc<dyn SessionStore> = mismatched;
    assert!(
        AgentKernel::recover_with_clock(mismatched_store, composition(), Arc::new(FixedClock),)
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
            facts: (vec![
                accepted_fact(1, &turn),
                model_intent_fact(2, &turn, &effect),
                model_started_fact(3, &turn, &effect),
                model_finished_fact(4, &turn, &effect),
                budget_fact(5, &turn, BudgetDimension::ProviderAttempts, 2, 1),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
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
}

#[tokio::test]
async fn finalizers_are_effect_owned_concurrent_and_resolve_failures_by_registration_order() {
    let runtime = Runtime::default();
    let (_owner, owner_context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let credential = owner_context.registration_context().unwrap();
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
        &credential,
        "first".into(),
        make("first", false),
    )
    .unwrap();
    let failing = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
        "failing".into(),
        make("failing", true),
    )
    .unwrap();
    let _never = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
        "never".into(),
        make("never", false),
    )
    .unwrap();
    assert!(matches!(
        rsi_agent_turn_protocol::TurnFinalization::register(
            &kernel,
            &credential,
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
        &credential,
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
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn finalizer_snapshot_starts_every_hook_before_waiting_and_contains_panics() {
    let runtime = Runtime::default();
    let (_owner, owner_context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let credential = owner_context.registration_context().unwrap();
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
    let one = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
        "one".into(),
        make(false),
    )
    .unwrap();
    let two = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
        "two".into(),
        make(true),
    )
    .unwrap();
    let three = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
        "three".into(),
        make(false),
    )
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
    drop((one, two, three));
    assert!(
        rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context)
            .await
            .unwrap()
            .completion_blocker()
            .is_none()
    );
    release.notify_waiters();
    assert!(matches!(
        finalization.await.unwrap(),
        Err(TurnFinalizationError::Failed { code, .. }) if code == "test.concurrent_failure"
    ));

    let calls = Arc::new(Mutex::new(Vec::new()));
    let _panic = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
        "panic".into(),
        Arc::new(PanickingFinalizer),
    )
    .unwrap();
    let _after = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        &credential,
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
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Both startup attempts and the failure barrier prove one durable history.
async fn partial_recovery_restarts_after_the_last_correlated_terminal_without_rewriting_it() {
    let memory = Arc::new(MemoryStore::new());
    let session = SessionId::new("partial-terminal-recovery").unwrap();
    let first = TurnId::new("first-recovery-turn").unwrap();
    let second = TurnId::new("second-recovery-turn").unwrap();
    let accepted = |seq, turn_id| {
        Arc::new(
            SessionFact::new(
                seq,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id,
                    text: "unfinished".into(),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        )
    };
    memory
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![
                accepted(1, first.clone()),
                accepted(2, second.clone()),
                Arc::new(
                    SessionFact::new(
                        3,
                        1,
                        SessionFactBody::CancelRequested {
                            turn_id: second.clone(),
                            reason: None,
                        },
                    )
                    .unwrap(),
                ),
            ],
        })
        .await
        .unwrap();
    let observed = Arc::new(FactReadRaceStore::new(memory.clone()));
    observed.pause_next_agent_commit_after_apply();
    let recovering = tokio::spawn({
        let observed = observed.clone();
        async move {
            AgentKernel::recover_with_clock(observed, composition(), Arc::new(FixedClock)).await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observed.wait_until_agent_commit_is_applied(),
    )
    .await
    .unwrap();
    let first_boundary = memory.read_turn_boundary(&session, &first).await.unwrap();
    assert!(matches!(
        first_boundary.terminal().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Interrupted { .. },
            ..
        }
    ));
    let first_marker = memory.read_controls(&session, 0, 8).await.unwrap().records;
    assert_eq!(first_marker.len(), 1);
    assert_eq!(
        memory
            .list_open_turns(&session, 0, 8)
            .await
            .unwrap()
            .turns
            .len(),
        1
    );
    memory.fail_next_appends(1);
    observed.release_applied_agent_commit();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), recovering)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let recovered =
        AgentKernel::recover_with_clock(memory.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let controls = memory.read_controls(&session, 0, 8).await.unwrap();
    assert_eq!(controls.records.len(), 2);
    assert_eq!(controls.records[0], first_marker[0]);
    assert!(
        matches!(controls.records[1].body(), AgentControlRecordBody::TurnBoundaryRecorded { turn_id, terminal_fact_seq: 5 } if turn_id == &second)
    );
    assert_eq!(
        recovered.outcome(&session, &second).await.unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    assert_eq!(
        memory
            .read_turn_boundary(&session, &first)
            .await
            .unwrap()
            .terminal(),
        first_boundary.terminal()
    );
    assert!(
        memory
            .list_open_turns(&session, 0, 8)
            .await
            .unwrap()
            .turns
            .is_empty()
    );
    let worker = recovered.start_workers();
    recovered.shutdown(worker).await.unwrap();
}
