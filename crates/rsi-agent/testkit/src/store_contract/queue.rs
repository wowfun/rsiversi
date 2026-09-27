use super::{
    AgentControlRecord, AgentControlRecordBody, AgentMessage, AgentMessageContent,
    AgentMessageSource, AtomicAgentCommit, AtomicSessionAppend, MessageId, MessageOptions,
    MessageTarget, SessionHeader, SessionId, SessionStore,
};
use rsi_agent_session_protocol::{
    MessageDelivery, MessageDiscardReason, QueueMutationOutcome, QueueMutationReceipt,
    QueueMutationRejection, QueueOperationId, QueueSlot, QueueSlotId,
};

fn record(seq: u64, body: AgentControlRecordBody) -> AgentControlRecord {
    AgentControlRecord::new(seq, 1000 + seq, body).unwrap()
}
async fn append(
    store: &dyn SessionStore,
    session: &SessionId,
    seq: u64,
    header: Option<SessionHeader>,
    controls: Vec<AgentControlRecord>,
) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::AtomicAgentCommitResult> {
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: 0,
                expected_control_seq: seq,
                header,
                facts: vec![],
                controls,
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
}
fn receipt(operation: &str, seq: u64, outcome: QueueMutationOutcome) -> QueueMutationReceipt {
    QueueMutationReceipt {
        operation_id: QueueOperationId::new(operation).unwrap(),
        request_fingerprint: "a".repeat(64),
        slot_id: QueueSlotId::new("message-0").unwrap(),
        expected_message_id: MessageId::new("message-0").unwrap(),
        control_seq: seq,
        outcome,
    }
}

const OPERATIONS: usize = 4100;

