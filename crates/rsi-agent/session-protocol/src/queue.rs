//! Bounded queue compare-and-set inputs and durable operation receipts.
use crate::{
    AgentMessageContent, MessageId, QueueOperationId, QueueSlotId, Result, SessionError, TurnId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Compact receipt bound, independent of the replaced message size.
pub const MAXIMUM_QUEUE_MUTATION_RECEIPT_BYTES: usize = 4096;

/// Original scheduling position, retained by every successor and Steer promotion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSlot {
    /// Initial message identity.
    pub id: QueueSlotId,
    /// Original acceptance time; successor acceptance retains its actual new time.
    pub timestamp_ms: u64,
    /// Original acceptance sequence used as the ready-index tie breaker.
    pub control_seq: u64,
}
impl QueueSlot {
    /// Constructs the initial immutable slot from a validated acceptance.
    pub fn initial(message_id: &MessageId, timestamp_ms: u64, control_seq: u64) -> Self {
        Self {
            id: QueueSlotId(message_id.0.clone()),
            timestamp_ms,
            control_seq,
        }
    }
    /// Validates durable ordering metadata against its owning acceptance horizon.
    pub fn validate(&self, accepted_control_seq: u64) -> Result<()> {
        if self.timestamp_ms == 0
            || self.control_seq == 0
            || self.control_seq > accepted_control_seq
        {
            return Err(SessionError::Invalid(
                "queue slot has an invalid ready-order boundary".into(),
            ));
        }
        Ok(())
    }
}

/// Human intent for one still-pending queue slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueMutation {
    /// Replace all content, retaining the original delivery and invocation options.
    Replace {
        /// Fresh immutable identity for the successor acceptance.
        new_message_id: MessageId,
        /// Complete ordered content, not a text-only patch.
        content: Vec<AgentMessageContent>,
    },
    /// Bind a new Human steering acceptance to the exact displayed Turn.
    ConvertToSteer {
        /// Fresh immutable identity, even though content is unchanged.
        new_message_id: MessageId,
        /// No other Turn can satisfy this precondition.
        expected_turn_id: TurnId,
    },
    /// Discard pending input without cancelling any claimed Turn.
    Withdraw,
}
impl QueueMutation {
    /// Successor identity, if the mutation accepts a replacement.
    pub fn new_message_id(&self) -> Option<&MessageId> {
        match self {
            Self::Replace { new_message_id, .. } | Self::ConvertToSteer { new_message_id, .. } => {
                Some(new_message_id)
            }
            Self::Withdraw => None,
        }
    }
}

/// Frozen idempotent envelope; Session authority is supplied separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueMutationRequest {
    /// One caller-preallocated operation identity, retained across unknown replies.
    pub operation_id: QueueOperationId,
    /// Initial message identity, stable across all successors.
    pub slot_id: QueueSlotId,
    /// Sole compare-and-set predecessor; a separate revision is unnecessary.
    pub expected_message_id: MessageId,
    /// Complete immutable mutation intent.
    pub mutation: QueueMutation,
}
impl QueueMutationRequest {
    /// Validates external/durable input shape before admission.
    pub fn validate(&self) -> Result<()> {
        self.validate_shape()?;
        crate::bounded_compact_json_len(self, crate::MAXIMUM_SESSION_FACT_BYTES)?;
        Ok(())
    }
    fn validate_shape(&self) -> Result<()> {
        if self.mutation.new_message_id() == Some(&self.expected_message_id) {
            return Err(SessionError::Invalid(
                "queue successor must have a new MessageId".into(),
            ));
        }
        if let QueueMutation::Replace { content, .. } = &self.mutation {
            crate::validate_message_content(content, crate::MAXIMUM_TURN_TEXT_BYTES)?;
        }
        Ok(())
    }
    /// Canonical closed-envelope digest without a second retained payload buffer.
    pub fn fingerprint(&self) -> Result<String> {
        struct HashWriter {
            hash: Sha256,
            bytes: usize,
        }
        impl std::io::Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > crate::MAXIMUM_SESSION_FACT_BYTES - self.bytes {
                    return Err(std::io::Error::other(
                        "queue request exceeds its encoded byte limit",
                    ));
                }
                self.bytes += bytes.len();
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        self.validate_shape()?;
        let mut writer = HashWriter {
            hash: Sha256::new(),
            bytes: 0,
        };
        writer.hash.update(b"rsi.queue-mutation.v1\0");
        serde_json::to_writer(&mut writer, self)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(hex::encode(writer.hash.finalize()))
    }
}

