//! Canonical mailbox references for atomic input claims.

use super::{
    AgentMessage, Arc, AtomicSessionAppend, BTreeMap, BTreeSet, MAXIMUM_PENDING_AGENT_MESSAGES,
    MAXIMUM_STORE_BATCH_FACTS, MessageId, MessageTarget, Result, SessionFact, SessionFactBody,
    StepId, StoreAgentMessage, StoreError, TurnId,
};
use rsi_agent_session_protocol::AgentControlRecordBody;

/// One logical Fact supplied directly or reconstructed from a claim control.
#[derive(Debug)]
pub enum StoreFactAppend<'a> {
    /// Caller-owned validated Fact.
    Inline(&'a Arc<SessionFact>),
    /// Exact coordinates and timestamp of a `MessageClaimed` control.
    Claimed(StoreInputClaim<'a>),
}

impl StoreFactAppend<'_> {
    /// Exact sequence chosen by the caller.
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            Self::Inline(fact) => fact.seq(),
            Self::Claimed(claim) => claim.seq,
        }
    }
}

impl AtomicSessionAppend {
    /// Whether the suffix contains bodies reconstructed from canonical acceptance.
    #[must_use]
    pub fn has_referenced_claims(&self) -> bool {
        self.controls.iter().any(|record| {
            matches!(record.body(),
            AgentControlRecordBody::MessageClaimed { claimed_fact_seq, .. }
            if self.facts.binary_search_by_key(claimed_fact_seq, |fact| fact.seq()).is_err())
        })
    }
    /// Merges explicit Facts and canonical input references into one exact suffix.
    /// Supplied input Facts still have to match their accepted body at commit.
    pub fn fact_suffix(&self) -> Result<Vec<StoreFactAppend<'_>>> {
        let mut suffix = Vec::with_capacity(self.facts.len());
        let mut supplied_inputs = BTreeMap::new();
        let mut previous = self.expected_fact_seq;
        for fact in &self.facts {
            if fact.seq() <= previous {
                return Err(StoreError::Invalid(
                    "atomic Facts are not strictly ordered".into(),
                ));
            }
            previous = fact.seq();
            if let SessionFactBody::InputMessageClaimed { source, .. } = fact.body() {
                use rsi_agent_session_protocol::InputMessageSource as Source;
                let id = match source {
                    Source::Command { message_id, .. }
                    | Source::Program { message_id, .. }
                    | Source::Human { message_id, .. }
                    | Source::Application { message_id, .. }
                    | Source::Continuation { message_id, .. }
                    | Source::Agent { message_id, .. }
                    | Source::Completion { message_id, .. } => Some(message_id),
                    _ => None,
                };
                if let Some(id) = id {
                    supplied_inputs.insert(id, fact.seq());
                }
            }
            suffix.push(StoreFactAppend::Inline(fact));
        }
        let mut messages = BTreeSet::new();
        let mut coordinates = BTreeSet::new();
        for control in &self.controls {
            if let AgentControlRecordBody::MessageClaimed {
                message_id,
                claimed_fact_seq,
                turn_id,
                step_id,
                ..
            } = control.body()
            {
                if !messages.insert(message_id) || !coordinates.insert(*claimed_fact_seq) {
                    return Err(StoreError::Invalid(
                        "atomic claim repeats a message or Fact".into(),
                    ));
                }
                if supplied_inputs
                    .get(message_id)
                    .is_some_and(|seq| seq != claimed_fact_seq)
                {
                    return Err(StoreError::Invalid(
                        "claim differs from its supplied input coordinate".into(),
                    ));
                }
                if self
                    .facts
                    .binary_search_by_key(claimed_fact_seq, |fact| fact.seq())
                    .is_err()
                {
                    suffix.push(StoreFactAppend::Claimed(StoreInputClaim {
                        message_id,
                        turn_id,
                        step_id,
                        seq: *claimed_fact_seq,
                        timestamp_ms: control.timestamp_ms(),
                    }));
                }
            }
        }
        if suffix.len() > MAXIMUM_STORE_BATCH_FACTS {
            return Err(StoreError::Invalid(
                "atomic Fact suffix exceeds its record bound".into(),
            ));
        }
        if suffix.len() != self.facts.len() {
            suffix.sort_unstable_by_key(StoreFactAppend::seq);
        }
        let mut expected = self.expected_fact_seq;
        for item in &suffix {
            expected = expected
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("Fact sequence exhausted".into()))?;
            if item.seq() != expected {
                return Err(StoreError::Invalid(
                    "atomic Fact suffix is not contiguous".into(),
                ));
            }
        }
        Ok(suffix)
    }
}

/// Exact coordinates of an input reconstructed from canonical acceptance.
#[derive(Debug)]
pub struct StoreInputClaim<'a> {
    /// Accepted identity in the same Session.
    pub message_id: &'a MessageId,
    /// Owning Turn.
    pub turn_id: &'a TurnId,
    /// Owning Step.
    pub step_id: &'a StepId,
    /// Caller-chosen Fact sequence.
    pub seq: u64,
    /// Caller-chosen claim time.
    pub timestamp_ms: u64,
}

