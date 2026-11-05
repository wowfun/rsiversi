use super::*;
use rsi_agent_session_protocol::MessageDelivery;
use rsi_agent_store_protocol::MailboxRead;

async fn running(
    store: Arc<FactReadRaceStore>,
) -> (
    AgentKernel,
    rsi_agent_kernel::KernelWorkers,
    rsi_agent_turn_protocol::ExecutorLease,
    rsi_agent_turn_protocol::TurnClaim,
) {
    let kernel = AgentKernel::recover_with_clock(store, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("inbox-stop")),
            message: mailbox_message("initial"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let executor = kernel.register("stop-executor".into()).unwrap();
    let claim = kernel
        .claim("stop-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    (kernel, workers, executor, claim)
}

async fn send(kernel: &AgentKernel, id: &str, delivery: MessageDelivery) {
    let session = SessionId::new("inbox-stop").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: resume(kernel, session).await,
            message: mailbox_message(id),
            delivery,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn idle_wait_follows_waking_work_and_ignores_paused_input_without_polling() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let backend: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let store = Arc::new(FactReadRaceStore::new(backend));
        let (kernel, workers, executor, first) = running(store.clone()).await;
        send(&kernel, "replacement", MessageDelivery::NextTurn).await;
        let idle_kernel = kernel.clone();
        let id = first.session_id().clone();
        let reads = store.inbox_control_reads.load(Ordering::Acquire);
        let waiting = tokio::spawn(async move { idle_kernel.wait_idle(&id).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while store.inbox_control_reads.load(Ordering::Acquire) == reads {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!waiting.is_finished());
        let stable = store.inbox_control_reads.load(Ordering::Acquire);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(
            store.inbox_control_reads.load(Ordering::Acquire),
            stable,
            "idle wait has no timer reads"
        );
        kernel
            .finish_turn(&first, &TurnOutcome::Completed)
            .await
            .unwrap();
        let second = kernel
            .claim("stop-executor", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert!(!waiting.is_finished(), "replacement driver is still active");
        send(&kernel, "held", MessageDelivery::NextTurn).await;
        send(&kernel, "injected", MessageDelivery::NextStep).await;
        kernel
            .cancel_target(
                second.session_id(),
                CancelTarget::Session { keep_inbox: true },
                None,
            )
            .await
            .unwrap();
        kernel
            .finish_turn(&second, &TurnOutcome::Cancelled)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let state = store
            .read_inbox_control_state(second.session_id(), true)
            .await
            .unwrap();
        assert!(!state.has_open_turn && !state.has_active_activation && !state.has_waking_message);
        assert!(state.inbox_paused);
        assert_eq!(state.pending_message_ids.len(), 2);
        kernel.wait_idle(second.session_id()).await.unwrap();
        drop(executor);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn idle_wait_terminates_on_kernel_shutdown() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let (kernel, workers, executor, claim) = running(store.clone()).await;
    let idle_kernel = kernel.clone();
    let id = claim.session_id().clone();
    let waiting = tokio::spawn(async move { idle_kernel.wait_idle(&id).await });
    while store.inbox_control_reads.load(Ordering::Acquire) == 0 {
        tokio::task::yield_now().await;
    }
    drop(executor);
    kernel.shutdown(workers).await.unwrap();
    assert!(matches!(
        waiting.await.unwrap(),
        Err(TurnError::ShuttingDown)
    ));
}

#[tokio::test]
async fn inbox_clear_preserves_execution_and_pause_and_reconciles_a_lost_receipt() {
    for sqlite in [false, true] {
        for paused in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let backend: Arc<dyn SessionStore> = if sqlite {
                Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
            } else {
                Arc::new(MemoryStore::new())
            };
            let store = Arc::new(FactReadRaceStore::new(backend));
            let (kernel, workers, executor, claim) = running(store.clone()).await;
            send(&kernel, "held", MessageDelivery::NextTurn).await;
            send(&kernel, "injected", MessageDelivery::NextStep).await;
            if paused {
                kernel
                    .cancel_target(
                        claim.session_id(),
                        CancelTarget::Session { keep_inbox: true },
                        None,
                    )
                    .await
                    .unwrap();
            }
            let before = store.read_watermarks(claim.session_id()).await.unwrap();
            store
                .fail_inbox_stop_after_apply
                .store(true, Ordering::Release);
            assert!(
                kernel
                    .cancel_target(claim.session_id(), CancelTarget::Inbox, None)
                    .await
                    .unwrap()
                    .accepted
            );
            assert_eq!(kernel.cancellation(&claim).unwrap().is_cancelled(), paused);
            let inbox = store
                .read_inbox_control_state(claim.session_id(), true)
                .await
                .unwrap();
            assert_eq!(inbox.inbox_paused, paused);
            assert!(inbox.has_open_turn && inbox.pending_message_ids.is_empty());
            assert_eq!(inbox.durable_fact_seq, before.durable_fact_seq);
            assert_eq!(inbox.durable_control_seq, before.durable_control_seq + 2);
            assert!(
                !kernel
                    .cancel_target(claim.session_id(), CancelTarget::Inbox, None)
                    .await
                    .unwrap()
                    .accepted
            );
            send(&kernel, "later", MessageDelivery::NextTurn).await;
            assert_eq!(
                store
                    .read_inbox_control_state(claim.session_id(), true)
                    .await
                    .unwrap()
                    .pending_message_ids,
                vec![MessageId::new("later").unwrap()]
            );
            kernel
                .finish_turn(
                    &claim,
                    &if paused {
                        TurnOutcome::Cancelled
                    } else {
                        TurnOutcome::Completed
                    },
                )
                .await
                .unwrap();
            drop(executor);
            kernel.shutdown(workers).await.unwrap();
        }
    }
}

#[tokio::test]
async fn session_stop_atomically_cancels_execution_and_parks_or_clears_pending_input() {
    for sqlite in [false, true] {
        for keep_inbox in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let backend: Arc<dyn SessionStore> = if sqlite {
                Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
            } else {
                Arc::new(MemoryStore::new())
            };
            let store = Arc::new(FactReadRaceStore::new(backend));
            let (kernel, workers, executor, claim) = running(store.clone()).await;
            let id = claim.session_id();
            let read_mailbox = || async {
                store
                    .read_agent_mailbox(id, MailboxRead::All)
                    .await
                    .unwrap()
            };
            for (name, delivery) in [
                ("queued", MessageDelivery::NextTurn),
                ("injected", MessageDelivery::NextStep),
                ("steered", MessageDelivery::Steer),
            ] {
                send(&kernel, name, delivery).await;
            }
            let token = kernel.cancellation(&claim).unwrap();
            let before = read_mailbox().await;
            assert_eq!(before.pending.len(), 3);
            store.pause_next_agent_commit_before_apply();
            let mut stop = {
                let kernel = kernel.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    kernel
                        .cancel_target(&id, CancelTarget::Session { keep_inbox }, None)
                        .await
                })
            };
            tokio::select! {
                () = store.wait_until_agent_commit_is_before_apply() => {}
                result = &mut stop => panic!("stop returned before the atomic write: {result:?}"),
            }
            assert!(!token.is_cancelled());
            assert_eq!(read_mailbox().await, before);
            store.release_agent_commit_before_apply();
            assert!(stop.await.unwrap().unwrap().accepted);
            assert!(token.is_cancelled());
            let after = read_mailbox().await;
            assert!(after.inbox_paused);
            if keep_inbox {
                assert_eq!(after.pending, before.pending);
            } else {
                assert!(after.pending.is_empty());
            }
            let tail = store.read_watermarks(id).await.unwrap();
            assert!(
                !kernel
                    .cancel_target(id, CancelTarget::Session { keep_inbox }, None)
                    .await
                    .unwrap()
                    .accepted
            );
            assert_eq!(store.read_watermarks(id).await.unwrap(), tail);
            kernel
                .finish_turn(&claim, &TurnOutcome::Cancelled)
                .await
                .unwrap();
            assert!(
                store
                    .read_inbox_control_state(id, false)
                    .await
                    .unwrap()
                    .inbox_paused
            );
            assert!(
                store
                    .list_ready_roots(None, 16)
                    .await
                    .unwrap()
                    .roots
                    .is_empty()
            );
            send(&kernel, "after-stop", MessageDelivery::Steer).await;
            assert!(
                !store
                    .read_inbox_control_state(id, false)
                    .await
                    .unwrap()
                    .inbox_paused
            );
            let waking = store
                .read_agent_message(id, &MessageId::new("after-stop").unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(waking.target, MessageTarget::NextTurn);
            assert!(waking.bound_turn_id.is_none());
            drop(executor);
            kernel.shutdown(workers).await.unwrap();
        }
    }
}

#[tokio::test]
async fn session_stop_reconciles_lost_ack_in_pages_without_reading_pending_bodies() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let (kernel, workers, executor, claim) = running(store.clone()).await;
    for index in 0..8 {
        send(&kernel, &format!("held-{index}"), MessageDelivery::NextTurn).await;
    }
    store.mailbox_reads.lock().unwrap().clear();
    *store.count_control_reads_for.lock().unwrap() = Some(claim.session_id().clone());
    store.control_read_attempts.store(0, Ordering::Release);
    store
        .fail_inbox_stop_after_apply
        .store(true, Ordering::Release);
    assert!(
        kernel
            .cancel_target(
                claim.session_id(),
                CancelTarget::Session { keep_inbox: false },
                None
            )
            .await
            .unwrap()
            .accepted
    );
    assert!(kernel.cancellation(&claim).unwrap().is_cancelled());
    assert!(store.mailbox_reads.lock().unwrap().is_empty());
    assert_eq!(
        store.control_read_attempts.load(Ordering::Acquire),
        1,
        "all nine controls fit one proof page"
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    drop(executor);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn session_stop_proved_prewrite_failure_preserves_execution_and_can_retry() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let (kernel, workers, executor, claim) = running(store.clone()).await;
    send(&kernel, "held", MessageDelivery::NextTurn).await;
    let before = store
        .read_agent_mailbox(claim.session_id(), MailboxRead::All)
        .await
        .unwrap();
    store
        .fail_inbox_stop_before_apply
        .store(true, Ordering::Release);
    assert!(
        kernel
            .cancel_target(
                claim.session_id(),
                CancelTarget::Session { keep_inbox: true },
                None
            )
            .await
            .is_err()
    );
    assert!(!kernel.cancellation(&claim).unwrap().is_cancelled());
    assert_eq!(
        store
            .read_agent_mailbox(claim.session_id(), MailboxRead::All)
            .await
            .unwrap(),
        before
    );
    assert!(
        kernel
            .cancel_target(
                claim.session_id(),
                CancelTarget::Session { keep_inbox: true },
                None
            )
            .await
            .unwrap()
            .accepted
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    drop(executor);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn session_stop_unknown_commit_quarantines_the_fact_cursor_until_recovery() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let (kernel, workers, executor, claim) = running(store.clone()).await;
    store
        .fail_inbox_stop_after_apply
        .store(true, Ordering::Release);
    *store.control_read_error.lock().unwrap() = Some(StoreError::Io("proof unavailable".into()));
    assert!(
        kernel
            .cancel_target(
                claim.session_id(),
                CancelTarget::Session { keep_inbox: true },
                None
            )
            .await
            .is_err()
    );
    assert!(kernel.cancellation(&claim).unwrap().is_cancelled());
    let durable = store.read_watermarks(claim.session_id()).await.unwrap();
    assert!(
        kernel
            .cancel_target(
                claim.session_id(),
                CancelTarget::Session { keep_inbox: true },
                None
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.read_watermarks(claim.session_id()).await.unwrap(),
        durable
    );
    let finish = kernel.finish_turn(&claim, &TurnOutcome::Cancelled).await;
    assert!(matches!(finish, Err(TurnError::Flush(_))), "{finish:?}");
    drop(executor);
    assert!(kernel.shutdown(workers).await.is_err());
    let cold = AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    assert_eq!(
        cold.outcome(claim.session_id(), claim.turn_id())
            .await
            .unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    assert!(
        store
            .read_inbox_control_state(claim.session_id(), false)
            .await
            .unwrap()
            .inbox_paused
    );
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn session_stop_survives_caller_detachment_and_new_wake_targets_the_next_turn() {
    for keep_inbox in [false, true] {
        let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
        let (kernel, workers, executor, claim) = running(store.clone()).await;
        send(&kernel, "held", MessageDelivery::NextTurn).await;
        store.pause_next_agent_commit_after_apply();
        let mut waiter = {
            let kernel = kernel.clone();
            let id = claim.session_id().clone();
            tokio::spawn(async move {
                kernel
                    .cancel_target(&id, CancelTarget::Session { keep_inbox }, None)
                    .await
            })
        };
        tokio::select! {
            () = store.wait_until_agent_commit_is_applied() => {}
            result = &mut waiter => panic!("stop returned before its write acknowledgement: {result:?}"),
        }
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        store.release_applied_agent_commit();
        // Submission admission waits for the detached stop to install its cancellation.
        send(&kernel, "new-wake", MessageDelivery::Steer).await;
        assert!(kernel.cancellation(&claim).unwrap().is_cancelled());
        let mailbox = store
            .read_agent_mailbox(claim.session_id(), MailboxRead::All)
            .await
            .unwrap();
        assert!(!mailbox.inbox_paused);
        assert_eq!(mailbox.pending.len(), if keep_inbox { 2 } else { 1 });
        let waking = store
            .read_agent_message(claim.session_id(), &MessageId::new("new-wake").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(waking.target, MessageTarget::NextTurn);
        assert!(waking.bound_turn_id.is_none());
        kernel
            .finish_turn(&claim, &TurnOutcome::Cancelled)
            .await
            .unwrap();
        assert!(
            !store
                .read_inbox_control_state(claim.session_id(), false)
                .await
                .unwrap()
                .inbox_paused
        );
        drop(executor);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn session_stop_can_clear_an_idle_inbox_when_its_composition_is_unavailable() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let initial =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = initial.start_workers();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header("idle-stop")),
            message: mailbox_message("unclaimed"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    initial.shutdown(workers).await.unwrap();
    let cold = AgentKernel::recover_with_clock(
        store.clone(),
        Arc::new(UnboundedDiagnosticComposition),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let id = SessionId::new("idle-stop").unwrap();
    store.mailbox_reads.lock().unwrap().clear();
    store.reset_header_read_attempts();
    assert!(
        cold.cancel_target(&id, CancelTarget::Session { keep_inbox: true }, None)
            .await
            .unwrap()
            .accepted
    );
    cold.wait_idle(&id).await.unwrap();
    assert_eq!(
        store.header_read_attempts(),
        0,
        "retaining an idle inbox needs no Header or composition"
    );
    assert!(
        cold.cancel_target(&id, CancelTarget::Session { keep_inbox: false }, None)
            .await
            .unwrap()
            .accepted
    );
    assert!(store.mailbox_reads.lock().unwrap().is_empty());
    let inbox = store.read_inbox_control_state(&id, true).await.unwrap();
    assert!(inbox.inbox_paused && inbox.pending_message_ids.is_empty());
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn session_stop_captures_all_direct_turns_but_not_a_later_submission() {
    async fn next(kernel: &AgentKernel) -> rsi_agent_turn_protocol::SubmittedTurn {
        kernel
            .submit(SubmitTurn {
                reasoning_effort: None,
                turn_id: client_turn_id(),
                session: resume(kernel, SessionId::new("direct-stop").unwrap()).await,
                text: "another direct turn".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap()
    }
    let store = Arc::new(MemoryStore::new());
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let mut accepted = vec![submit(&kernel, "direct-stop", "initial direct turn").await];
    for _ in 0..2 {
        accepted.push(next(&kernel).await);
    }
    let id = &accepted[0].session_id;
    assert!(
        kernel
            .cancel_target(id, CancelTarget::Session { keep_inbox: false }, None)
            .await
            .unwrap()
            .accepted
    );
    let later = next(&kernel).await;
    let page = store
        .read_facts(
            id,
            0,
            32,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    let cancelled = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::CancelRequested { turn_id, .. } => Some(turn_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cancelled,
        accepted
            .iter()
            .map(|turn| &turn.turn_id)
            .collect::<Vec<_>>()
    );
    assert!(!cancelled.contains(&&later.turn_id));
    kernel.shutdown(workers).await.unwrap();
}
