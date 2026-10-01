use crate::{Result, StoreError, validate_session_read_limit};
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, AgentMessageSource, ExecutionCoordinates,
    ModelEventPurpose, SessionFact, SessionFactBody, SessionHeader, SessionId,
};
use rsi_ai_protocol::LanguageEvent;
use serde::{Deserialize, Serialize};

/// Validates one exact summary request before Store access.
pub fn validate_activity_summaries(sessions: &[SessionId]) -> Result<()> {
    if sessions.len() > 64
        || sessions
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != sessions.len()
    {
        return Err(StoreError::Invalid(
            "activity summary request exceeds 64 unique identities".into(),
        ));
    }
    Ok(())
}

/// Exact live descending key; activity mutations do not invalidate this cursor.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreActivityCursor {
    /// Last qualifying activity, in Unix milliseconds.
    pub last_activity_ms: u64,
    /// Stable tie-breaker.
    pub session_id: SessionId,
}
/// Bounded indexed metadata, independent of Header or transcript size.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreActivityRow {
    /// Exact durable identity.
    pub session_id: SessionId,
    /// Immutable execution machine and workspace.
    pub coordinates: ExecutionCoordinates,
    /// Header creation timestamp.
    pub created_at_ms: u64,
    /// Latest qualifying activity timestamp; never less than creation.
    pub last_activity_ms: u64,
}
impl StoreActivityRow {
    /// Returns this row's exact live keyset position.
    pub fn cursor(&self) -> StoreActivityCursor {
        StoreActivityCursor {
            last_activity_ms: self.last_activity_ms,
            session_id: self.session_id.clone(),
        }
    }
}
/// A single metadata read snapshot, including the group's newest visible key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreActivityPage {
    /// Exclusive key supplied by the caller.
    pub after: Option<StoreActivityCursor>,
    /// At most the requested number of strictly descending rows.
    pub sessions: Vec<StoreActivityRow>,
    /// A further row existed within this snapshot.
    pub has_more: bool,
    /// Newest group key, including when continuation contains no rows.
    pub newest: Option<StoreActivityCursor>,
}
impl StoreActivityPage {
    /// Checks bounded ordering, scope and scalar metadata at a read boundary.
    pub fn validate(&self, coordinates: Option<&ExecutionCoordinates>, limit: usize) -> Result<()> {
        validate_session_read_limit(limit)?;
        if self.sessions.len() > limit || self.sessions.is_empty() && self.has_more {
            return Err(StoreError::Corrupt("invalid activity page bounds".into()));
        }
        let mut previous = self.after.clone();
        for row in &self.sessions {
            let cursor = row.cursor();
            if row.created_at_ms == 0
                || row.last_activity_ms < row.created_at_ms
                || coordinates.is_some_and(|value| value != &row.coordinates)
                || previous.as_ref().is_some_and(|value| &cursor >= value)
                || self.newest.as_ref().is_none_or(|value| &cursor > value)
            {
                return Err(StoreError::Corrupt(
                    "invalid activity page projection".into(),
                ));
            }
            previous = Some(cursor);
        }
        if self
            .newest
            .as_ref()
            .is_some_and(|value| value.last_activity_ms == 0)
        {
            return Err(StoreError::Corrupt("invalid activity head".into()));
        }
        Ok(())
    }
}
/// Maximum complete manual-order membership.
pub const MAXIMUM_ORDER_MEMBERS: usize = 1024;
/// Maximum encoded complete membership document.
pub const MAXIMUM_ORDER_SEED_BYTES: usize = 128 * 1024;
/// Exact member and its dictionary-encoded execution coordinate group.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreOrderMember {
    /// Latest qualifying activity from the membership read snapshot.
    pub last_activity_ms: u64,
    /// Stable durable identity.
    pub session: SessionId,
    /// Index in the complete seed's coordinate dictionary.
    pub group: u16,
}
/// Complete membership or explicit inability to enumerate within the contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreOrderSeed {
    /// All selected identities, strictly increasing and without duplicates.
    Available {
        /// Unique execution coordinates in first-member order.
        groups: Vec<ExecutionCoordinates>,
        /// Complete members; consumers reconcile before paging summaries.
        members: Vec<StoreOrderMember>,
    },
    /// Manual ordering must pause without overwriting the saved order.
    TooLarge,
}
impl StoreOrderSeed {
    /// Builds a complete seed from ordered identities and coordinates in one snapshot.
    ///
    /// # Panics
    /// Panics if the fixed membership bound can no longer fit a group index.
    pub fn from_members(rows: Vec<(SessionId, ExecutionCoordinates, u64)>) -> Result<Self> {
        if rows.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(StoreError::Corrupt(
                "order members are not strictly increasing".into(),
            ));
        }
        if rows.len() > MAXIMUM_ORDER_MEMBERS {
            return Ok(Self::TooLarge);
        }
        let mut lookup = std::collections::BTreeMap::new();
        let mut groups = Vec::new();
        let mut members = Vec::with_capacity(rows.len());
        for (session, coordinates, last_activity_ms) in rows {
            if last_activity_ms == 0 {
                return Err(StoreError::Corrupt("invalid member activity".into()));
            }
            let group = *lookup.entry(coordinates.clone()).or_insert_with(|| {
                let index = u16::try_from(groups.len()).expect("bounded membership");
                groups.push(coordinates);
                index
            });
            members.push(StoreOrderMember {
                last_activity_ms,
                session,
                group,
            });
        }
        let seed = Self::Available { groups, members };
        if serde_json::to_vec(&seed)
            .map_err(|_| StoreError::Corrupt("invalid order membership".into()))?
            .len()
            > MAXIMUM_ORDER_SEED_BYTES
        {
            Ok(Self::TooLarge)
        } else {
            Ok(seed)
        }
    }
}
/// Projects this commit's qualifying timestamps; zero means no activity.
/// Same-commit successor acceptance is queue editing, not new human input.
pub fn appended_activity(
    header: Option<&SessionHeader>,
    facts: &[std::sync::Arc<SessionFact>],
    controls: &[AgentControlRecord],
) -> u64 {
    let mut projection = ActivityProjection::default();
    for fact in facts {
        projection.observe_fact(fact);
    }
    for record in controls {
        projection.observe_control(record);
    }
    projection
        .latest()
        .max(header.map_or(0, SessionHeader::created_at_ms))
}