/// Stable domain rejection after admission, distinct from transport failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMutationRejection {
    /// No accepted input belongs to this slot in the selected Session.
    MissingSlot,
    /// Another successor already replaced the expected identity.
    StaleMessage,
    /// Execution claimed the input before mutation admission.
    Claimed,
    /// The current message has already been discarded.
    Discarded,
    /// Only direct Human input is editable.
    NotHuman,
    /// New-Turn invocation options prevent conversion into an existing Turn.
    IncompatibleOptions,
    /// The exact requested Turn is absent, stopping or terminal.
    StaleTurn,
    /// The proposed successor identity already belongs to an acceptance.
    MessageConflict,
}

/// Compact committed result; content remains in its canonical acceptance control.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueMutationOutcome {
    /// Complete content was replaced within the same slot and order.
    Replaced {
        /// Exact successor identity.
        message_id: MessageId,
        /// Exact successor acceptance record.
        accepted_control_seq: u64,
    },
    /// Existing content was accepted as Steer for the requested Turn.
    Converted {
        /// Exact successor identity.
        message_id: MessageId,
        /// Exact successor acceptance record.
        accepted_control_seq: u64,
        /// Immutable bound Turn, retained even after promotion.
        bound_turn_id: TurnId,
    },
    /// Pending input was discarded; no execution was cancelled.
    Withdrawn,
    /// Preconditions failed, durably retaining the same result for retries.
    Rejected {
        /// Closed domain reason.
        reason: QueueMutationRejection,
        /// Latest slot identity, when a slot exists.
        current_message_id: Option<MessageId>,
    },
}
/// Lifetime-retained receipt for one admitted operation in a Session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueMutationReceipt {
    /// Frozen operation identity.
    pub operation_id: QueueOperationId,
    /// Exact request digest, including all content and preconditions.
    pub request_fingerprint: String,
    /// Original slot identity.
    pub slot_id: QueueSlotId,
    /// Predecessor named by the request.
    pub expected_message_id: MessageId,
    /// Canonical receipt control sequence.
    pub control_seq: u64,
    /// Committed outcome.
    pub outcome: QueueMutationOutcome,
}
impl QueueMutationReceipt {
    /// Checks a reply against the complete frozen caller intent, including its outcome shape.
    pub fn validate_for(&self, request: &QueueMutationRequest) -> Result<()> {
        self.validate()?;
        let outcome_matches = match (&request.mutation, &self.outcome) {
            (_, QueueMutationOutcome::Rejected { .. })
            | (QueueMutation::Withdraw, QueueMutationOutcome::Withdrawn) => true,
            (
                QueueMutation::Replace { new_message_id, .. },
                QueueMutationOutcome::Replaced { message_id, .. },
            ) => new_message_id == message_id,
            (
                QueueMutation::ConvertToSteer {
                    new_message_id,
                    expected_turn_id,
                },
                QueueMutationOutcome::Converted {
                    message_id,
                    bound_turn_id,
                    ..
                },
            ) => new_message_id == message_id && expected_turn_id == bound_turn_id,
            _ => false,
        };
        if !outcome_matches
            || self.operation_id != request.operation_id
            || self.request_fingerprint != request.fingerprint()?
            || self.slot_id != request.slot_id
            || self.expected_message_id != request.expected_message_id
        {
            return Err(SessionError::Invalid(
                "queue receipt differs from the frozen request".into(),
            ));
        }
        Ok(())
    }
    /// Revalidates a decoded receipt independently of Store graph checks.
    pub fn validate(&self) -> Result<()> {
        crate::validate_sha256("queue request fingerprint", &self.request_fingerprint)?;
        if self.control_seq == 0 {
            return Err(SessionError::Invalid(
                "queue receipt requires a control sequence".into(),
            ));
        }
        match &self.outcome {
            QueueMutationOutcome::Replaced {
                message_id,
                accepted_control_seq,
            }
            | QueueMutationOutcome::Converted {
                message_id,
                accepted_control_seq,
                ..
            } => {
                if message_id == &self.expected_message_id
                    || *accepted_control_seq == 0
                    || *accepted_control_seq >= self.control_seq
                {
                    return Err(SessionError::Invalid(
                        "queue receipt successor boundary is invalid".into(),
                    ));
                }
            }
            QueueMutationOutcome::Withdrawn | QueueMutationOutcome::Rejected { .. } => {}
        }
        crate::bounded_compact_json_len(self, MAXIMUM_QUEUE_MUTATION_RECEIPT_BYTES)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_mutation_hash_binds_predecessor_and_complete_content() {
        let mut request = QueueMutationRequest {
            operation_id: QueueOperationId::new("edit").unwrap(),
            slot_id: QueueSlotId::new("initial").unwrap(),
            expected_message_id: MessageId::new("initial").unwrap(),
            mutation: QueueMutation::Replace {
                new_message_id: MessageId::new("next").unwrap(),
                content: vec![AgentMessageContent::Text {
                    text: "new text".into(),
                }],
            },
        };
        let first = request.fingerprint().unwrap();
        let decoded: QueueMutationRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(first, decoded.fingerprint().unwrap());
        request.expected_message_id = MessageId::new("different").unwrap();
        assert_ne!(first, request.fingerprint().unwrap());
        request.expected_message_id = MessageId::new("next").unwrap();
        assert!(request.validate().is_err());
        request.expected_message_id = MessageId::new("initial").unwrap();
        if let QueueMutation::Replace { content, .. } = &mut request.mutation {
            content.clear();
        }
        assert!(request.validate().is_err());
    }
    #[test]
    fn receipt_rejects_fabricated_successor_boundary() {
        let mut receipt = QueueMutationReceipt {
            operation_id: QueueOperationId::new("edit").unwrap(),
            request_fingerprint: "a".repeat(64),
            slot_id: QueueSlotId::new("initial").unwrap(),
            expected_message_id: MessageId::new("initial").unwrap(),
            control_seq: 3,
            outcome: QueueMutationOutcome::Replaced {
                message_id: MessageId::new("next").unwrap(),
                accepted_control_seq: 2,
            },
        };
        receipt.validate().unwrap();
        receipt.control_seq = 2;
        assert!(receipt.validate().is_err());
    }
    #[test]
    fn fingerprint_keeps_canonical_bytes_and_rejects_invalid_content() {
        let mut request = QueueMutationRequest {
            operation_id: QueueOperationId::new("edit").unwrap(),
            slot_id: QueueSlotId::new("slot").unwrap(),
            expected_message_id: MessageId::new("old").unwrap(),
            mutation: QueueMutation::Replace {
                new_message_id: MessageId::new("new").unwrap(),
                content: vec![AgentMessageContent::Text {
                    text: "\\\"\n你好".into(),
                }],
            },
        };
        let mut hash = Sha256::new();
        hash.update(b"rsi.queue-mutation.v1\0");
        hash.update(serde_json::to_vec(&request).unwrap());
        assert_eq!(request.fingerprint().unwrap(), hex::encode(hash.finalize()));
        if let QueueMutation::Replace { content, .. } = &mut request.mutation {
            *content = vec![AgentMessageContent::Text {
                text: "x".repeat(crate::MAXIMUM_TURN_TEXT_BYTES + 1),
            }];
        }
        assert!(request.fingerprint().is_err());
        assert!(request.validate().is_err());
    }
}