impl StoreInputClaim<'_> {
    /// Moves the accepted body into its logical Fact, without copying content.
    pub fn materialize(&self, message: AgentMessage) -> Result<SessionFact> {
        if self.message_id != &message.message_id {
            return Err(StoreError::Corrupt(
                "claim selected a different accepted message".into(),
            ));
        }
        let (source, content) = message.into_claimed_parts();
        SessionFact::new(
            self.seq,
            self.timestamp_ms,
            SessionFactBody::InputMessageClaimed {
                turn_id: self.turn_id.clone(),
                step_id: self.step_id.clone(),
                source,
                content,
            },
        )
        .map_err(|error| StoreError::Invalid(error.to_string()))
    }
}

/// Body-free identity and ordering used by the Kernel's claim selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreClaimableMessage {
    /// Exact accepted message identity.
    pub message_id: MessageId,
    /// Current delivery horizon.
    pub target: MessageTarget,
    /// Acceptance order at the captured cut.
    pub accepted_control_seq: u64,
}

/// Complete bounded selection, independently of mailbox payload pagination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreMailboxClaim {
    /// Whether waking new work is paused.
    pub inbox_paused: bool,
    /// Whether any ordinary next-Turn input precedes continuation work.
    pub has_ordinary_pending_turn: bool,
    /// Exact waking message status and execution options, when requested.
    pub selected: Option<StoreAgentMessage>,
    /// All selected pending identities in acceptance order.
    pub pending: Vec<StoreClaimableMessage>,
    /// Control watermark from this same snapshot.
    pub durable_control_seq: u64,
    /// Fact watermark from this same snapshot.
    pub durable_fact_seq: u64,
}

impl StoreMailboxClaim {
    /// Validates the selection at its Store boundary.
    pub fn validate(&self) -> Result<()> {
        if self.pending.len() > MAXIMUM_PENDING_AGENT_MESSAGES {
            return Err(StoreError::Corrupt(
                "claim selection exceeds mailbox capacity".into(),
            ));
        }
        let mut previous = 0;
        let mut messages = BTreeSet::new();
        for entry in &self.pending {
            if entry.accepted_control_seq <= previous
                || entry.accepted_control_seq > self.durable_control_seq
                || !messages.insert(&entry.message_id)
            {
                return Err(StoreError::Corrupt(
                    "invalid claim selection order or identity".into(),
                ));
            }
            previous = entry.accepted_control_seq;
        }
        if let Some(selected) = &self.selected {
            selected.validate(self.durable_control_seq)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::*;

    #[test]
    fn referenced_claims_bind_the_complete_domain_fact_span() {
        let message = AgentMessage {
            message_id: MessageId::new("message").unwrap(),
            source: AgentMessageSource::Human { attribution: None },
            content: vec![AgentMessageContent::Text {
                text: "accepted".into(),
            }],
            options: MessageOptions::default(),
        };
        let turn = TurnId::new("turn").unwrap();
        let step = StepId::new("step").unwrap();
        let claim = AgentControlRecord::new(
            1,
            1,
            AgentControlRecordBody::MessageClaimed {
                message_id: message.message_id.clone(),
                activation_id: ActivationId::new("activation").unwrap(),
                turn_id: turn.clone(),
                step_id: step.clone(),
                claimed_fact_seq: 1,
            },
        )
        .unwrap();
        let mut append = AtomicSessionAppend {
            session_id: SessionId::new("session").unwrap(),
            expected_fact_seq: 0,
            expected_control_seq: 0,
            header: None,
            facts: vec![],
            controls: vec![claim],
        };
        let suffix = append.fact_suffix().unwrap();
        let StoreFactAppend::Claimed(reference) = &suffix[0] else {
            panic!("missing reference")
        };
        let original = message.content.as_ptr();
        let fact = reference.materialize(message).unwrap();
        assert!(
            matches!(fact.body(), SessionFactBody::InputMessageClaimed { content, .. } if content.as_ptr() == original)
        );
        let domain = DomainStateCommit::new(
            Some(DomainRequestId::new("request").unwrap()),
            DomainMutationSource::Turn { turn_id: turn },
            vec![
                DomainStateUpdate::new(
                    DomainRevision::new(1),
                    DomainSnapshot::new(
                        DomainIdentity::new("fixture.state", 1).unwrap(),
                        DomainStateValue::new(true.into()).unwrap(),
                    ),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        for complete in [false, true] {
            let bound = if complete {
                domain.clone().with_facts([&fact]).unwrap()
            } else {
                domain.clone()
            };
            append.controls.truncate(1);
            append.controls.push(
                AgentControlRecord::new(
                    2,
                    1,
                    AgentControlRecordBody::DomainStateCommitted { commit: bound },
                )
                .unwrap(),
            );
            append.validate().unwrap();
            let mut check = super::super::StoreDomainFactCheck::new(&append.controls).unwrap();
            check.push(&fact).unwrap();
            assert_eq!(check.finish().is_ok(), complete);
            append.facts.push(Arc::new(fact.clone()));
            assert_eq!(append.validate().is_ok(), complete);
            append.facts.clear();
        }
        let duplicate = AgentControlRecord::new(3, 1, append.controls[0].body().clone()).unwrap();
        append.controls.push(duplicate);
        assert!(append.fact_suffix().is_err());
        append.controls = vec![
            AgentControlRecord::new(
                1,
                1,
                AgentControlRecordBody::DomainStateCommitted { commit: domain },
            )
            .unwrap(),
        ];
        append.validate().unwrap();
    }
}
