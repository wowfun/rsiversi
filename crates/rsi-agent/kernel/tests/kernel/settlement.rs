use super::*;

#[tokio::test]
async fn transient_ancestor_settlement_failure_is_retried_without_restart() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-settlement-retry-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-settlement-retry-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
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
    assert!(matches!(
        kernel
            .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
            .await,
        Err(TurnError::Store(message)) if message.contains("injected Agent tree-read failure")
    ));
    assert!(store.active_activation(&child_id).await.unwrap().is_none());

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store.active_activation(&root_id).await.unwrap().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the background Kernel worker must retry durable ancestor settlement");
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
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-reserved-completion-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-reserved-completion-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
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
                target: MessageTarget::NextStep,
                wake_required: false,
            })
            .await
            .unwrap();
    }
    assert!(matches!(
        kernel
            .submit_message(SubmitMessage {
                session: resume(&kernel, root_id.clone()).await,
                message: mailbox_message("message-parent-capacity-overflow"),
                target: MessageTarget::NextStep,
                wake_required: false,
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
    let kernel = SessionKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-terminal-submission-race").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-terminal-submission-race"),
            target: MessageTarget::NextTurn,
            wake_required: true,
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
                .finish_activation_turn(&claim, &TurnOutcome::Completed)
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
