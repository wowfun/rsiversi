use super::*;

async fn waiting_root(
    kernel: &AgentKernel,
    index: usize,
) -> (
    SessionId,
    rsi_agent_turn_protocol::TurnClaim,
    [rsi_agent_turn_protocol::ExecutorLease; 2],
) {
    let root = SessionId::new(format!("isolated-root-{index:03}")).unwrap();
    let root_executor = format!("isolated-root-executor-{index}");
    let child_executor = format!("isolated-child-executor-{index}");
    let root_lease = kernel.register(root_executor.clone()).unwrap();
    let child_lease = kernel.register(child_executor.clone()).unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root.as_str())),
            message: mailbox_message(&format!("isolated-root-message-{index}")),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let root_claim = kernel
        .claim(&root_executor, CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(root_claim.session_id(), &root);
    kernel
        .spawn_agent(SpawnAgentRequest {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&root_claim).unwrap(),
            child_session_id: SessionId::new(format!("isolated-child-{index:03}")).unwrap(),
            task_name: "child".into(),
            message_id: MessageId::new(format!("isolated-child-message-{index}")).unwrap(),
            message: "hold ancestor waiting".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let child_claim = kernel
        .claim(&child_executor, CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    (root, child_claim, [root_lease, child_lease])
}

#[tokio::test(start_paused = true)]
async fn persistent_first_root_failure_preserves_flush_later_pages_and_global_health() {
    let memory = Arc::new(MemoryStore::new());
    let observed = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel =
        AgentKernel::recover_with_clock(observed.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let mut roots = Vec::new();
    for index in 0..18 {
        roots.push(waiting_root(&kernel, index).await);
    }
    *observed.failed_subtree.lock().unwrap() = Some(roots[0].0.clone());
    observed.fail_waiting_pages.store(true, Ordering::Release);
    for (_, claim, _) in &roots {
        kernel
            .finish_activation_turn(claim, &TurnOutcome::Completed)
            .await
            .unwrap()
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while kernel.settlement_health().global_error.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    observed.fail_waiting_pages.store(false, Ordering::Release);
    wait_for_settlement(memory.as_ref(), &roots[17].0).await;
    for (root, _, _) in roots.iter().skip(1) {
        wait_for_settlement(memory.as_ref(), root).await;
    }
    assert!(
        memory
            .active_activation(&roots[0].0)
            .await
            .unwrap()
            .is_some()
    );
    let health = kernel.settlement_health();
    assert!(
        health.global_error.is_some(),
        "a successful page hid a failed scan"
    );
    assert!(
        health
            .recent_errors
            .iter()
            .any(|error| error.session_id == roots[0].0)
    );

    let ordinary = submit(
        &kernel,
        "settlement-independent-flush",
        "flush remains live",
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !memory
            .read_facts(&ordinary.session_id, 0, 8)
            .await
            .is_ok_and(|page| page.durable_seq == 1)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        memory
            .read_facts(&ordinary.session_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        1
    );
    *observed.failed_subtree.lock().unwrap() = None;
    wait_for_settlement(memory.as_ref(), &roots[0].0).await;
    tokio::time::timeout(std::time::Duration::from_secs(12), async {
        while kernel.settlement_health().global_error.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(kernel.settlement_health().recent_errors.is_empty());
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn failed_settlement_enumeration_uses_exponential_backoff() {
    let observed = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let kernel =
        AgentKernel::recover_with_clock(observed.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    observed.waiting_page_reads.store(0, Ordering::Release);
    observed.fail_waiting_pages.store(true, Ordering::Release);
    let workers = kernel.start_workers();
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while observed.waiting_page_reads.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(990)).await;
    let attempts = observed.waiting_page_reads.load(Ordering::Acquire);
    assert!(
        (3..=4).contains(&attempts),
        "unexpected retry attempts: {attempts}"
    );
    assert_eq!(kernel.settlement_health().failures, attempts as u64);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn transient_ancestor_settlement_failure_is_retried_without_restart() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let root_id = SessionId::new("session-settlement-retry-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-settlement-retry-root"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-settlement-retry-root".into())
        .unwrap();
    let root_claim = kernel
        .claim("executor-settlement-retry-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child_id = SessionId::new("session-settlement-retry-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&root_claim).unwrap(),
            child_session_id: child_id.clone(),
            task_name: "child".into(),
            message_id: MessageId::new("message-settlement-retry-child").unwrap(),
            message: "settle after a transient Store failure".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel
        .register("executor-settlement-retry-child".into())
        .unwrap();
    let child_claim = kernel
        .claim("executor-settlement-retry-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
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

    store.fail_next_agent_tree_read_for(root_id.clone());
    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    assert!(store.active_activation(&child_id).await.unwrap().is_none());
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while kernel.settlement_health().failures == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    wait_for_settlement(store.as_ref(), &root_id).await;
    assert!(kernel.settlement_health().failures >= 1);
    assert!(kernel.settlement_health().recent_errors.is_empty());
    assert_eq!(
        store
            .list_ready_messages(&root_id, None, 8)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn reserved_child_completion_settles_at_full_parent_mailbox_occupancy() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let root_id = SessionId::new("session-reserved-completion-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-reserved-completion-root"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-reserved-completion-root".into())
        .unwrap();
    let root_claim = kernel
        .claim(
            "executor-reserved-completion-root",
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
    let child_id = SessionId::new("session-reserved-completion-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&root_claim).unwrap(),
            child_session_id: child_id,
            task_name: "child".into(),
            message_id: MessageId::new("message-reserved-completion-child").unwrap(),
            message: "complete into the reserved mailbox slot".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel
        .register("executor-reserved-completion-child".into())
        .unwrap();
    let child_claim = kernel
        .claim(
            "executor-reserved-completion-child",
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();

    for index in 0..rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES - 1 {
        kernel
            .submit_message(SubmitMessage {
                session: resume(&kernel, root_id.clone()).await,
                message: mailbox_message(&format!("message-parent-capacity-{index}")),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
            })
            .await
            .unwrap();
    }
    assert!(matches!(
        kernel
            .submit_message(SubmitMessage {
                session: resume(&kernel, root_id.clone()).await,
                message: mailbox_message("message-parent-capacity-overflow"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
            })
            .await,
        Err(TurnError::Capacity)
    ));
    kernel
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();

    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();

    wait_for_settlement(store.as_ref(), &root_id).await;
    assert!(store.active_activation(&root_id).await.unwrap().is_none());
    let mailbox = store.read_agent_mailbox_summary(&root_id).await.unwrap();
    assert_eq!(
        mailbox.pending_count,
        rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
    );
    assert_eq!(
        store
            .list_ready_messages(&root_id, None, 8)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn activation_terminal_accepts_a_turn_submitted_during_preparation() {
    let memory = Arc::new(MemoryStore::new());
    let observed = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = observed.clone();
    let kernel = AgentKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_workers();
    let session_id = SessionId::new("session-terminal-submission-race").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-terminal-submission-race"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel
        .register("executor-terminal-submission-race".into())
        .unwrap();
    let claim = kernel
        .claim(
            "executor-terminal-submission-race",
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();

    observed.pause_next_descendant_snapshot();
    let finish = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move {
            kernel
                .finish_activation_turn(
                    &claim,
                    &TurnOutcome::Failed {
                        code: "fixture.failure".into(),
                        message: "exercise descendant cancellation preparation".into(),
                    },
                )
                .await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observed.wait_for_descendant_snapshot_pause(),
    )
    .await
    .expect("activation terminal preparation did not reach the descendant snapshot");

    let queued = kernel
        .submit(SubmitTurn {
            turn_id: TurnId::new("turn-terminal-submission-race-queued").unwrap(),
            session: resume(&kernel, session_id.clone()).await,
            text: "accepted while terminal preparation is paused".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    observed.release_descendant_snapshot();

    assert!(
        finish.await.unwrap().is_ok(),
        "a durably accepted suffix must not turn terminal preparation into an invariant failure"
    );
    assert_eq!(
        kernel.outcome(&session_id, &queued.turn_id).await.unwrap(),
        None
    );
    kernel.shutdown(worker).await.unwrap();
}
