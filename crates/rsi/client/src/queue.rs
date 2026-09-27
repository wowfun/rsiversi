//! Shared bounded queue projection and exact-envelope reconciliation.
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody as Body, AgentMessageSourceKind, MessageOptions,
    QueueMutationReceipt, QueueMutationRequest, QueueSlot, QueueSlotId, TurnId,
};
use rsi_agent_store_protocol::StorePendingMessage;
use rsi_session_protocol::{Result, SessionError, SessionHandle};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};

/// Rust-owned controls for one still-pending slot, including read-only non-Human sources.
#[derive(Clone, Debug, Serialize)]
pub struct QueueItem<'a> {
    /// Complete bounded routing metadata without content allocation.
    #[serde(flatten)]
    pub message: &'a StorePendingMessage,
    /// Only direct Human pending input is editable.
    pub editable: bool,
    /// Exact displayed Turn for conversion; never inferred from busy state in JavaScript.
    pub convert_turn: Option<&'a TurnId>,
    /// Presentation label for the resolved route.
    pub delivery_label: &'static str,
    /// Presentation label for opening the complete accepted content.
    pub read_label: &'static str,
    /// Pending-only action label, absent for other source classes.
    pub withdraw_label: Option<&'static str>,
    /// Exact-Turn conversion label, absent when conversion is unavailable.
    pub convert_label: Option<&'static str>,
}
impl<'a> QueueItem<'a> {
    /// Derives action availability from acknowledged presentation coordinates.
    pub fn new(message: &'a StorePendingMessage, displayed_turn: Option<&'a TurnId>) -> Self {
        let editable = message.source_kind == AgentMessageSourceKind::Human;
        let convert_turn = displayed_turn.filter(|turn| {
            editable && !message.has_turn_options && message.bound_turn_id.as_ref() != Some(*turn)
        });
        let delivery_label = match message.target {
            rsi_agent_session_protocol::MessageTarget::NextTurn => "Next turn",
            rsi_agent_session_protocol::MessageTarget::NextStep => "Next step",
        };
        let convert_label = convert_turn.as_ref().map(|_| "Steer now");
        Self {
            message,
            editable,
            convert_turn,
            delivery_label,
            read_label: if editable { "Edit" } else { "View" },
            withdraw_label: editable.then_some("Withdraw"),
            convert_label,
        }
    }
}

/// At most 64 current slots and one adjacent successor link; old versions are not retained.
#[derive(Clone, Debug, Default)]
pub struct QueueProjection {
    entries: BTreeMap<QueueSlotId, StorePendingMessage>,
    revision: Arc<()>,
    successor: Option<(rsi_agent_session_protocol::MessageId, QueueSlot)>,
}
impl QueueProjection {
    /// Seeds from the same atomic inspection horizon used by the observation cursor.
    pub fn seed(&mut self, pending: Vec<StorePendingMessage>) {
        let entries = pending
            .into_iter()
            .map(|entry| (entry.queue_slot.id.clone(), entry))
            .collect();
        if self.entries != entries {
            self.entries = entries;
            self.revision = Arc::new(());
        }
        self.successor = None;
    }
    /// Applies a canonical control to the bounded current-slot projection.
    pub fn observe(&mut self, record: &AgentControlRecord) {
        let mut changed = false;
        match record.body() {
            Body::MessageSuccessor {
                predecessor_id,
                successor_id,
                slot,
            } => {
                let before = self.entries.len();
                self.entries
                    .retain(|_, entry| &entry.message_id != predecessor_id);
                changed = before != self.entries.len();
                self.successor = Some((successor_id.clone(), slot.clone()));
            }
            Body::MessageAccepted {
                message,
                delivery,
                target,
                bound_turn_id,
                ..
            } => {
                let slot = self
                    .successor
                    .take()
                    .filter(|(id, _)| id == &message.message_id)
                    .map_or_else(
                        || {
                            QueueSlot::initial(
                                &message.message_id,
                                record.timestamp_ms(),
                                record.seq(),
                            )
                        },
                        |(_, slot)| slot,
                    );
                let entry = StorePendingMessage {
                    queue_slot: slot.clone(),
                    source_kind: message.source.kind(),
                    has_turn_options: message.options != MessageOptions::default(),
                    message_id: message.message_id.clone(),
                    delivery: *delivery,
                    target: *target,
                    permits_promotion: rsi_agent_store_protocol::message_permits_promotion(
                        message.source.kind(),
                        *delivery,
                        bound_turn_id.as_ref(),
                    ),
                    bound_turn_id: bound_turn_id.clone(),
                    accepted_control_seq: record.seq(),
                };
                if self.entries.len() < rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
                    || self.entries.contains_key(&slot.id)
                {
                    changed = self.entries.get(&slot.id) != Some(&entry);
                    self.entries.insert(slot.id, entry);
                }
            }
            Body::MessageClaimed { message_id, .. } | Body::MessageDiscarded { message_id, .. } => {
                let before = self.entries.len();
                self.entries
                    .retain(|_, entry| &entry.message_id != message_id);
                changed = before != self.entries.len();
            }
            Body::MessagePromoted { message_id } => {
                if let Some(entry) = self
                    .entries
                    .values_mut()
                    .find(|entry| &entry.message_id == message_id)
                {
                    changed = entry.target != rsi_agent_session_protocol::MessageTarget::NextTurn;
                    entry.target = rsi_agent_session_protocol::MessageTarget::NextTurn;
                }
            }
            _ => {}
        }
        if changed {
            self.revision = Arc::new(());
        }
    }
    /// Identity of the current visible queue; shared by unchanged snapshots.
    pub fn revision(&self) -> Arc<()> {
        self.revision.clone()
    }
    /// Borrows one exact current slot without sorting or allocating a presentation view.
    pub fn get(&self, slot: &QueueSlotId) -> Option<&StorePendingMessage> {
        self.entries.get(slot)
    }
    /// Returns stable scheduling order and exact displayed-Turn actions.
    pub fn view<'a>(&'a self, turn: Option<&'a TurnId>) -> Vec<QueueItem<'a>> {
        let mut entries = self
            .entries
            .values()
            .map(|entry| QueueItem::new(entry, turn))
            .collect::<Vec<_>>();
        entries.sort_by_key(|item| {
            (
                item.message.queue_slot.timestamp_ms,
                item.message.queue_slot.control_seq,
            )
        });
        entries
    }
}

