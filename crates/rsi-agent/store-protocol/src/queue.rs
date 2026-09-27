//! Mechanical queue linkage shared by append admission, durable adapters and the offline verifier.
use crate::{Result, StoreAgentMessage, StoreAgentMessageState, StoreError};
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody as Body, AgentMessageSource, MessageDelivery,
    MessageDiscardReason, MessageOptions, QueueMutationOutcome, QueueSlot,
};

fn corrupt() -> StoreError {
    StoreError::Corrupt("queue successor or receipt differs from its canonical boundary".into())
}

/// Checks a withdrawal's current slot and source before its discard changes the projection.
pub fn validate_queue_withdrawal(
    predecessor: &StoreAgentMessage,
    receipt: &rsi_agent_session_protocol::QueueMutationReceipt,
) -> Result<()> {
    if receipt.outcome != QueueMutationOutcome::Withdrawn
        || predecessor.message.message_id != receipt.expected_message_id
        || predecessor.queue_slot.id != receipt.slot_id
        || predecessor.message.source != AgentMessageSource::Human
        || predecessor.state != StoreAgentMessageState::Pending
    {
        return Err(corrupt());
    }
    Ok(())
}

/// Requires each successful mutation and its receipt to occupy one atomic suffix.
pub fn validate_queue_suffix(records: &[AgentControlRecord]) -> Result<()> {
    for (index, record) in records.iter().enumerate() {
        match record.body() {
            Body::MessageSuccessor {
                predecessor_id,
                successor_id,
                slot,
            } => {
                let Some(acceptance) = records.get(index + 1) else {
                    return Err(corrupt());
                };
                let Some(receipt) = records.get(index + 2) else {
                    return Err(corrupt());
                };
                let Body::MessageAccepted {
                    message,
                    bound_turn_id,
                    ..
                } = acceptance.body()
                else {
                    return Err(corrupt());
                };
                let Body::QueueMutationRecorded { receipt } = receipt.body() else {
                    return Err(corrupt());
                };
                let (id, seq, bound) = match &receipt.outcome {
                    QueueMutationOutcome::Replaced {
                        message_id,
                        accepted_control_seq,
                    } => (message_id, *accepted_control_seq, None),
                    QueueMutationOutcome::Converted {
                        message_id,
                        accepted_control_seq,
                        bound_turn_id,
                    } => (message_id, *accepted_control_seq, Some(bound_turn_id)),
                    _ => return Err(corrupt()),
                };
                if predecessor_id != &receipt.expected_message_id
                    || slot.id != receipt.slot_id
                    || successor_id != id
                    || successor_id != &message.message_id
                    || seq != acceptance.seq()
                    || bound.is_some() && bound != bound_turn_id.as_ref()
                {
                    return Err(corrupt());
                }
            }
            Body::QueueMutationRecorded { receipt } => {
                receipt.validate().map_err(|_| corrupt())?;
                match &receipt.outcome {
                    QueueMutationOutcome::Rejected { .. } => {}
                    QueueMutationOutcome::Withdrawn => {
                        if !matches!(index.checked_sub(1).and_then(|i| records.get(i)).map(AgentControlRecord::body), Some(Body::MessageDiscarded { message_id, reason: MessageDiscardReason::Cancelled }) if message_id == &receipt.expected_message_id)
                        {
                            return Err(corrupt());
                        }
                    }
                    QueueMutationOutcome::Replaced { .. }
                    | QueueMutationOutcome::Converted { .. } => {
                        if !matches!(
                            index
                                .checked_sub(2)
                                .and_then(|i| records.get(i))
                                .map(AgentControlRecord::body),
                            Some(Body::MessageSuccessor { .. })
                        ) {
                            return Err(corrupt());
                        }
                    }
                }
            }
            // Replaced is exclusively derived from MessageSuccessor, never an independent discard.
            Body::MessageDiscarded {
                reason: MessageDiscardReason::Replaced,
                ..
            } => return Err(corrupt()),
            _ => {}
        }
    }
    Ok(())
}