async fn assert_absent_turn_conversion_rejected(
    store: &dyn SessionStore,
    session: &SessionId,
    original: &rsi_agent_store_protocol::StoreAgentMessage,
) {
    let absent_turn = rsi_agent_session_protocol::TurnId::new("absent-turn").unwrap();
    let mut successor = original.message.clone();
    successor.message_id = MessageId::new("forged-steer").unwrap();
    let forged = receipt(
        "forged-conversion",
        67,
        QueueMutationOutcome::Converted {
            message_id: successor.message_id.clone(),
            accepted_control_seq: 66,
            bound_turn_id: absent_turn.clone(),
        },
    );
    let result = append(
        store,
        session,
        64,
        None,
        vec![
            record(
                65,
                AgentControlRecordBody::MessageSuccessor {
                    predecessor_id: original.message.message_id.clone(),
                    successor_id: successor.message_id.clone(),
                    slot: original.queue_slot.clone(),
                },
            ),
            record(
                66,
                AgentControlRecordBody::MessageAccepted {
                    message: successor.clone(),
                    delivery: MessageDelivery::Steer,
                    bound_turn_id: Some(absent_turn),
                    root_session_id: session.clone(),
                    target: MessageTarget::NextStep,
                    wake_required: false,
                },
            ),
            record(
                67,
                AgentControlRecordBody::QueueMutationRecorded {
                    receipt: forged.clone(),
                },
            ),
        ],
    )
    .await;
    assert!(
        matches!(result, Err(rsi_agent_store_protocol::StoreError::Invalid(ref message))
        if message.contains("current activation Turn")),
        "{result:?}"
    );
    assert_eq!(
        store
            .read_agent_message(session, &original.message.message_id)
            .await
            .unwrap()
            .as_ref(),
        Some(original)
    );
    assert!(
        !store
            .agent_message_exists(session, &successor.message_id)
            .await
            .unwrap()
    );
    assert!(
        store
            .read_queue_mutation(session, &forged.operation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .read_watermarks(session)
            .await
            .unwrap()
            .durable_control_seq,
        64
    );
    store.validate_session(session).await.unwrap();
}

/// Exercises full-queue atomic replacement, immutable prefixes, rollback, order and durable receipt retention.
///
/// # Panics
/// Panics if the Store violates a canonical queue projection or transaction boundary.
#[allow(clippy::too_many_lines)] // One ordered scenario proves full capacity, rollback, immutable prefixes and durable receipt retention in both adapters.
pub async fn assert_queue_store_contract(store: &dyn SessionStore, header: SessionHeader) {
    let session = header.session_id().clone();
    let controls = (0..64)
        .map(|index| {
            record(
                index + 1,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: MessageId::new(format!("message-{index}")).unwrap(),
                        source: AgentMessageSource::Human,
                        content: vec![AgentMessageContent::Text {
                            text: format!("original {index}"),
                        }],
                        options: MessageOptions::default(),
                    },
                    delivery: MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    root_session_id: session.clone(),
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
        })
        .collect::<Vec<_>>();
    let mut invalid_binding = controls[0].body().clone();
    if let AgentControlRecordBody::MessageAccepted {
        delivery,
        bound_turn_id,
        target,
        wake_required,
        ..
    } = &mut invalid_binding
    {
        *delivery = MessageDelivery::Steer;
        *bound_turn_id = Some(rsi_agent_session_protocol::TurnId::new("absent-turn").unwrap());
        *target = MessageTarget::NextStep;
        *wake_required = false;
    }
    assert!(matches!(
        append(
            store,
            &session,
            0,
            Some(header.clone()),
            vec![record(1, invalid_binding)]
        )
        .await,
        Err(rsi_agent_store_protocol::StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.header(&session).await,
        Err(rsi_agent_store_protocol::StoreError::NotFound(_))
    ));
    append(store, &session, 0, Some(header), controls.clone())
        .await
        .unwrap();
    let original = store
        .read_agent_message(&session, &MessageId::new("message-0").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original.queue_slot,
        QueueSlot::initial(&original.message.message_id, 1001, 1)
    );
    let mut foreign = receipt("foreign-slot", 66, QueueMutationOutcome::Withdrawn);
    foreign.slot_id = QueueSlotId::new("message-1").unwrap();
    assert!(
        append(
            store,
            &session,
            64,
            None,
            vec![
                record(
                    65,
                    AgentControlRecordBody::MessageDiscarded {
                        message_id: original.message.message_id.clone(),
                        reason: MessageDiscardReason::Cancelled
                    }
                ),
                record(
                    66,
                    AgentControlRecordBody::QueueMutationRecorded { receipt: foreign }
                ),
            ]
        )
        .await
        .is_err()
    );
    assert_eq!(
        store
            .read_agent_message(&session, &original.message.message_id)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    assert_eq!(store.queue_mutation_count(&session).await.unwrap(), 0);
    assert_absent_turn_conversion_rejected(store, &session, &original).await;
    let mut successor = original.message.clone();
    successor.message_id = MessageId::new("replacement").unwrap();
    successor.content = vec![AgentMessageContent::Text {
        text: "edited complete content".into(),
    }];
    let link = record(
        65,
        AgentControlRecordBody::MessageSuccessor {
            predecessor_id: original.message.message_id.clone(),
            successor_id: successor.message_id.clone(),
            slot: original.queue_slot.clone(),
        },
    );
    let accepted = record(
        66,
        AgentControlRecordBody::MessageAccepted {
            message: successor.clone(),
            delivery: original.delivery,
            bound_turn_id: None,
            root_session_id: session.clone(),
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    );
    let saved = receipt(
        "replace",
        67,
        QueueMutationOutcome::Replaced {
            message_id: successor.message_id.clone(),
            accepted_control_seq: 66,
        },
    );
    let recorded = record(
        67,
        AgentControlRecordBody::QueueMutationRecorded {
            receipt: saved.clone(),
        },
    );
    // The store boundary rejects a last-position successor before indexed access.
    assert!(
        append(store, &session, 64, None, vec![link.clone()])
            .await
            .is_err()
    );
    // Missing receipt cannot partially discard a predecessor.
    assert!(
        append(
            store,
            &session,
            64,
            None,
            vec![link.clone(), accepted.clone()]
        )
        .await
        .is_err()
    );
    assert_eq!(
        store
            .read_agent_message(&session, &original.message.message_id)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    let mut incorrect = original.queue_slot.clone();
    incorrect.timestamp_ms += 1;
    let bad_link = record(
        65,
        AgentControlRecordBody::MessageSuccessor {
            predecessor_id: original.message.message_id.clone(),
            successor_id: successor.message_id.clone(),
            slot: incorrect,
        },
    );
    assert!(
        append(
            store,
            &session,
            64,
            None,
            vec![bad_link, accepted.clone(), recorded.clone()]
        )
        .await
        .is_err()
    );
    append(store, &session, 64, None, vec![link, accepted, recorded])
        .await
        .unwrap();
    let old = store
        .read_agent_message(&session, &original.message.message_id)
        .await
        .unwrap()
        .unwrap();
    for id in [&original.message.message_id, &successor.message_id] {
        assert!(store.agent_message_exists(&session, id).await.unwrap());
    }
    assert!(
        !store
            .agent_message_exists(&session, &MessageId::new("never-accepted").unwrap())
            .await
            .unwrap()
    );
    assert_eq!(old.message, original.message);
    assert!(matches!(
        old.state,
        rsi_agent_store_protocol::StoreAgentMessageState::Discarded {
            reason: MessageDiscardReason::Replaced,
            control_seq: 65
        }
    ));
    let current = store
        .read_queue_slot(&session, &saved.slot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.message, successor);
    assert_eq!(current.queue_slot, original.queue_slot);
    assert_eq!(current.accepted_control_seq, 66);
    assert_eq!(current.accepted_timestamp_ms, 1066);
    let ready = store.list_ready_messages(&session, None, 64).await.unwrap();
    assert_eq!(ready.messages.len(), 64);
    assert_eq!(ready.messages[0].message_id, successor.message_id);
    assert_eq!(ready.messages[0].control_seq, 1);
    assert_eq!(ready.messages[0].timestamp_ms, 1001);
    assert_eq!(
        store.read_controls(&session, 0, 64).await.unwrap().records,
        controls
    );
    assert_eq!(
        store
            .read_queue_mutation(&session, &saved.operation_id)
            .await
            .unwrap(),
        Some(saved.clone())
    );
    let duplicate = receipt(
        "replace",
        68,
        QueueMutationOutcome::Rejected {
            reason: QueueMutationRejection::MissingSlot,
            current_message_id: None,
        },
    );
    let rejected = append(
        store,
        &session,
        67,
        None,
        vec![record(
            68,
            AgentControlRecordBody::QueueMutationRecorded { receipt: duplicate },
        )],
    )
    .await;
    assert!(
        matches!(
            rejected,
            Err(rsi_agent_store_protocol::StoreError::Invalid(_))
        ),
        "{rejected:?}"
    );
    assert_eq!(store.queue_mutation_count(&session).await.unwrap(), 1);
    assert_eq!(
        store
            .read_queue_mutation(&session, &saved.operation_id)
            .await
            .unwrap(),
        Some(saved.clone())
    );
    // Rejections remain replayable without exhausting this Session’s editing lifetime.
    let mut seq = 67;
    for start in (1..OPERATIONS).step_by(256) {
        let mut batch = Vec::new();
        for index in start..(start + 256).min(OPERATIONS) {
            seq += 1;
            let receipt = receipt(
                &format!("reject-{index}"),
                seq,
                QueueMutationOutcome::Rejected {
                    reason: QueueMutationRejection::MissingSlot,
                    current_message_id: None,
                },
            );
            batch.push(record(
                seq,
                AgentControlRecordBody::QueueMutationRecorded { receipt },
            ));
        }
        append(store, &session, seq - batch.len() as u64, None, batch)
            .await
            .unwrap();
    }
    assert_eq!(
        store.queue_mutation_count(&session).await.unwrap(),
        OPERATIONS
    );
    let extra = record(
        seq + 1,
        AgentControlRecordBody::QueueMutationRecorded {
            receipt: receipt(
                "after-many-rejections",
                seq + 1,
                QueueMutationOutcome::Rejected {
                    reason: QueueMutationRejection::MissingSlot,
                    current_message_id: None,
                },
            ),
        },
    );
    assert!(
        append(store, &session, seq, None, vec![extra])
            .await
            .is_ok()
    );
    assert_eq!(
        store
            .read_queue_mutation(&session, &saved.operation_id)
            .await
            .unwrap(),
        Some(saved)
    );
    store.validate_session(&session).await.unwrap();
}
