use super::*;
use rsi_agent_session_protocol::AgentControlRecord;
use rsi_agent_session_protocol::{
    MessageDelivery, QueueMutation, QueueMutationOutcome as Outcome,
    QueueMutationRejection as Rejection, QueueMutationRequest, QueueOperationId, QueueSlotId,
};
use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};

fn edit(
    operation: &str,
    slot: &str,
    expected: &str,
    mutation: QueueMutation,
) -> QueueMutationRequest {
    QueueMutationRequest {
        operation_id: QueueOperationId::new(operation).unwrap(),
        slot_id: QueueSlotId::new(slot).unwrap(),
        expected_message_id: MessageId::new(expected).unwrap(),
        mutation,
    }
}
fn replace(id: &str) -> QueueMutation {
    QueueMutation::Replace {
        new_message_id: MessageId::new(id).unwrap(),
        content: vec![AgentMessageContent::Text {
            text: format!("replacement {id}"),
        }],
    }
}
async fn create(kernel: &AgentKernel, session: &SessionId, id: &str) {
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session.as_str())),
            message: mailbox_message(id),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
}
async fn submit(kernel: &AgentKernel, session: &SessionId, id: &str, delivery: MessageDelivery) {
    kernel
        .submit_message(SubmitMessage {
            session: resume(kernel, session.clone()).await,
            message: mailbox_message(id),
            delivery,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn queue_concurrent_operation_ids_serialize_through_cancelled_commit_waiters() {
    for cancel_waiter in [false, true] {
        for change_request in [false, true] {
            let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
            let kernel =
                AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                    .await
                    .unwrap();
            let worker = kernel.start_workers();
            let session = SessionId::new("queue-concurrent-operation").unwrap();
            create(&kernel, &session, "initial").await;
            let request = edit("operation", "initial", "initial", replace("successor"));
            store.pause_next_agent_commit_before_apply();
            let first = {
                let (kernel, session, request) = (kernel.clone(), session.clone(), request.clone());
                tokio::spawn(async move { kernel.mutate_queue(&session, request, None).await })
            };
            store.wait_until_agent_commit_is_before_apply().await;
            let first = if cancel_waiter {
                first.abort();
                assert!(first.await.unwrap_err().is_cancelled());
                None
            } else {
                Some(first)
            };
            let mut retry = request.clone();
            if change_request {
                retry.mutation = replace("different-successor");
            }
            let mut second = Box::pin(kernel.mutate_queue(&session, retry, None));
            // Poll the competitor while the original Store commit is definitely unapplied.
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            assert_eq!(store.queue_mutation_count(&session).await.unwrap(), 0);
            store.release_agent_commit_before_apply();
            let second_result = second.await;
            let receipt = kernel
                .queue_mutation_status(&session, &request.operation_id)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(receipt.outcome, Outcome::Replaced { .. }));
            if change_request {
                assert!(matches!(
                    second_result,
                    Err(TurnError::QueueOperationConflict)
                ));
            } else {
                assert_eq!(second_result.unwrap(), receipt);
            }
            if let Some(first) = first {
                assert_eq!(first.await.unwrap().unwrap(), receipt);
            }
            assert_eq!(store.queue_mutation_count(&session).await.unwrap(), 1);
            assert_eq!(
                store
                    .read_watermarks(&session)
                    .await
                    .unwrap()
                    .durable_control_seq,
                4
            );
            let ready = store.list_ready_messages(&session, None, 64).await.unwrap();
            assert_eq!(ready.messages.len(), 1);
            assert_eq!(ready.messages[0].message_id.as_str(), "successor");
            store.validate_session(&session).await.unwrap();
            kernel.shutdown(worker).await.unwrap();
        }
    }
}

#[tokio::test]
async fn queue_successor_collision_with_submit_observes_both_commit_orders() {
    for queue_first in [false, true] {
        let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = kernel.start_workers();
        let session = SessionId::new("queue-submit-collision").unwrap();
        create(&kernel, &session, "initial").await;
        let request = edit("operation", "initial", "initial", replace("successor"));
        let submission = SubmitMessage {
            session: resume(&kernel, session.clone()).await,
            message: mailbox_message("successor"),
            delivery: MessageDelivery::NextTurn,
        };
        store.pause_next_agent_commit_before_apply();
        let (mutation, submitted) = if queue_first {
            let first = {
                let (kernel, session, request) = (kernel.clone(), session.clone(), request.clone());
                tokio::spawn(async move { kernel.mutate_queue(&session, request, None).await })
            };
            store.wait_until_agent_commit_is_before_apply().await;
            let mut second = Box::pin(kernel.submit_message(submission));
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            store.release_agent_commit_before_apply();
            (first.await.unwrap().unwrap(), second.await)
        } else {
            let first = {
                let kernel = kernel.clone();
                tokio::spawn(async move { kernel.submit_message(submission).await })
            };
            store.wait_until_agent_commit_is_before_apply().await;
            let mut second = Box::pin(kernel.mutate_queue(&session, request.clone(), None));
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            store.release_agent_commit_before_apply();
            (second.await.unwrap(), first.await.unwrap())
        };
        let ready = store.list_ready_messages(&session, None, 64).await.unwrap();
        if queue_first {
            assert!(matches!(mutation.outcome, Outcome::Replaced { .. }));
            assert!(matches!(submitted, Err(TurnError::MessageConflict { .. })));
            assert_eq!(ready.messages.len(), 1);
        } else {
            assert!(matches!(
                mutation.outcome,
                Outcome::Rejected {
                    reason: Rejection::MessageConflict,
                    ..
                }
            ));
            submitted.unwrap();
            assert_eq!(ready.messages.len(), 2);
            assert_eq!(ready.messages[0].message_id.as_str(), "initial");
        }
        assert_eq!(
            ready.messages.last().unwrap().message_id.as_str(),
            "successor"
        );
        assert_eq!(
            kernel.mutate_queue(&session, request, None).await.unwrap(),
            mutation
        );
        assert_eq!(store.queue_mutation_count(&session).await.unwrap(), 1);
        store.validate_session(&session).await.unwrap();
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle verifies durable edits and recovery against both Store adapters.
async fn queue_replace_preserves_position_old_identity_and_receipts_across_restart() {
    for sqlite in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temporary.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = kernel.start_workers();
        let session = SessionId::new("queue-edit").unwrap();
        create(&kernel, &session, "initial").await;
        for index in 1..64 {
            submit(
                &kernel,
                &session,
                &format!("pending-{index}"),
                MessageDelivery::NextTurn,
            )
            .await;
        }
        let before = store.read_controls(&session, 0, 64).await.unwrap();
        let request = edit("edit-first", "initial", "initial", replace("successor"));
        let receipt = kernel
            .mutate_queue(&session, request.clone(), None)
            .await
            .unwrap();
        assert!(matches!(receipt.outcome, Outcome::Replaced { .. }));
        assert_eq!(
            kernel
                .mutate_queue(&session, request.clone(), None)
                .await
                .unwrap(),
            receipt
        );
        let mut different = request.clone();
        different.mutation = replace("different");
        assert!(matches!(
            kernel.mutate_queue(&session, different, None).await,
            Err(TurnError::QueueOperationConflict)
        ));
        let old_cancel = kernel
            .cancel_target(
                &session,
                CancelTarget::Message(MessageId::new("initial").unwrap()),
                None,
            )
            .await
            .unwrap();
        assert!(!old_cancel.accepted);
        assert!(old_cancel.already_terminal);
        assert_eq!(
            kernel
                .message_status(&session, &MessageId::new("successor").unwrap())
                .await
                .unwrap()
                .state,
            MessageState::Pending
        );
        let ready = store.list_ready_messages(&session, None, 64).await.unwrap();
        assert_eq!(ready.messages.len(), 64);
        assert_eq!(ready.messages[0].message_id.as_str(), "successor");
        assert_eq!(ready.messages[0].control_seq, 1);
        assert_eq!(
            store.read_controls(&session, 0, 64).await.unwrap().records,
            before.records
        );
        let stale = kernel
            .mutate_queue(
                &session,
                edit("stale", "initial", "initial", QueueMutation::Withdraw),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            stale.outcome,
            Outcome::Rejected {
                reason: Rejection::StaleMessage,
                ..
            }
        ));
        kernel.shutdown(worker).await.unwrap();
        drop(kernel);
        let restarted =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = restarted.start_workers();
        assert_eq!(
            restarted
                .queue_mutation_status(&session, &request.operation_id)
                .await
                .unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(
            restarted
                .mutate_queue(&session, request, None)
                .await
                .unwrap(),
            receipt
        );
        let withdrawn = restarted
            .mutate_queue(
                &session,
                edit("withdraw", "initial", "successor", QueueMutation::Withdraw),
                None,
            )
            .await
            .unwrap();
        assert_eq!(withdrawn.outcome, Outcome::Withdrawn);
        assert_eq!(
            store
                .list_ready_messages(&session, None, 64)
                .await
                .unwrap()
                .messages
                .len(),
            63
        );
        restarted.shutdown(worker).await.unwrap();
        drop(restarted);
        drop(store);
        if sqlite {
            // Cancellation can leave a dispatched SQLite reader owning the writer lease.
            // Wait only for that documented resource lifetime, never suppress corruption.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    match rsi_agent_store_sqlite::SqliteStore::verify(temporary.path()) {
                        Err(StoreError::WriterLocked) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        result => break result.unwrap(),
                    }
                }
            })
            .await
            .expect("cancelled SQLite reader did not release its writer lease");
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle verifies durable edits and recovery against both Store adapters.
async fn queue_conversion_requires_exact_turn_and_promotes_in_original_order() {
    for sqlite in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temporary.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = kernel.start_workers();
        let session = SessionId::new("queue-convert").unwrap();
        create(&kernel, &session, "running").await;
        let lease = kernel.register("queue-executor".into()).unwrap();
        let claim = kernel
            .claim("queue-executor", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        submit(&kernel, &session, "before", MessageDelivery::NextTurn).await;
        submit(&kernel, &session, "convert", MessageDelivery::NextTurn).await;
        submit(&kernel, &session, "after", MessageDelivery::NextTurn).await;
        let original = store
            .read_agent_message(&session, &MessageId::new("convert").unwrap())
            .await
            .unwrap()
            .unwrap();
        let stale = edit(
            "wrong-turn",
            "convert",
            "convert",
            QueueMutation::ConvertToSteer {
                new_message_id: MessageId::new("stale-successor").unwrap(),
                expected_turn_id: TurnId::new("unseen").unwrap(),
            },
        );
        let rejected = kernel
            .mutate_queue(&session, stale.clone(), None)
            .await
            .unwrap();
        assert!(matches!(
            rejected.outcome,
            Outcome::Rejected {
                reason: Rejection::StaleTurn,
                ..
            }
        ));
        assert_eq!(
            kernel.mutate_queue(&session, stale, None).await.unwrap(),
            rejected
        );
        let converted = kernel
            .mutate_queue(
                &session,
                edit(
                    "convert-now",
                    "convert",
                    "convert",
                    QueueMutation::ConvertToSteer {
                        new_message_id: MessageId::new("steering").unwrap(),
                        expected_turn_id: claim.turn_id().clone(),
                    },
                ),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(converted.outcome, Outcome::Converted { .. }));
        let steering = store
            .read_agent_message(&session, &MessageId::new("steering").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(steering.queue_slot, original.queue_slot);
        assert_eq!(steering.message.content, original.message.content);
        assert_eq!(steering.target, MessageTarget::NextStep);
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(lease);
        let ready = store.list_ready_messages(&session, None, 8).await.unwrap();
        assert_eq!(
            ready
                .messages
                .iter()
                .map(|entry| entry.message_id.as_str())
                .collect::<Vec<_>>(),
            ["before", "steering", "after"]
        );
        assert_eq!(ready.messages[1].control_seq, original.accepted_control_seq);
        // Editing an already-promoted steer retains waking order and does not bind to a later Turn.
        kernel
            .mutate_queue(
                &session,
                edit(
                    "edit-promoted",
                    "convert",
                    "steering",
                    replace("promoted-edit"),
                ),
                None,
            )
            .await
            .unwrap();
        let promoted = store
            .read_queue_slot(&session, &QueueSlotId::new("convert").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(promoted.target, MessageTarget::NextTurn);
        assert_eq!(promoted.delivery, MessageDelivery::Steer);
        assert!(promoted.bound_turn_id.is_none());
        assert_eq!(promoted.queue_slot, original.queue_slot);
        kernel.shutdown(worker).await.unwrap();
        drop(kernel);
        drop(store);
        if sqlite {
            // Cancellation can leave a dispatched SQLite reader owning the writer lease.
            // Wait only for that documented resource lifetime, never suppress corruption.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    match rsi_agent_store_sqlite::SqliteStore::verify(temporary.path()) {
                        Err(StoreError::WriterLocked) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        result => break result.unwrap(),
                    }
                }
            })
            .await
            .expect("cancelled SQLite reader did not release its writer lease");
        }
    }
}