/// Validates the immutable predecessor and complete successor payload before retiring it.
pub fn validate_queue_successor(
    predecessor: &StoreAgentMessage,
    link: &AgentControlRecord,
    acceptance: &AgentControlRecord,
    receipt: &AgentControlRecord,
) -> Result<QueueSlot> {
    let Body::MessageSuccessor {
        predecessor_id,
        successor_id,
        slot,
    } = link.body()
    else {
        return Err(corrupt());
    };
    let Body::MessageAccepted {
        message,
        delivery,
        bound_turn_id,
        root_session_id,
        target,
        wake_required,
    } = acceptance.body()
    else {
        return Err(corrupt());
    };
    let Body::QueueMutationRecorded { receipt } = receipt.body() else {
        return Err(corrupt());
    };
    if predecessor.message.message_id != *predecessor_id
        || predecessor.queue_slot != *slot
        || !matches!(predecessor.state, StoreAgentMessageState::Pending)
        || predecessor.message.source != AgentMessageSource::Human
        || message.source != AgentMessageSource::Human
        || message.message_id != *successor_id
        || message.options != predecessor.message.options
        || root_session_id != &predecessor.root_session_id
    {
        return Err(corrupt());
    }
    match &receipt.outcome {
        QueueMutationOutcome::Replaced { .. }
            if *delivery == predecessor.delivery
                && *bound_turn_id
                    == if predecessor.target == rsi_agent_session_protocol::MessageTarget::NextStep
                    {
                        predecessor.bound_turn_id.clone()
                    } else {
                        None
                    }
                && *target == predecessor.target
                && *wake_required == predecessor.wake_required => {}
        QueueMutationOutcome::Converted {
            bound_turn_id: expected,
            ..
        } if *delivery == MessageDelivery::Steer
            && bound_turn_id.as_ref() == Some(expected)
            && message.content == predecessor.message.content
            && message.options == MessageOptions::default() => {}
        _ => return Err(corrupt()),
    }
    Ok(slot.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, MessageId, MessageTarget, QueueOperationId, SessionId,
        TurnId,
    };

    #[test]
    fn converted_successor_must_remain_a_nonwaking_next_step() {
        let id = MessageId::new("original").unwrap();
        let predecessor = StoreAgentMessage {
            queue_slot: QueueSlot::initial(&id, 1, 1),
            message: AgentMessage {
                message_id: id.clone(),
                source: AgentMessageSource::Human,
                options: MessageOptions::default(),
                content: vec![AgentMessageContent::Text {
                    text: "hello".into(),
                }],
            },
            delivery: MessageDelivery::NextTurn,
            bound_turn_id: None,
            accepted_timestamp_ms: 1,
            encoded_message_bytes: 128,
            root_session_id: SessionId::new("root").unwrap(),
            target: MessageTarget::NextTurn,
            wake_required: true,
            accepted_control_seq: 1,
            state: StoreAgentMessageState::Pending,
        };
        let successor_id = MessageId::new("successor").unwrap();
        let turn = TurnId::new("active").unwrap();
        let link = AgentControlRecord::new(
            2,
            2,
            Body::MessageSuccessor {
                predecessor_id: id.clone(),
                successor_id: successor_id.clone(),
                slot: predecessor.queue_slot.clone(),
            },
        )
        .unwrap();
        let receipt = AgentControlRecord::new(
            4,
            4,
            Body::QueueMutationRecorded {
                receipt: rsi_agent_session_protocol::QueueMutationReceipt {
                    operation_id: QueueOperationId::new("convert").unwrap(),
                    request_fingerprint: "a".repeat(64),
                    slot_id: predecessor.queue_slot.id.clone(),
                    expected_message_id: id,
                    control_seq: 4,
                    outcome: QueueMutationOutcome::Converted {
                        message_id: successor_id.clone(),
                        accepted_control_seq: 3,
                        bound_turn_id: turn.clone(),
                    },
                },
            },
        )
        .unwrap();
        for (target, wake_required, valid) in [
            (MessageTarget::NextStep, false, true),
            (MessageTarget::NextTurn, true, false),
        ] {
            let mut message = predecessor.message.clone();
            message.message_id = successor_id.clone();
            let acceptance = AgentControlRecord::new(
                3,
                3,
                Body::MessageAccepted {
                    message,
                    delivery: MessageDelivery::Steer,
                    bound_turn_id: Some(turn.clone()),
                    root_session_id: predecessor.root_session_id.clone(),
                    target,
                    wake_required,
                },
            );
            if !valid {
                assert!(
                    acceptance.is_err(),
                    "typed acceptance rejects a bound waking steer"
                );
                continue;
            }
            let acceptance = acceptance.unwrap();
            assert_eq!(
                validate_queue_successor(&predecessor, &link, &acceptance, &receipt).is_ok(),
                valid,
                "target={target:?}, wake={wake_required}"
            );
        }
    }
}