/// Constant-space canonical projection over validated, sequence-ordered records.
/// Queue validation establishes that a successor link and acceptance are adjacent.
#[derive(Debug, Default)]
pub struct ActivityProjection {
    latest: u64,
    successor: Option<rsi_agent_session_protocol::MessageId>,
}
impl ActivityProjection {
    /// Incorporates a qualifying Fact without retaining its body.
    pub fn observe_fact(&mut self, fact: &SessionFact) {
        if is_activity_fact(fact.body()) {
            self.latest = self.latest.max(fact.timestamp_ms());
        }
    }
    /// Incorporates new human input, excluding the immediately linked queue successor.
    /// Feed contiguous controls from suffixes accepted by [`crate::validate_queue_suffix`].
    /// That validator keeps successor, acceptance and receipt in one atomic batch;
    /// resetting this projection between validated batches cannot lose suppression.
    pub fn observe_control(&mut self, record: &AgentControlRecord) {
        let successor = self.successor.take();
        match record.body() {
            AgentControlRecordBody::MessageSuccessor { successor_id, .. } => {
                self.successor = Some(successor_id.clone());
            }
            AgentControlRecordBody::MessageAccepted { message, .. }
                if matches!(message.source, AgentMessageSource::Human)
                    && successor.as_ref() != Some(&message.message_id) =>
            {
                self.latest = self.latest.max(record.timestamp_ms());
            }
            _ => {}
        }
    }
    /// Returns the greatest qualifying timestamp, or zero for an empty projection.
    pub const fn latest(&self) -> u64 {
        self.latest
    }
}