#[tokio::test]
async fn queue_withdraw_and_replace_serialize_with_next_step_claims_without_cancelling_turn() {
    for mutation in [QueueMutation::Withdraw, replace("edited-step")] {
        let store = Arc::new(MemoryStore::new());
        let kernel = kernel(store.clone()).await;
        let worker = kernel.start_workers();
        let session = SessionId::new("queue-race-step").unwrap();
        create(&kernel, &session, "running").await;
        let _lease = kernel.register("queue-executor".into()).unwrap();
        let claim = kernel
            .claim("queue-executor", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        submit(&kernel, &session, "step", MessageDelivery::Steer).await;
        let request = edit("race", "step", "step", mutation);
        let (edited, claimed) = tokio::join!(
            kernel.mutate_queue(&session, request, None),
            kernel.enter_pending_step_messages(&claim)
        );
        let receipt = edited.unwrap();
        let count = claimed.unwrap();
        match receipt.outcome {
            Outcome::Rejected {
                reason: Rejection::Claimed,
                ..
            }
            | Outcome::Replaced { .. } => assert_eq!(count, 1),
            Outcome::Withdrawn => assert_eq!(count, 0),
            other => panic!("unexpected race result: {other:?}"),
        }
        let facts = store.read_facts(&session, 0, 64).await.unwrap();
        assert!(
            !facts
                .facts
                .iter()
                .any(|fact| matches!(fact.body(), SessionFactBody::CancelRequested { .. }))
        );
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
async fn queue_edits_serialize_with_next_turn_claims() {
    for sqlite in [false, true] {
        for mutation in [QueueMutation::Withdraw, replace("next-successor")] {
            let temporary = tempfile::tempdir().unwrap();
            let store: Arc<dyn SessionStore> = if sqlite {
                Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temporary.path()).unwrap())
            } else {
                Arc::new(MemoryStore::new())
            };
            let kernel =
                AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                    .await
                    .unwrap();
            let worker = kernel.start_workers();
            let session = SessionId::new("queue-next-race").unwrap();
            create(&kernel, &session, "next").await;
            let claim = ClaimMessage {
                session: kernel.prepare_resume(&session).await.unwrap(),
                message_id: MessageId::new("next").unwrap(),
                activation_id: ActivationId::new("next-activation").unwrap(),
                path: AgentPath::root(),
                turn_id: TurnId::new("next-turn").unwrap(),
                step_id: StepId::new("next-step").unwrap(),
            };
            let (edited, claimed) = tokio::join!(
                kernel.mutate_queue(&session, edit("race-next", "next", "next", mutation), None),
                kernel.claim_message(claim)
            );
            match edited.unwrap().outcome {
                Outcome::Rejected {
                    reason: Rejection::Claimed,
                    ..
                } => {
                    claimed.unwrap();
                }
                Outcome::Withdrawn | Outcome::Replaced { .. } => assert!(claimed.is_err()),
                other => panic!("unexpected next-Turn race: {other:?}"),
            }
            assert!(
                !store
                    .read_facts(&session, 0, 64)
                    .await
                    .unwrap()
                    .facts
                    .iter()
                    .any(|fact| matches!(fact.body(), SessionFactBody::CancelRequested { .. }))
            );
            kernel.shutdown(worker).await.unwrap();
        }
    }
}

#[tokio::test]
async fn queue_edits_and_receipt_replay_survive_many_rejections_and_recovery() {
    const OPERATIONS: usize = 4100;
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let session = SessionId::new("queue-receipt-cap").unwrap();
    create(&kernel, &session, "first").await;
    let request = edit("old", "first", "first", replace("second"));
    let saved = kernel
        .mutate_queue(&session, request.clone(), None)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
    let mut seq = saved.control_seq;
    for start in (1..OPERATIONS).step_by(256) {
        let expected = seq;
        let mut controls = Vec::new();
        for index in start..(start + 256).min(OPERATIONS) {
            seq += 1;
            let mut receipt = saved.clone();
            receipt.operation_id = QueueOperationId::new(format!("filled-{index}")).unwrap();
            receipt.control_seq = seq;
            receipt.outcome = Outcome::Rejected {
                reason: Rejection::MissingSlot,
                current_message_id: None,
            };
            controls.push(
                AgentControlRecord::new(
                    seq,
                    42,
                    AgentControlRecordBody::QueueMutationRecorded { receipt },
                )
                .unwrap(),
            );
        }
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: expected,
                    header: None,
                    facts: vec![],
                    controls,
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
    }
    let restarted =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = restarted.start_workers();
    assert_eq!(
        restarted
            .mutate_queue(&session, request, None)
            .await
            .unwrap(),
        saved
    );
    let new = edit(
        "after-rejections",
        "first",
        "second",
        QueueMutation::Withdraw,
    );
    let withdrawn = restarted
        .mutate_queue(&session, new.clone(), None)
        .await
        .unwrap();
    assert_eq!(withdrawn.outcome, Outcome::Withdrawn);
    assert_eq!(
        restarted
            .queue_mutation_status(&session, &new.operation_id)
            .await
            .unwrap(),
        Some(withdrawn)
    );
    assert!(matches!(
        restarted
            .message_status(&session, &MessageId::new("second").unwrap())
            .await
            .unwrap()
            .state,
        MessageState::Discarded { .. }
    ));
    restarted.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle verifies durable edits and recovery against both Store adapters.
async fn queue_edits_preserve_tree_ready_order_and_pinned_fork_prefix() {
    #[derive(Debug)]
    struct Tick(std::sync::atomic::AtomicU64);
    impl Clock for Tick {
        fn now_ms(&self) -> u64 {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        }
    }
    for sqlite in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temporary.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let kernel = AgentKernel::recover_with_clock(
            store.clone(),
            composition(),
            Arc::new(Tick(std::sync::atomic::AtomicU64::new(100))),
        )
        .await
        .unwrap();
        let worker = kernel.start_workers();
        let root = SessionId::new("queue-tree-root").unwrap();
        create(&kernel, &root, "first-turn").await;
        let _lease = kernel.register("tree".into()).unwrap();
        let first = kernel
            .claim("tree", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        kernel
            .finish_turn(&first, &TurnOutcome::Completed)
            .await
            .unwrap();
        submit(&kernel, &root, "second-turn", MessageDelivery::NextTurn).await;
        let running = kernel
            .claim("tree", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        submit(&kernel, &root, "old-position", MessageDelivery::NextTurn).await;
        let child = SessionId::new("queue-tree-child").unwrap();
        kernel
            .spawn_agent(SpawnAgentRequest {
                output_contract: None,
                role: None,
                model: None,
                reasoning_effort: None,
                cancellation: CancellationToken::new(),
                caller: control_tool_caller(&kernel, &running).await,
                child_session_id: child.clone(),
                task_name: "queue-child".into(),
                message_id: MessageId::new("agent-input").unwrap(),
                message: "child work".into(),
                fork_turns: ForkTurnSelection::All,
            })
            .await
            .unwrap();
        let prefix = store.read_controls(&root, 0, 64).await.unwrap();
        let ready_before = store
            .list_ready_messages(&root, None, 64)
            .await
            .unwrap()
            .messages;
        assert_eq!(
            ready_before
                .iter()
                .map(|entry| entry.message_id.as_str())
                .collect::<Vec<_>>(),
            ["old-position", "agent-input"]
        );
        let refused = kernel
            .mutate_queue(
                &child,
                edit(
                    "non-human",
                    "agent-input",
                    "agent-input",
                    QueueMutation::Withdraw,
                ),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            refused.outcome,
            Outcome::Rejected {
                reason: Rejection::NotHuman,
                ..
            }
        ));
        kernel
            .mutate_queue(
                &root,
                edit(
                    "tree-edit",
                    "old-position",
                    "old-position",
                    replace("new-position"),
                ),
                None,
            )
            .await
            .unwrap();
        let after = store
            .list_ready_messages(&root, None, 64)
            .await
            .unwrap()
            .messages;
        assert_eq!(
            after
                .iter()
                .map(|entry| entry.message_id.as_str())
                .collect::<Vec<_>>(),
            ["new-position", "agent-input"]
        );
        assert_eq!(
            (after[0].timestamp_ms, after[0].control_seq),
            (ready_before[0].timestamp_ms, ready_before[0].control_seq)
        );
        assert_eq!(
            store
                .read_controls(&root, 0, prefix.records.len())
                .await
                .unwrap()
                .records,
            prefix.records
        );
        let _child_lease = kernel.register("tree-child".into()).unwrap();
        let child_claim = kernel
            .claim("tree-child", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child_claim.session_id(), &child);
        let inherited = kernel
            .read_fork_facts(&child_claim, 0, 64)
            .await
            .unwrap()
            .unwrap();
        assert!(!inherited.facts.is_empty());
        kernel
            .finish_turn(&child_claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel
            .finish_turn(&running, &TurnOutcome::Completed)
            .await
            .unwrap();
        store.validate_session(&child).await.unwrap();
        kernel.shutdown(worker).await.unwrap();
        drop(kernel);
        drop(store);
        if sqlite {
            // Cancellation can leave a dispatched SQLite reader owning the writer lease.
            // Wait only for that documented resource lifetime, never suppress corruption.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    match rsi_agent_store_sqlite::SqliteStore::verify(temporary.path()) {
                        Err(StoreError::WriterLocked) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        result => break result.unwrap(),
                    }
                }
            })
            .await
            .expect("cancelled SQLite reader did not release its writer lease");
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle verifies durable edits and recovery against both Store adapters.
async fn queue_conversion_rejects_overrides_and_recovers_unclaimed_successor() {
    for sqlite in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temporary.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let initial =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = initial.start_workers();
        let session = SessionId::new("queue-convert-recovery").unwrap();
        create(&initial, &session, "running").await;
        let lease = initial.register("recovery".into()).unwrap();
        let claim = initial
            .claim("recovery", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let mut overridden = mailbox_message("overridden");
        overridden.options.model = Some(rsi_ai_protocol::ModelRef::new("test", "model").unwrap());
        initial
            .submit_message(SubmitMessage {
                session: resume(&initial, session.clone()).await,
                message: overridden,
                delivery: MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let refused = initial
            .mutate_queue(
                &session,
                edit(
                    "override",
                    "overridden",
                    "overridden",
                    QueueMutation::ConvertToSteer {
                        new_message_id: MessageId::new("forbidden").unwrap(),
                        expected_turn_id: claim.turn_id().clone(),
                    },
                ),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            refused.outcome,
            Outcome::Rejected {
                reason: Rejection::IncompatibleOptions,
                ..
            }
        ));
        submit(&initial, &session, "convert", MessageDelivery::NextTurn).await;
        let before = store
            .read_agent_message(&session, &MessageId::new("convert").unwrap())
            .await
            .unwrap()
            .unwrap();
        let request = edit(
            "recover-convert",
            "convert",
            "convert",
            QueueMutation::ConvertToSteer {
                new_message_id: MessageId::new("converted").unwrap(),
                expected_turn_id: claim.turn_id().clone(),
            },
        );
        let receipt = initial
            .mutate_queue(&session, request.clone(), None)
            .await
            .unwrap();
        initial.shutdown(worker).await.unwrap();
        drop(lease);
        drop(initial);
        let restarted =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = restarted.start_workers();
        let current = store
            .read_queue_slot(&session, &QueueSlotId::new("convert").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.queue_slot, before.queue_slot);
        assert_eq!(current.target, MessageTarget::NextTurn);
        assert_eq!(current.delivery, MessageDelivery::Steer);
        let stale = restarted
            .mutate_queue(
                &session,
                edit(
                    "cold-old-turn",
                    "convert",
                    current.message.message_id.as_str(),
                    QueueMutation::ConvertToSteer {
                        new_message_id: MessageId::new("cold-forbidden").unwrap(),
                        expected_turn_id: claim.turn_id().clone(),
                    },
                ),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            stale.outcome,
            Outcome::Rejected {
                reason: Rejection::StaleTurn,
                ..
            }
        ));
        assert!(
            store
                .read_turn_boundary(&session, claim.turn_id())
                .await
                .unwrap()
                .terminal()
                .is_some()
        );
        assert_eq!(
            store
                .read_queue_slot(&session, &QueueSlotId::new("convert").unwrap())
                .await
                .unwrap()
                .unwrap(),
            current
        );
        assert_eq!(
            restarted
                .mutate_queue(&session, request, None)
                .await
                .unwrap(),
            receipt
        );
        restarted.shutdown(worker).await.unwrap();
        drop(restarted);
        drop(store);
        if sqlite {
            // Cancellation can leave a dispatched SQLite reader owning the writer lease.
            // Wait only for that documented resource lifetime, never suppress corruption.
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    match rsi_agent_store_sqlite::SqliteStore::verify(temporary.path()) {
                        Err(StoreError::WriterLocked) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        result => break result.unwrap(),
                    }
                }
            })
            .await
            .expect("cancelled SQLite reader did not release its writer lease");
        }
    }
}