/// Verifies a saved result against every frozen request coordinate before settling it.
pub fn validate_queue_receipt(
    request: &QueueMutationRequest,
    receipt: &QueueMutationReceipt,
) -> Result<()> {
    if receipt.validate_for(request).is_err() {
        return Err(SessionError::QueueOutcomeUnknown {
            operation_id: request.operation_id.clone(),
        });
    }
    Ok(())
}

/// Queries first after an unknown outcome; only an explicit retry permits identical replay.
pub async fn reconcile_queue(
    handle: &dyn SessionHandle,
    request: QueueMutationRequest,
    retry: bool,
) -> Result<Option<QueueMutationReceipt>> {
    let receipt = match handle.queue_mutation_status(&request.operation_id).await? {
        Some(receipt) => Some(receipt),
        None if retry => Some(handle.mutate_queue(request.clone()).await?),
        None => None,
    };
    if let Some(receipt) = &receipt {
        validate_queue_receipt(&request, receipt)?;
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{MessageDelivery, MessageId, MessageTarget};

    #[test]
    fn queue_view_keeps_slot_order_and_serializes_borrowed_action_coordinates() {
        let entry = |id: &str, time, seq| {
            let message_id = MessageId::new(id).unwrap();
            StorePendingMessage {
                queue_slot: QueueSlot::initial(&message_id, time, seq),
                message_id,
                source_kind: AgentMessageSourceKind::Human,
                has_turn_options: false,
                delivery: MessageDelivery::NextTurn,
                target: MessageTarget::NextTurn,
                permits_promotion: false,
                bound_turn_id: None,
                accepted_control_seq: seq + 100,
            }
        };
        let mut projection = QueueProjection::default();
        projection.seed(vec![
            entry("a", 11, 1),
            entry("z", 10, 2),
            entry("b", 10, 1),
        ]);
        let turn = TurnId::new("displayed").unwrap();
        let view = projection.view(Some(&turn));
        assert_eq!(
            view.iter()
                .map(|item| item.message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["b", "z", "a"]
        );
        for item in &view {
            assert!(std::ptr::eq(
                item.message,
                std::ptr::from_ref(&projection.entries[&item.message.queue_slot.id])
            ));
            assert!(std::ptr::eq(
                item.convert_turn.unwrap(),
                std::ptr::from_ref(&turn)
            ));
        }
        let encoded = serde_json::to_value(view).unwrap();
        assert_eq!(encoded[0]["message_id"], "b");
        assert_eq!(encoded[0]["convert_turn"], "displayed");
        assert_eq!(encoded[0]["convert_label"], "Steer now");
        let revision = projection.revision();
        let control = AgentControlRecord::new(
            200,
            1,
            Body::MessageDiscarded {
                message_id: MessageId::new("absent").unwrap(),
                reason: rsi_agent_session_protocol::MessageDiscardReason::Cancelled,
            },
        )
        .unwrap();
        projection.observe(&control);
        assert!(Arc::ptr_eq(&revision, &projection.revision()));
        let control = AgentControlRecord::new(
            201,
            1,
            Body::MessageDiscarded {
                message_id: MessageId::new("b").unwrap(),
                reason: rsi_agent_session_protocol::MessageDiscardReason::Cancelled,
            },
        )
        .unwrap();
        projection.observe(&control);
        assert!(!Arc::ptr_eq(&revision, &projection.revision()));
        assert_eq!(projection.view(None).len(), 2);
        let revision = projection.revision();
        projection.observe(&control);
        assert!(Arc::ptr_eq(&revision, &projection.revision()));
    }
}
