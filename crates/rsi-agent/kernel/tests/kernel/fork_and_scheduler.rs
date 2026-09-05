use super::*;

#[tokio::test]
async fn fork_replay_validates_its_immutable_boundary_only_at_the_initial_cursor() {
    let store = Arc::new(MemoryStore::new());
    append_terminal_history(&store, "session-fork-page-root", 300).await;
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-fork-page-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: resume(&kernel, root_id.clone()).await,
            message: mailbox_message("message-fork-page-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let root_lease = kernel.register("executor-fork-page-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-fork-page-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child_id = SessionId::new("session-fork-page-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: kernel.agent_caller(&root_claim).unwrap(),
            child_session_id: child_id.clone(),
            task_name: "paged-child".into(),
            message_id: MessageId::new("message-fork-page-child").unwrap(),
            message: "inherit the complete balanced prefix".into(),
            fork_turns: ForkTurnSelection::All,
        })
        .await
        .unwrap();
    assert_eq!(store.fork_boundary_resolution_count(), 1);

    let child_lease = kernel.register("executor-fork-page-child".into()).unwrap();
    let child_claim = kernel
        .claim("executor-fork-page-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_claim.session_id(), &child_id);
    let first = kernel
        .read_fork_facts(&child_claim, 0, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.facts.len(), MAXIMUM_FACTS_PER_READ);
    assert_eq!(store.fork_boundary_resolution_count(), 2);
    let second = kernel
        .read_fork_facts(
            &child_claim,
            first.through_parent_seq,
            MAXIMUM_FACTS_PER_READ,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.through_parent_seq, second.terminal_parent_seq);
    assert_eq!(second.facts.len(), 88);
    assert_eq!(
        store.fork_boundary_resolution_count(),
        2,
        "later pages must reuse the boundary established at the initial cursor"
    );

    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    drop((child_lease, root_lease));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn a_busy_session_message_does_not_block_an_idle_child_in_the_same_tree() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-a-busy-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-root-active"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-busy-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-busy-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();

    kernel
        .submit_message(SubmitMessage {
            session: resume(&kernel, root_id.clone()).await,
            message: mailbox_message("message-root-queued"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let child_id = SessionId::new("session-z-idle-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller,
            child_session_id: child_id.clone(),
            task_name: "idle-child".into(),
            message_id: MessageId::new("message-child-ready").unwrap(),
            message: "claim me while the root remains busy".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();

    let _child_lease = kernel.register("executor-idle-child".into()).unwrap();
    let child_claim = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        kernel.claim("executor-idle-child", CancellationToken::new()),
    )
    .await
    .expect("the busy root's earlier ready message must not block its idle child")
    .unwrap()
    .unwrap();
    assert_eq!(child_claim.session_id(), &child_id);

    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn concurrent_executor_lanes_contend_for_one_ready_message_without_failure() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-concurrent-ready-claim").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-concurrent-ready-claim"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _one = kernel.register("executor-ready-one".into()).unwrap();
    let _two = kernel.register("executor-ready-two".into()).unwrap();
    let cancellation = CancellationToken::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = tokio::task::JoinSet::new();
    for executor in ["executor-ready-one", "executor-ready-two"] {
        let kernel = kernel.clone();
        let cancellation = cancellation.child_token();
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            barrier.wait().await;
            kernel.claim(executor, cancellation).await
        });
    }
    barrier.wait().await;
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), tasks.join_next())
        .await
        .expect("one claim lane did not acquire the ready message")
        .expect("claim task set ended early")
        .unwrap()
        .expect("ready-message contention escaped as a claim failure")
        .expect("one claim lane must acquire the ready message");
    cancellation.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), tasks.join_next())
            .await
            .expect("the losing claim lane did not observe cancellation")
            .expect("claim task set ended early")
            .unwrap()
            .expect("the losing claim lane failed during ordinary contention")
            .is_none()
    );

    kernel
        .finish_activation_turn(&first, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Cancellation and budget cases share the full live-descendant and preserved-inbox contract.
async fn cancelled_or_budget_exhausted_parent_cascades_without_erasing_child_inbox() {
    for (label, parent_outcome) in [
        ("cancel", None),
        ("budget", Some((BudgetDimension::Elapsed, 1_800_000_u64))),
    ] {
        let store = Arc::new(MemoryStore::new());
        let kernel = kernel(store.clone()).await;
        let worker = kernel.start_write_behind();
        let root_id = SessionId::new(format!("session-cascade-root-{label}")).unwrap();
        let root_message = MessageId::new(format!("message-cascade-root-{label}")).unwrap();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(root_id.as_str())),
                message: mailbox_message(root_message.as_str()),
                target: MessageTarget::NextTurn,
                wake_required: true,
            })
            .await
            .unwrap();
        kernel
            .claim_message(ClaimMessage {
                session: kernel.prepare_resume(&root_id).await.unwrap(),
                message_id: root_message,
                activation_id: ActivationId::new(format!("activation-cascade-root-{label}"))
                    .unwrap(),
                path: AgentPath::root(),
                turn_id: TurnId::new(format!("turn-cascade-root-{label}")).unwrap(),
                step_id: StepId::new(format!("step-cascade-root-{label}")).unwrap(),
            })
            .await
            .unwrap();
        let _root_lease = kernel
            .register(format!("executor-cascade-root-{label}"))
            .unwrap();
        let root_claim = kernel
            .claim(
                &format!("executor-cascade-root-{label}"),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        let caller = kernel.agent_caller(&root_claim).unwrap();
        let child_id = SessionId::new(format!("session-cascade-child-{label}")).unwrap();
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller: caller.clone(),
                child_session_id: child_id.clone(),
                task_name: format!("child-{label}"),
                message_id: MessageId::new(format!("message-cascade-child-{label}")).unwrap(),
                message: "perform child work".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await
            .unwrap();
        let _child_lease = kernel
            .register(format!("executor-cascade-child-{label}"))
            .unwrap();
        let child_claim = kernel
            .claim(
                &format!("executor-cascade-child-{label}"),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        let child_cancellation = kernel.cancellation(&child_claim).unwrap();
        let queued = kernel
            .submit(SubmitTurn {
                session: resume(&kernel, child_id.clone()).await,
                turn_id: TurnId::new(format!("turn-queued-child-{label}")).unwrap(),
                text: "already accepted behind the running child".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        kernel
            .flush(&child_claim, queued.accepted_seq)
            .await
            .unwrap();
        let held_message = MessageId::new(format!("message-held-child-{label}")).unwrap();
        kernel
            .send_agent_message(SendAgentMessage {
                caller,
                target_session_id: child_id.clone(),
                message_id: held_message.clone(),
                message: "retain this accepted follow-up".into(),
                start_new_turn: false,
            })
            .await
            .unwrap();

        let outcome = if let Some((dimension, limit)) = parent_outcome {
            let budget_outcome = TurnOutcome::BudgetExceeded {
                dimension,
                consumed: limit,
                limit,
            };
            kernel
                .close_current_step(&root_claim, &budget_outcome)
                .await
                .unwrap();
            kernel
                .publish(
                    &root_claim,
                    vec![SessionFactBody::BudgetExhausted {
                        turn_id: root_claim.turn_id().clone(),
                        dimension,
                        consumed: limit,
                        limit,
                    }],
                )
                .await
                .unwrap();
            budget_outcome
        } else {
            kernel
                .cancel(&root_id, root_claim.turn_id(), Some("stop tree".into()))
                .await
                .unwrap();
            TurnOutcome::Cancelled
        };
        kernel
            .finish_activation_turn(&root_claim, &outcome)
            .await
            .unwrap()
            .unwrap();
        assert!(
            child_cancellation.is_cancelled(),
            "{label} must durably cascade to a live child"
        );
        assert_eq!(
            store
                .active_activation(&root_id)
                .await
                .unwrap()
                .unwrap()
                .phase,
            StoreActivationPhase::WaitingForDescendants
        );
        assert!(matches!(
            kernel
                .message_status(&child_id, &held_message)
                .await
                .unwrap()
                .state,
            MessageState::Pending
        ));

        kernel
            .finish_activation_turn(&child_claim, &TurnOutcome::Cancelled)
            .await
            .unwrap()
            .unwrap();
        let queued_claim = kernel
            .claim(
                &format!("executor-cascade-child-{label}"),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queued_claim.turn_id(), &queued.turn_id);
        assert!(kernel.cancellation(&queued_claim).unwrap().is_cancelled());
        let terminal = kernel
            .publish(
                &queued_claim,
                vec![SessionFactBody::TurnTerminal {
                    turn_id: queued.turn_id,
                    outcome: TurnOutcome::Cancelled,
                }],
            )
            .await
            .unwrap()
            .published();
        kernel
            .flush(&queued_claim, terminal[0].seq())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while store.active_activation(&root_id).await.unwrap().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the drained descendant must release its waiting ancestors");
        assert!(store.active_activation(&child_id).await.unwrap().is_none());
        assert!(matches!(
            kernel
                .message_status(&child_id, &held_message)
                .await
                .unwrap()
                .state,
            MessageState::Pending
        ));
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_an_unclaimed_message_is_durable_and_idempotent() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-message-cancel").unwrap();
    let message_id = MessageId::new("message-cancel").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    assert!(
        kernel
            .cancel_target(&session_id, CancelTarget::Message(message_id.clone()), None)
            .await
            .unwrap()
            .accepted
    );
    let repeated = kernel
        .cancel_target(&session_id, CancelTarget::Message(message_id.clone()), None)
        .await
        .unwrap();
    assert!(!repeated.accepted);
    assert!(repeated.already_terminal);
    assert!(matches!(
        kernel
            .message_status(&session_id, &message_id)
            .await
            .unwrap()
            .state,
        MessageState::Discarded { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn recovery_resumes_a_durably_parked_wait_before_interrupting_its_activation() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let worker = initial.start_write_behind();
    let root_id = SessionId::new("session-parked-recovery-root").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-parked-recovery-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = initial
        .register("executor-parked-recovery-root".into())
        .unwrap();
    let root_claim = initial
        .claim("executor-parked-recovery-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = initial.agent_caller(&root_claim).unwrap();
    let child_id = SessionId::new("session-parked-recovery-child").unwrap();
    initial
        .spawn_agent(SpawnAgentRequest {
            caller: caller.clone(),
            child_session_id: child_id,
            task_name: "parked-recovery-child".into(),
            message_id: MessageId::new("message-parked-recovery-child").unwrap(),
            message: "keep the parent wait parked".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let waiter = tokio::spawn({
        let initial = initial.clone();
        async move {
            initial
                .wait_agent(
                    &caller,
                    std::time::Duration::from_mins(1),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if store
                .active_activation(&root_id)
                .await
                .unwrap()
                .is_some_and(|active| active.phase == StoreActivationPhase::Parked)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("wait did not durably park");

    initial.shutdown(worker).await.unwrap();
    assert!(waiter.await.unwrap().is_err());
    let recovered = kernel(store.clone()).await;
    assert!(matches!(
        recovered
            .outcome(&root_id, root_claim.turn_id())
            .await
            .unwrap(),
        Some(TurnOutcome::Interrupted { .. })
    ));
    let controls = store.read_controls(&root_id, 0, 32).await.unwrap();
    let resumed = controls.records.iter().position(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                cause: WaitResumeCause::Cancel,
                ..
            }
        )
    });
    let waiting = controls.records.iter().position(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::ActivationWaitingForDescendants { .. }
        )
    });
    assert!(
        resumed
            .zip(waiting)
            .is_some_and(|(resumed, waiting)| resumed < waiting)
    );
}

#[tokio::test]
async fn claimed_message_retry_reuses_the_stored_acceptance_boundary_with_background_context() {
    let store = Arc::new(MemoryStore::new());
    let context = Arc::new(QueuedWorkspaceContext {
        snapshots: Mutex::new(VecDeque::from([WorkspaceContextSnapshot {
            complete: true,
            instructions_sha256: "a".repeat(64),
            instructions: Some("workspace instructions".into()),
            skill_catalog_sha256: "b".repeat(64),
            skill_catalog: Some("<available_skills>test</available_skills>".into()),
            invocations: Vec::new(),
        }])),
        calls: AtomicUsize::new(0),
    });
    let initial = SessionKernel::recover_with_context_clock_and_limits(
        store.clone(),
        composition(),
        context,
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let worker = initial.start_write_behind();
    let session_id = SessionId::new("session-claim-retry-boundary").unwrap();
    let message_id = MessageId::new("message-claim-retry-boundary").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let activation_id = ActivationId::new("activation-claim-retry-boundary").unwrap();
    let turn_id = TurnId::new("turn-claim-retry-boundary").unwrap();
    let step_id = StepId::new("step-claim-retry-boundary").unwrap();
    let submitted = initial
        .claim_message(ClaimMessage {
            session: initial.prepare_resume(&session_id).await.unwrap(),
            message_id: message_id.clone(),
            activation_id: activation_id.clone(),
            path: AgentPath::root(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(submitted.accepted_seq, 1);
    assert_eq!(
        initial
            .message_status(&session_id, &message_id)
            .await
            .unwrap()
            .state,
        MessageState::Claimed {
            activation_id: activation_id.clone(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
            entered_fact_seq: 5,
        }
    );
    initial.shutdown(worker).await.unwrap();

    let recovered = kernel(store).await;
    let retried = recovered
        .claim_message(ClaimMessage {
            session: recovered.prepare_resume(&session_id).await.unwrap(),
            message_id,
            activation_id,
            path: AgentPath::root(),
            turn_id,
            step_id,
        })
        .await
        .unwrap();
    assert_eq!(retried.accepted_seq, submitted.accepted_seq);
}

#[tokio::test]
async fn activation_terminal_requeues_the_next_oldest_turn() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-activation-handoff").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-activation-handoff"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-activation-handoff".into())
        .unwrap();
    let activation_claim = kernel
        .claim("executor-activation-handoff", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let queued = kernel
        .submit(SubmitTurn {
            session: resume(&kernel, session_id.clone()).await,
            turn_id: TurnId::new("turn-after-activation").unwrap(),
            text: "queued behind activation".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        kernel
            .claim("executor-activation-handoff", cancelled)
            .await
            .unwrap()
            .is_none()
    );

    kernel
        .finish_activation_turn(&activation_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        kernel.claim("executor-activation-handoff", CancellationToken::new()),
    )
    .await
    .expect("activation terminal did not requeue the next accepted Turn")
    .unwrap()
    .unwrap();
    assert_eq!(next.turn_id(), &queued.turn_id);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn completed_activation_sessions_release_resident_capacity() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let _lease = kernel
        .register("executor-activation-eviction".into())
        .unwrap();
    for index in 0..=MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("session-activation-eviction-{index}")).unwrap();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(session_id.as_str())),
                message: mailbox_message(&format!("message-activation-eviction-{index}")),
                target: MessageTarget::NextTurn,
                wake_required: true,
            })
            .await
            .unwrap();
        let claim = kernel
            .claim("executor-activation-eviction", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        kernel
            .finish_activation_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap()
            .unwrap();
    }
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn submit_message_rejects_a_waking_next_step_before_durable_admission() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let session_id = SessionId::new("session-invalid-wake-target").unwrap();
    assert!(matches!(
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(session_id.as_str())),
                message: mailbox_message("message-invalid-wake-target"),
                target: MessageTarget::NextStep,
                wake_required: true,
            })
            .await,
        Err(TurnError::Invalid(_))
    ));
    assert!(matches!(
        store.header(&session_id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn fresh_message_cannot_publish_over_an_unflushed_fresh_turn_header() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-fresh-message-race").unwrap();
    store.pause_next_append();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = session_id.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    session: fresh(header(session_id.as_str())),
                    turn_id: TurnId::new("turn-fresh-message-race").unwrap(),
                    text: "first fresh submission".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_append_is_blocked().await;
    let competing = kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-fresh-message-race"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await;
    store.release_blocked_append();
    let first = first.await.unwrap();

    assert!(matches!(competing, Err(TurnError::Invalid(_))));
    assert!(first.is_ok(), "winning fresh Turn was corrupted: {first:?}");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn controls_only_message_commit_retries_a_concurrent_fact_flush() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-message-flush-race", "run").await;
    let _lease = kernel
        .register("executor-message-flush-race".into())
        .unwrap();
    let claim = kernel
        .claim("executor-message-flush-race", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &submitted.turn_id);

    store.pause_next_append();
    let pending = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: claim.turn_id().clone(),
                effect_id: EffectId::new("effect-message-flush-race").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let pending_seq = pending[0].seq();
    let resumed = resume(&kernel, submitted.session_id.clone()).await;
    store.pause_next_agent_commit_before_apply();
    let message = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit_message(SubmitMessage {
                    session: resumed,
                    message: mailbox_message("message-flush-race"),
                    target: MessageTarget::NextStep,
                    wake_required: false,
                })
                .await
        }
    });
    store.wait_until_agent_commit_is_before_apply().await;

    let flush = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.flush(&claim, pending_seq).await }
    });
    store.wait_until_append_is_blocked().await;
    store.release_blocked_append();
    assert_eq!(flush.await.unwrap().unwrap(), pending_seq);
    store.release_agent_commit_before_apply();

    let receipt = message
        .await
        .unwrap()
        .expect("a concurrent write-behind Fact flush is a retryable internal race");
    assert_eq!(receipt.observed_fact_seq, pending_seq);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancellation_cannot_diverge_resident_state_from_an_applied_activation_terminal() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-terminal-cancel-race").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-terminal-cancel-race"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _lease = kernel
        .register("executor-terminal-cancel-race".into())
        .unwrap();
    let claim = kernel
        .claim("executor-terminal-cancel-race", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    store.pause_next_agent_commit_after_apply();
    let finish = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move {
            kernel
                .finish_activation_turn(&claim, &TurnOutcome::Completed)
                .await
        }
    });
    store.wait_until_agent_commit_is_applied().await;
    let cancel = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = session_id.clone();
        let turn_id = claim.turn_id().clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    store.release_applied_agent_commit();

    assert!(
        finish.await.unwrap().is_ok(),
        "applied terminal commit diverged from resident state"
    );
    let cancelled = cancel.await.unwrap().unwrap();
    assert!(!cancelled.accepted);
    assert!(cancelled.already_terminal);
    kernel.shutdown(worker).await.unwrap();
}
