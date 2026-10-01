use super::*;
use rsi_execution::{ExecutionCoordinates, ExecutionLocation, ExecutionTargetId};
#[path = "../../../../../fixtures/rsi/execution/metadata.rs"]
pub(super) mod tuple;
fn remote_header(session: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session).unwrap(),
        1,
        ExecutionCoordinates::new(
            ExecutionLocation::Ssh {
                target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
            },
            "/workspace",
        )
        .unwrap(),
        AgentPresetId::new("test-agent").unwrap(),
        profile(),
    )
    .unwrap()
}
#[tokio::test(start_paused = true)]
#[allow(clippy::too_many_lines)] // One exact durable input crosses rejection, revocation, readmission and a running claim.
async fn ssh_pending_input_needs_live_admission_and_transfers_exact_lease_to_claim() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let header = remote_header("ssh-input");
    let session = header.session_id().clone();
    let message = mailbox_message("ssh-message");
    let request = |prepared| SubmitMessage {
        session: prepared,
        message: message.clone(),
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
    };
    assert!(matches!(
        kernel.submit_message(request(fresh(header.clone()))).await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert!(
        matches!(store.header(&session).await, Err(StoreError::NotFound(_))),
        "rejected input must not create durable state"
    );
    let gate = Arc::new(tuple::Gate::default());
    let original = tuple::lease(header.coordinates().location().clone(), gate.clone(), 1);
    let receipt = kernel
        .submit_message(request(
            fresh(header.clone())
                .with_execution(original.clone())
                .unwrap(),
        ))
        .await
        .unwrap();
    gate.revoked.store(true, Ordering::SeqCst);
    let _executor = kernel.register("executor-authority".into()).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            kernel.claim("executor-authority", CancellationToken::new())
        )
        .await
        .is_err()
    );
    assert_eq!(
        kernel
            .message_status(&session, &message.message_id)
            .await
            .unwrap()
            .state,
        MessageState::Pending
    );
    let replacement = tuple::lease(
        header.coordinates().location().clone(),
        Arc::new(tuple::Gate::default()),
        2,
    );
    assert!(matches!(
        kernel
            .claim_message(ClaimMessage {
                session: kernel
                    .prepare_resume(&session)
                    .await
                    .unwrap()
                    .with_execution(replacement.clone())
                    .unwrap(),
                message_id: message.message_id.clone(),
                activation_id: ActivationId::new("cannot-replace-admission").unwrap(),
                path: AgentPath::root(),
                turn_id: TurnId::new("cannot-replace-admission").unwrap(),
                step_id: StepId::new("cannot-replace-admission").unwrap(),
            })
            .await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert_eq!(
        kernel
            .submit_message(request(
                resume(&kernel, session.clone())
                    .await
                    .with_execution(replacement.clone())
                    .unwrap()
            ))
            .await
            .unwrap(),
        receipt
    );
    let claim = kernel
        .claim("executor-authority", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.execution(), Some(&replacement));
    assert_eq!(
        kernel.agent_caller(&claim).unwrap().execution(),
        Some(&replacement)
    );
    let third = tuple::lease(
        header.coordinates().location().clone(),
        Arc::new(tuple::Gate::default()),
        3,
    );
    assert!(matches!(
        kernel
            .submit_message(request(
                resume(&kernel, session.clone())
                    .await
                    .with_execution(third)
                    .unwrap()
            ))
            .await
            .unwrap()
            .state,
        MessageState::Claimed { .. }
    ));
    assert_eq!(
        kernel.agent_caller(&claim).unwrap().execution(),
        Some(&replacement),
        "a later retry cannot substitute a running Turn's owner"
    );
    let controls = store.read_controls(&session, 0, 16).await.unwrap();
    assert_eq!(
        controls
            .records
            .iter()
            .filter(|record| matches!(
                record.body(),
                AgentControlRecordBody::MessageAccepted { .. }
            ))
            .count(),
        1
    );
    kernel.release(&claim).unwrap();
    kernel.shutdown(worker).await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn ssh_pending_input_does_not_recover_authority_from_durable_coordinates() {
    let store = Arc::new(MemoryStore::new());
    let header = remote_header("ssh-restart");
    let session = header.session_id().clone();
    let message = mailbox_message("ssh-restart-message");
    let first = kernel(store.clone()).await;
    let first_worker = first.start_workers();
    let gate = Arc::new(tuple::Gate::default());
    let execution = tuple::lease(header.coordinates().location().clone(), gate.clone(), 1);
    first
        .submit_message(SubmitMessage {
            session: fresh(header.clone()).with_execution(execution).unwrap(),
            message: message.clone(),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    first.shutdown(first_worker).await.unwrap();
    assert_eq!(
        Arc::strong_count(&gate),
        1,
        "shutdown releases pending execution authority even while the Kernel handle remains alive"
    );
    drop(first);
    let restarted = kernel(store.clone()).await;
    let worker = restarted.start_workers();
    let _executor = restarted.register("executor-restart".into()).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            restarted.claim("executor-restart", CancellationToken::new())
        )
        .await
        .is_err()
    );
    assert_eq!(
        store.read_facts(&session, 0, 8).await.unwrap().durable_seq,
        0
    );
    let renewed = tuple::lease(
        header.coordinates().location().clone(),
        Arc::new(tuple::Gate::default()),
        2,
    );
    restarted
        .submit_message(SubmitMessage {
            session: resume(&restarted, session.clone())
                .await
                .with_execution(renewed.clone())
                .unwrap(),
            message,
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let claim = restarted
        .claim("executor-restart", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.execution(), Some(&renewed));
    restarted.release(&claim).unwrap();
    restarted.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn ssh_queue_successor_uses_editor_lease_and_receipt_retry_cannot_rebind_it() {
    use rsi_agent_session_protocol::{
        MessageDelivery, QueueMutation, QueueMutationRequest, QueueOperationId, QueueSlotId,
    };
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    let header = remote_header("ssh-queue-edit");
    let session = header.session_id().clone();
    let original = tuple::lease(
        header.coordinates().location().clone(),
        Arc::new(tuple::Gate::default()),
        1,
    );
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header.clone()).with_execution(original).unwrap(),
            message: mailbox_message("original"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let request = QueueMutationRequest {
        operation_id: QueueOperationId::new("edit").unwrap(),
        slot_id: QueueSlotId::new("original").unwrap(),
        expected_message_id: MessageId::new("original").unwrap(),
        mutation: QueueMutation::Replace {
            new_message_id: MessageId::new("successor").unwrap(),
            content: vec![AgentMessageContent::Text {
                text: "edited".into(),
            }],
        },
    };
    assert!(matches!(
        kernel.mutate_queue(&session, request.clone(), None).await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert!(
        kernel
            .queue_mutation_status(&session, &request.operation_id)
            .await
            .unwrap()
            .is_none()
    );
    let edited = tuple::lease(
        header.coordinates().location().clone(),
        Arc::new(tuple::Gate::default()),
        2,
    );
    let receipt = kernel
        .mutate_queue(&session, request.clone(), Some(edited.clone()))
        .await
        .unwrap();
    let substitution = tuple::lease(
        header.coordinates().location().clone(),
        Arc::new(tuple::Gate::default()),
        3,
    );
    assert_eq!(
        kernel
            .mutate_queue(&session, request, Some(substitution))
            .await
            .unwrap(),
        receipt
    );
    let _executor = kernel.register("queue-execution".into()).unwrap();
    let claim = kernel
        .claim("queue-execution", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.execution(), Some(&edited));
    kernel.release(&claim).unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn revoked_running_claim_rejects_new_agent_work_but_settles_started_tool() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let header = remote_header("ssh-running");
    let gate = Arc::new(tuple::Gate::default());
    let execution = tuple::lease(header.coordinates().location().clone(), gate.clone(), 1);
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header).with_execution(execution).unwrap(),
            message: mailbox_message("ssh-running-message"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _executor = kernel.register("executor-revoked".into()).unwrap();
    let claim = kernel
        .claim("executor-revoked", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = super::tool_origin::control_tool_caller(&kernel, &claim).await;
    let child = SessionId::new("revoked-child").unwrap();
    let request = SpawnAgentRequest {
        output_contract: None,
        role: None,
        caller: caller.clone(),
        model: None,
        reasoning_effort: None,
        cancellation: CancellationToken::new(),
        child_session_id: child.clone(),
        task_name: "child".into(),
        message_id: MessageId::new("child-message").unwrap(),
        message: "work".into(),
        fork_turns: ForkTurnSelection::None,
    };
    assert!(
        kernel
            .list_agents(
                &caller,
                rsi_agent_turn_protocol::AgentListScope::Descendants
            )
            .await
            .unwrap()
            .is_empty()
    );
    gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        kernel
            .list_agents(
                &caller,
                rsi_agent_turn_protocol::AgentListScope::Descendants
            )
            .await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert!(matches!(
        kernel.spawn_agent(request).await,
        Err(TurnError::ExecutionUnavailable)
    ));
    assert!(matches!(
        store.header(&child).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        kernel
            .wait_agent(
                &caller,
                std::time::Duration::from_millis(10),
                CancellationToken::new()
            )
            .await,
        Err(TurnError::ExecutionUnavailable)
    ));
    super::tool_origin::finish_control_tool(&kernel, &claim).await;
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(claim.session_id(), claim.turn_id())
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    kernel.shutdown(worker).await.unwrap();
}
