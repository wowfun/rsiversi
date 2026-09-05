use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Both delivery horizons must run against the same live-to-idle target transition.
async fn send_and_followup_delivery_horizons_do_not_depend_on_a_target_race() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-delivery-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-delivery-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-delivery-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-delivery-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();
    let child_id = SessionId::new("session-delivery-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: caller.clone(),
            child_session_id: child_id.clone(),
            task_name: "delivery-child".into(),
            message_id: MessageId::new("message-delivery-child").unwrap(),
            message: "start the child".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel.register("executor-delivery-child".into()).unwrap();
    let child_claim = kernel
        .claim("executor-delivery-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    let step_message = MessageId::new("message-delivery-step").unwrap();
    kernel
        .send_agent_message(SendAgentMessage {
            caller: caller.clone(),
            target_session_id: child_id.clone(),
            message_id: step_message.clone(),
            message: "inject into the running activation".into(),
            start_new_turn: false,
        })
        .await
        .unwrap();
    let turn_message = MessageId::new("message-delivery-turn").unwrap();
    kernel
        .send_agent_message(SendAgentMessage {
            caller,
            target_session_id: child_id.clone(),
            message_id: turn_message.clone(),
            message: "queue a separate waking activation".into(),
            start_new_turn: true,
        })
        .await
        .unwrap();

    let mailbox = store.read_agent_mailbox(&child_id, None).await.unwrap();
    assert!(mailbox.pending.iter().any(|entry| {
        entry.message.message_id == step_message
            && entry.target == MessageTarget::NextStep
            && !entry.wake_required
    }));
    assert!(mailbox.pending.iter().any(|entry| {
        entry.message.message_id == turn_message
            && entry.target == MessageTarget::NextTurn
            && entry.wake_required
    }));
    assert_eq!(
        store
            .list_ready_messages(&root_id, None, 8)
            .await
            .unwrap()
            .messages
            .iter()
            .map(|message| &message.message_id)
            .collect::<Vec<_>>(),
        [&turn_message]
    );

    kernel
        .cancel_target(&child_id, CancelTarget::Message(turn_message), None)
        .await
        .unwrap();
    assert_eq!(
        kernel
            .enter_pending_step_messages(&child_claim)
            .await
            .unwrap(),
        1
    );
    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel
            .enter_pending_step_messages(&root_claim)
            .await
            .unwrap(),
        1
    );
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn recovery_closes_an_activation_step_before_interrupting_its_turn() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let initial_worker = initial.start_write_behind();
    let session_id = SessionId::new("session-message-step-recovery").unwrap();
    let message_id = MessageId::new("message-step-recovery").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let turn_id = TurnId::new("turn-message-step-recovery").unwrap();
    let step_id = StepId::new("step-message-step-recovery").unwrap();
    initial
        .claim_message(ClaimMessage {
            session: initial.prepare_resume(&session_id).await.unwrap(),
            message_id,
            activation_id: ActivationId::new("activation-message-step-recovery").unwrap(),
            path: AgentPath::root(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
        })
        .await
        .unwrap();
    initial.shutdown(initial_worker).await.unwrap();

    let restarted = kernel(store.clone()).await;
    assert!(matches!(
        restarted.outcome(&session_id, &turn_id).await.unwrap(),
        Some(TurnOutcome::Interrupted { effect: None, .. })
    ));
    let repaired = store.read_facts(&session_id, 3, 8).await.unwrap();
    assert_eq!(repaired.facts.len(), 2);
    assert!(matches!(
        repaired.facts[0].body(),
        SessionFactBody::StepEnded {
            step_id: recovered,
            ..
        } if recovered == &step_id
    ));
    assert!(matches!(
        repaired.facts[1].body(),
        SessionFactBody::TurnTerminal { .. }
    ));
    assert!(
        store
            .active_activation(&session_id)
            .await
            .unwrap()
            .is_none(),
        "startup reconciliation must settle the repaired root activation"
    );
}

#[tokio::test]
async fn child_completion_settles_a_waiting_parent_and_wakes_its_idle_mailbox() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-activation-root").unwrap();
    let root_message = MessageId::new("message-activation-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message(root_message.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let root_turn = TurnId::new("turn-activation-root").unwrap();
    kernel
        .claim_message(ClaimMessage {
            session: kernel.prepare_resume(&root_id).await.unwrap(),
            message_id: root_message,
            activation_id: ActivationId::new("activation-root").unwrap(),
            path: AgentPath::root(),
            turn_id: root_turn,
            step_id: StepId::new("step-activation-root").unwrap(),
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-activation-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-activation-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();
    let child_id = SessionId::new("session-activation-child").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.spawn_agent(SpawnAgentRequest {
            caller,
            child_session_id: child_id.clone(),
            task_name: "child".into(),
            message_id: MessageId::new("message-activation-child").unwrap(),
            message: "perform child work".into(),
            fork_turns: ForkTurnSelection::None,
        }),
    )
    .await
    .expect("spawn must not stall")
    .unwrap();
    let _child_lease = kernel.register("executor-activation-child".into()).unwrap();
    let child_claim = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.claim("executor-activation-child", CancellationToken::new()),
    )
    .await
    .expect("child claim must not stall")
    .unwrap()
    .unwrap();
    assert_eq!(child_claim.session_id(), &child_id);

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.finish_activation_turn(&root_claim, &TurnOutcome::Completed),
    )
    .await
    .expect("root terminal must not stall")
    .unwrap()
    .unwrap();
    assert_eq!(
        store
            .active_activation(&root_id)
            .await
            .unwrap()
            .unwrap()
            .phase,
        StoreActivationPhase::WaitingForDescendants
    );

    store.mismatch_active_rechecks(
        root_id.clone(),
        rsi_agent_session_protocol::MAXIMUM_AGENT_TREE_DEPTH + 1,
    );

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.finish_activation_turn(&child_claim, &TurnOutcome::Completed),
    )
    .await
    .expect("child terminal must not stall")
    .unwrap()
    .unwrap();
    assert!(store.active_activation(&child_id).await.unwrap().is_none());
    assert!(store.active_activation(&root_id).await.unwrap().is_none());
    let ready = store.list_ready_messages(&root_id, None, 8).await.unwrap();
    assert_eq!(ready.messages.len(), 1);
    assert_eq!(ready.messages[0].session_id, root_id);
    assert_eq!(ready.messages[0].target, MessageTarget::NextTurn);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn parent_terminal_promotes_a_completion_that_arrived_after_its_last_step_scan() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-terminal-promotion-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-terminal-promotion-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-terminal-promotion-root".into())
        .unwrap();
    let root_claim = kernel
        .claim("executor-terminal-promotion-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child_id = SessionId::new("session-terminal-promotion-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: kernel.agent_caller(&root_claim).unwrap(),
            child_session_id: child_id.clone(),
            task_name: "late-child".into(),
            message_id: MessageId::new("message-terminal-promotion-child").unwrap(),
            message: "finish after the parent's final scan".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel
        .register("executor-terminal-promotion-child".into())
        .unwrap();
    let child_claim = kernel
        .claim(
            "executor-terminal-promotion-child",
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        kernel
            .enter_pending_step_messages(&root_claim)
            .await
            .unwrap(),
        0
    );
    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    let before_terminal = store.read_agent_mailbox(&root_id, None).await.unwrap();
    assert!(before_terminal.pending.iter().any(|entry| {
        matches!(entry.message.source, AgentMessageSource::Completion { .. })
            && entry.target == MessageTarget::NextStep
            && !entry.wake_required
    }));

    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();

    let after_terminal = store.read_agent_mailbox(&root_id, None).await.unwrap();
    assert!(after_terminal.pending.iter().any(|entry| {
        matches!(entry.message.source, AgentMessageSource::Completion { .. })
            && entry.target == MessageTarget::NextTurn
            && entry.wake_required
    }));
    let ready = store.list_ready_messages(&root_id, None, 8).await.unwrap();
    assert_eq!(ready.messages.len(), 1);
    assert_eq!(ready.messages[0].session_id, root_id);
    assert_eq!(ready.messages[0].target, MessageTarget::NextTurn);
    assert!(store.active_activation(&root_id).await.unwrap().is_none());
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One public-seam sequence proves both message and completion winners around a durable park.
async fn agent_wait_persists_park_and_completion_resume_around_descendant_change() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-wait-root").unwrap();
    let root_message = MessageId::new("message-wait-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message(root_message.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let root_turn = TurnId::new("turn-wait-root").unwrap();
    let root_step = StepId::new("step-wait-root").unwrap();
    kernel
        .claim_message(ClaimMessage {
            session: kernel.prepare_resume(&root_id).await.unwrap(),
            message_id: root_message,
            activation_id: ActivationId::new("activation-wait-root").unwrap(),
            path: AgentPath::root(),
            turn_id: root_turn,
            step_id: root_step.clone(),
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-wait-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-wait-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();
    assert!(matches!(
        kernel
            .wait_agent(
                &caller,
                std::time::Duration::from_nanos(1),
                CancellationToken::new(),
            )
            .await,
        Err(TurnError::Invalid(message)) if message.contains("1ms..=1h")
    ));
    let child_id = SessionId::new("session-wait-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: caller.clone(),
            child_session_id: child_id.clone(),
            task_name: "child".into(),
            message_id: MessageId::new("message-wait-child").unwrap(),
            message: "perform child work".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel.register("executor-wait-child".into()).unwrap();
    let child_claim = kernel
        .claim("executor-wait-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    let waiter = tokio::spawn({
        let kernel = kernel.clone();
        let caller = caller.clone();
        async move {
            kernel
                .wait_agent(
                    &caller,
                    std::time::Duration::from_secs(2),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let controls = store.read_controls(&root_id, 0, 64).await.unwrap();
            if controls.records.iter().any(|record| {
                matches!(
                    record.body(),
                    AgentControlRecordBody::WaitParked { step_id, .. }
                        if step_id == &root_step
                )
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("wait must durably park before observing descendants");
    assert_eq!(
        store
            .active_activation(&root_id)
            .await
            .unwrap()
            .unwrap()
            .phase,
        StoreActivationPhase::Parked
    );

    kernel
        .send_agent_message(SendAgentMessage {
            caller: caller.clone(),
            target_session_id: child_id.clone(),
            message_id: MessageId::new("message-wait-inject").unwrap(),
            message: "additional child input".into(),
            start_new_turn: false,
        })
        .await
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), AgentWaitResult::Changed);
    assert_eq!(
        store
            .active_activation(&root_id)
            .await
            .unwrap()
            .unwrap()
            .phase,
        StoreActivationPhase::Running
    );
    let controls = store.read_controls(&root_id, 0, 64).await.unwrap();
    assert!(controls.records.iter().any(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                step_id,
                cause: WaitResumeCause::Message,
                ..
            } if step_id == &root_step
        )
    }));

    let completion_waiter = tokio::spawn({
        let kernel = kernel.clone();
        let caller = caller.clone();
        async move {
            kernel
                .wait_agent(
                    &caller,
                    std::time::Duration::from_secs(2),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let controls = store.read_controls(&root_id, 0, 64).await.unwrap();
            if controls
                .records
                .iter()
                .filter(|record| {
                    matches!(
                        record.body(),
                        AgentControlRecordBody::WaitParked { step_id, .. }
                            if step_id == &root_step
                    )
                })
                .count()
                >= 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second wait must durably park before completion");

    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        completion_waiter.await.unwrap().unwrap(),
        AgentWaitResult::Changed
    );
    let controls = store.read_controls(&root_id, 0, 64).await.unwrap();
    assert!(controls.records.iter().any(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                step_id,
                cause: WaitResumeCause::Completion,
                ..
            } if step_id == &root_step
        )
    }));

    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn agent_wait_timeout_is_durably_resumed_as_timeout() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-wait-timeout-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-wait-timeout-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-wait-timeout-root".into())
        .unwrap();
    let root_claim = kernel
        .claim("executor-wait-timeout-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: caller.clone(),
            child_session_id: SessionId::new("session-wait-timeout-child").unwrap(),
            task_name: "timeout-child".into(),
            message_id: MessageId::new("message-wait-timeout-child").unwrap(),
            message: "remain ready".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();

    assert_eq!(
        kernel
            .wait_agent(
                &caller,
                std::time::Duration::from_millis(1),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        AgentWaitResult::TimedOut
    );
    let controls = store.read_controls(&root_id, 0, 16).await.unwrap();
    assert!(controls.records.iter().any(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                cause: WaitResumeCause::Timeout,
                ..
            }
        )
    }));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn agent_wait_cancellation_is_typed_and_durably_resumed_as_cancel() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-wait-cancel-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-wait-cancel-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-wait-cancel-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-wait-cancel-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: caller.clone(),
            child_session_id: SessionId::new("session-wait-cancel-child").unwrap(),
            task_name: "cancel-child".into(),
            message_id: MessageId::new("message-wait-cancel-child").unwrap(),
            message: "remain ready".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert_eq!(
        kernel
            .wait_agent(&caller, std::time::Duration::from_secs(1), cancellation,)
            .await,
        Err(TurnError::Cancelled)
    );
    let controls = store.read_controls(&root_id, 0, 16).await.unwrap();
    assert!(controls.records.iter().any(|record| matches!(
        record.body(),
        AgentControlRecordBody::WaitResumed {
            cause: WaitResumeCause::Cancel,
            ..
        }
    )));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The 513-record interval is the public regression for paginated completion classification.
async fn wait_completion_cause_scans_beyond_one_control_page() {
    let memory = Arc::new(MemoryStore::new());
    let observed = Arc::new(FactReadRaceStore::new(memory.clone()));
    let service: Arc<dyn SessionStore> = observed.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-long-wait-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-long-wait-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-long-wait-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-long-wait-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let root_caller = kernel.agent_caller(&root_claim).unwrap();
    let child_id = SessionId::new("session-long-wait-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: root_caller.clone(),
            child_session_id: child_id.clone(),
            task_name: "long-wait-child".into(),
            message_id: MessageId::new("message-long-wait-child").unwrap(),
            message: "complete after a long control interval".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel.register("executor-long-wait-child".into()).unwrap();
    let child_claim = kernel
        .claim("executor-long-wait-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    observed.pause_second_next_descendant_snapshot();
    let wait = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .wait_agent(
                    &root_caller,
                    std::time::Duration::from_secs(10),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observed.wait_for_descendant_snapshot_pause(),
    )
    .await
    .expect("wait did not reach its post-park descendant observation");

    let child_fact_seq = memory
        .read_facts(&child_id, 0, 8)
        .await
        .unwrap()
        .durable_seq;
    let child_control_seq = memory
        .read_controls(&child_id, 0, 8)
        .await
        .unwrap()
        .durable_seq;
    let controls = (0..256_u64)
        .flat_map(|offset| {
            let message_id = MessageId::new(format!("message-long-wait-{offset}")).unwrap();
            let accepted_seq = child_control_seq + offset * 2 + 1;
            [
                rsi_agent_session_protocol::AgentControlRecord::new(
                    accepted_seq,
                    50 + offset,
                    AgentControlRecordBody::MessageAccepted {
                        message: AgentMessage {
                            message_id: message_id.clone(),
                            source: AgentMessageSource::Human,
                            content: vec![AgentMessageContent::Text {
                                text: "transient input".into(),
                            }],
                            options: MessageOptions::default(),
                        },
                        root_session_id: root_id.clone(),
                        target: MessageTarget::NextStep,
                        wake_required: false,
                    },
                )
                .unwrap(),
                rsi_agent_session_protocol::AgentControlRecord::new(
                    accepted_seq + 1,
                    50 + offset,
                    AgentControlRecordBody::MessageDiscarded {
                        message_id,
                        reason: rsi_agent_session_protocol::MessageDiscardReason::Cancelled,
                    },
                )
                .unwrap(),
            ]
        })
        .collect::<Vec<_>>();
    memory
        .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
            sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                session_id: child_id.clone(),
                expected_fact_seq: child_fact_seq,
                expected_control_seq: child_control_seq,
                header: None,
                facts: Vec::new(),
                controls,
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        })
        .await
        .unwrap();
    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    observed.release_descendant_snapshot();

    assert_eq!(wait.await.unwrap().unwrap(), AgentWaitResult::Changed);
    let controls = memory.read_controls(&root_id, 0, 16).await.unwrap();
    assert!(controls.records.iter().any(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                cause: WaitResumeCause::Completion,
                ..
            }
        )
    }));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn a_saturated_agent_tree_does_not_starve_an_independent_ready_session() {
    assert_tree_capacity(false).await;
}

#[tokio::test]
async fn direct_turn_claims_respect_the_same_tree_capacity() {
    assert_tree_capacity(true).await;
}

#[allow(clippy::too_many_lines)] // Both claim paths share the same saturated tree and independent-root proof.
async fn assert_tree_capacity(direct: bool) {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let saturated_root = SessionId::new("session-a-saturated-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(saturated_root.as_str())),
            message: mailbox_message("message-saturated-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let root_lease = kernel.register("executor-saturated-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-saturated-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let root_caller = kernel.agent_caller(&root_claim).unwrap();

    let mut child_leases = Vec::new();
    let mut child_claims = Vec::new();
    for index in 0..2 {
        let child_id = SessionId::new(format!("session-a-running-child-{index}")).unwrap();
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller: root_caller.clone(),
                child_session_id: child_id,
                task_name: format!("running-child-{index}"),
                message_id: MessageId::new(format!("message-running-child-{index}")).unwrap(),
                message: "hold one tree lane".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await
            .unwrap();
        let executor = format!("executor-running-child-{index}");
        child_leases.push(kernel.register(executor.clone()).unwrap());
        child_claims.push(
            kernel
                .claim(&executor, CancellationToken::new())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: root_caller,
            child_session_id: SessionId::new("session-a-blocked-child").unwrap(),
            task_name: "blocked-child".into(),
            message_id: MessageId::new("message-blocked-child").unwrap(),
            message: "remain ready behind the per-tree running cap".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();

    if direct {
        let child = SessionId::new("session-a-blocked-child").unwrap();
        kernel
            .cancel_target(
                &child,
                CancelTarget::Message(MessageId::new("message-blocked-child").unwrap()),
                None,
            )
            .await
            .unwrap();
        kernel
            .submit(SubmitTurn {
                session: resume(&kernel, child).await,
                turn_id: TurnId::new("turn-direct-blocked").unwrap(),
                text: "a direct turn must also wait for tree capacity".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
    }

    let independent = SessionId::new("session-z-independent").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(independent.as_str())),
            message: mailbox_message("message-independent"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let independent_lease = kernel.register("executor-independent".into()).unwrap();
    let independent_claim = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        kernel.claim("executor-independent", CancellationToken::new()),
    )
    .await
    .expect("an earlier saturated root must not block another ready Session")
    .unwrap()
    .unwrap();
    assert_eq!(independent_claim.session_id(), &independent);

    kernel
        .finish_activation_turn(&independent_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    for claim in &child_claims {
        kernel
            .finish_activation_turn(claim, &TurnOutcome::Completed)
            .await
            .unwrap()
            .unwrap();
    }
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    drop((root_lease, child_leases, independent_lease));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn one_ready_root_store_failure_does_not_terminate_or_hide_later_work() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    for (session, message) in [
        ("session-a-failing-root", "message-failing-root"),
        ("session-z-healthy-root", "message-healthy-root"),
    ] {
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(session)),
                message: mailbox_message(message),
                target: MessageTarget::NextTurn,
                wake_required: true,
            })
            .await
            .unwrap();
    }
    store.fail_next_agent_tree_read_for(SessionId::new("session-a-failing-root").unwrap());
    let lease = kernel.register("executor-root-isolation".into()).unwrap();
    let healthy = kernel
        .claim("executor-root-isolation", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(healthy.session_id().as_str(), "session-z-healthy-root");
    kernel
        .finish_activation_turn(&healthy, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    let recovered = kernel
        .claim("executor-root-isolation", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.session_id().as_str(), "session-a-failing-root");
    kernel
        .finish_activation_turn(&recovered, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn a_corrupt_ready_index_is_not_silently_reported_as_no_work() {
    let memory = Arc::new(MemoryStore::new());
    let observed = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = observed.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-corrupt-ready-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-corrupt-ready-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    observed.corrupt_next_ready_target();
    let lease = kernel.register("executor-corrupt-ready".into()).unwrap();

    assert!(matches!(
        kernel
            .claim("executor-corrupt-ready", CancellationToken::new())
            .await,
        Err(TurnError::Invariant(message)) if message.contains("ready index")
    ));

    let claim = kernel
        .claim("executor-corrupt-ready", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_activation_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    kernel.shutdown(worker).await.unwrap();
}

async fn active_parent_and_child(
    kernel: &SessionKernel,
) -> (
    rsi_agent_turn_protocol::TurnClaim,
    rsi_agent_turn_protocol::TurnClaim,
    rsi_agent_turn_protocol::ExecutorLease,
) {
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("session-review-parent")),
            message: mailbox_message("message-review-parent"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let lease = kernel.register("executor-review".into()).unwrap();
    let parent = kernel
        .claim("executor-review", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: kernel.agent_caller(&parent).unwrap(),
            child_session_id: SessionId::new("session-review-child").unwrap(),
            task_name: "child".into(),
            message_id: MessageId::new("message-review-child").unwrap(),
            message: "work".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let child = kernel
        .claim("executor-review", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    (parent, child, lease)
}

#[tokio::test]
async fn agent_wait_resumes_for_a_message_to_its_own_mailbox() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let (parent, child, _lease) = active_parent_and_child(&kernel).await;
    let waiter = tokio::spawn({
        let kernel = kernel.clone();
        let caller = kernel.agent_caller(&parent).unwrap();
        async move {
            kernel
                .wait_agent(
                    &caller,
                    std::time::Duration::from_secs(2),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store
            .active_activation(parent.session_id())
            .await
            .unwrap()
            .unwrap()
            .phase
            != StoreActivationPhase::Parked
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parent must durably park");
    kernel
        .send_agent_message(SendAgentMessage {
            caller: kernel.agent_caller(&child).unwrap(),
            target_session_id: parent.session_id().clone(),
            message_id: MessageId::new("message-review-inbox").unwrap(),
            message: "parent, continue".into(),
            start_new_turn: false,
        })
        .await
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), AgentWaitResult::Changed);
    assert!(
        store
            .read_controls(parent.session_id(), 0, 32)
            .await
            .unwrap()
            .records
            .iter()
            .any(|record| matches!(
                record.body(),
                AgentControlRecordBody::WaitResumed {
                    cause: WaitResumeCause::Message,
                    ..
                }
            ))
    );
    assert_eq!(
        kernel.enter_pending_step_messages(&parent).await.unwrap(),
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn a_direct_parent_turn_holds_step_messages_and_completion_wakes_its_next_activation() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let (parent, child, _lease) = active_parent_and_child(&kernel).await;
    kernel
        .finish_activation_turn(&parent, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    kernel
        .submit(SubmitTurn {
            session: resume(&kernel, parent.session_id().clone()).await,
            turn_id: TurnId::new("turn-review-direct").unwrap(),
            text: "direct work".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let direct = kernel
        .claim("executor-review", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .send_agent_message(SendAgentMessage {
            caller: kernel.agent_caller(&child).unwrap(),
            target_session_id: direct.session_id().clone(),
            message_id: MessageId::new("message-review-held").unwrap(),
            message: "hold until an activation Step".into(),
            start_new_turn: false,
        })
        .await
        .unwrap();
    assert_eq!(
        kernel.enter_pending_step_messages(&direct).await.unwrap(),
        0
    );
    kernel
        .finish_activation_turn(&child, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    let mailbox = store
        .read_agent_mailbox(parent.session_id(), None)
        .await
        .unwrap();
    assert!(mailbox.pending.iter().any(|entry| matches!(
        entry.message.source,
        AgentMessageSource::Completion { .. }
    ) && entry.target == MessageTarget::NextTurn
        && entry.wake_required));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn transient_ready_root_enumeration_failure_keeps_the_executor_registered() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("session-root-retry")),
            message: mailbox_message("message-root-retry"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-root-retry".into()).unwrap();
    store.fail_ready_roots.store(true, Ordering::Release);
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        kernel.claim("executor-root-retry", CancellationToken::new()),
    )
    .await
    .unwrap()
    .expect("a transient enumeration error must not terminate claim admission")
    .unwrap();
    assert_eq!(claim.session_id().as_str(), "session-root-retry");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn parked_parent_reacquires_tree_capacity_or_cancels_without_waiting_for_a_lane() {
    for cancel in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let service: Arc<dyn SessionStore> = store.clone();
        let kernel =
            SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = kernel.start_write_behind();
        let (parent, _child, _lease) = active_parent_and_child(&kernel).await;
        store.pause_second_next_descendant_snapshot();
        let cancellation = CancellationToken::new();
        let mut waiter = tokio::spawn({
            let kernel = kernel.clone();
            let caller = kernel.agent_caller(&parent).unwrap();
            let cancellation = cancellation.clone();
            async move {
                kernel
                    .wait_agent(&caller, std::time::Duration::from_secs(20), cancellation)
                    .await
            }
        });
        store.wait_for_descendant_snapshot_pause().await;
        let mut extras = Vec::new();
        for index in 0..2 {
            kernel
                .spawn_agent(SpawnAgentRequest {
                    caller: kernel.agent_caller(&parent).unwrap(),
                    child_session_id: SessionId::new(format!("session-capacity-extra-{index}"))
                        .unwrap(),
                    task_name: format!("extra-{index}"),
                    message_id: MessageId::new(format!("message-extra-{index}")).unwrap(),
                    message: "take the parked parent's capacity".into(),
                    fork_turns: ForkTurnSelection::None,
                })
                .await
                .unwrap();
            extras.push(
                kernel
                    .claim("executor-review", CancellationToken::new())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        store.release_descendant_snapshot();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), &mut waiter)
                .await
                .is_err()
        );
        assert_eq!(
            memory
                .active_activation(parent.session_id())
                .await
                .unwrap()
                .unwrap()
                .phase,
            StoreActivationPhase::Parked
        );
        if cancel {
            cancellation.cancel();
            assert_eq!(waiter.await.unwrap(), Err(TurnError::Cancelled));
        } else {
            kernel
                .finish_activation_turn(&extras[0], &TurnOutcome::Completed)
                .await
                .unwrap();
            assert_eq!(waiter.await.unwrap().unwrap(), AgentWaitResult::Changed);
        }
        kernel.shutdown(worker).await.unwrap();
    }
}