/// Closed activity whitelist, shared by append and full canonical verification.
pub fn is_activity_fact(body: &SessionFactBody) -> bool {
    matches!(
        body,
        SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::ImageRequested { .. }
            | SessionFactBody::CancelRequested { .. }
            | SessionFactBody::ImageOutput { .. }
            | SessionFactBody::ToolResult { .. }
            | SessionFactBody::ToolRejected { .. }
            | SessionFactBody::ModelEvent {
                purpose: ModelEventPurpose::Conversation,
                event: LanguageEvent::ContentDelta { .. }
                    | LanguageEvent::Source { .. }
                    | LanguageEvent::Finished { .. }
                    | LanguageEvent::Failed { .. },
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, EffectId, MessageDelivery, MessageId, MessageOptions,
        MessageTarget, QueueSlot, TurnId,
    };
    #[test]
    fn conversation_content_counts_but_compaction_and_bookkeeping_do_not() {
        let event = LanguageEvent::ContentDelta {
            index: 0,
            delta: rsi_ai_protocol::ContentDelta::Text("text".into()),
        };
        let body = |purpose| SessionFactBody::ModelEvent {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("model").unwrap(),
            event: event.clone(),
            purpose,
        };
        assert!(is_activity_fact(&body(ModelEventPurpose::Conversation)));
        assert!(!is_activity_fact(&body(
            ModelEventPurpose::ContextCompaction
        )));
        assert!(!is_activity_fact(&SessionFactBody::ModelStarted {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("model").unwrap()
        }));
    }
    fn human(seq: u64, time: u64, id: &str) -> AgentControlRecord {
        AgentControlRecord::new(
            seq,
            time,
            AgentControlRecordBody::MessageAccepted {
                message: AgentMessage {
                    message_id: MessageId::new(id).unwrap(),
                    source: AgentMessageSource::Human,
                    content: vec![AgentMessageContent::Text {
                        text: "input".into(),
                    }],
                    options: MessageOptions::default(),
                },
                delivery: MessageDelivery::NextTurn,
                bound_turn_id: None,
                root_session_id: SessionId::new("session").unwrap(),
                target: MessageTarget::NextTurn,
                wake_required: true,
            },
        )
        .unwrap()
    }
    #[test]
    fn queue_successor_does_not_create_human_activity_and_new_input_does() {
        let original = human(1, 20, "original");
        let link = AgentControlRecord::new(
            2,
            500,
            AgentControlRecordBody::MessageSuccessor {
                predecessor_id: MessageId::new("original").unwrap(),
                successor_id: MessageId::new("successor").unwrap(),
                slot: QueueSlot::initial(&MessageId::new("original").unwrap(), 20, 1),
            },
        )
        .unwrap();
        let edited = human(3, 500, "successor");
        assert_eq!(appended_activity(None, &[], &[original, link, edited]), 20);
        assert_eq!(appended_activity(None, &[], &[human(4, 30, "fresh")]), 30);
    }
    #[test]
    fn complete_order_seed_enforces_cardinality_and_encoded_bytes_independently() {
        let coordinates = ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/project",
        )
        .unwrap();
        let small = (0..1024)
            .map(|i| {
                (
                    SessionId::new(format!("id-{i:04}")).unwrap(),
                    coordinates.clone(),
                    1,
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            StoreOrderSeed::from_members(small.clone()).unwrap(),
            StoreOrderSeed::Available { .. }
        ));
        let mut too_many = small;
        too_many.push((SessionId::new("id-1024").unwrap(), coordinates.clone(), 1));
        assert_eq!(
            StoreOrderSeed::from_members(too_many).unwrap(),
            StoreOrderSeed::TooLarge
        );
        let large = (0..1024)
            .map(|i| {
                (
                    SessionId::new(format!("{i:04}{}", "x".repeat(252))).unwrap(),
                    coordinates.clone(),
                    1,
                )
            })
            .collect();
        assert_eq!(
            StoreOrderSeed::from_members(large).unwrap(),
            StoreOrderSeed::TooLarge
        );
    }
}
