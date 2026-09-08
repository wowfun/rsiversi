//! Typed domain mutation inputs and canonical durable receipts.

use crate::{Result, TurnError};
use rsi_agent_composition_protocol::ValidatedDomainProposal;
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, DomainRequestId, DomainStateCommit,
    SessionFactBody, SessionId,
};

/// One source-free execution request; only Kernel may assign its Turn provenance.
#[derive(Debug)]
pub struct DomainMutation {
    /// Stable caller-allocated idempotency identity.
    pub request_id: DomainRequestId,
    /// Complete typed replacements from the exact admitted composition generation.
    pub proposals: Vec<ValidatedDomainProposal>,
    /// Optional ordered generated Facts committed atomically with the replacements.
    pub facts: Vec<SessionFactBody>,
}

/// Canonical committed request; no speculative receipt or separately persisted payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainMutationReceipt {
    session_id: SessionId,
    record: AgentControlRecord,
}

impl DomainMutationReceipt {
    /// Validates a canonical non-baseline domain request record.
    pub fn new(session_id: SessionId, record: AgentControlRecord) -> Result<Self> {
        if !matches!(record.body(), AgentControlRecordBody::DomainStateCommitted { commit } if commit.request_id().is_some())
        {
            return Err(TurnError::Invariant(
                "domain receipt lacks a canonical request".into(),
            ));
        }
        Ok(Self { session_id, record })
    }
    /// Returns the owning Session.
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    /// Returns the exact committed control sequence.
    pub fn control_seq(&self) -> u64 {
        self.record.seq()
    }
    /// Returns the bounded canonical request, including revisions and optional Fact span.
    pub fn commit(&self) -> &DomainStateCommit {
        let AgentControlRecordBody::DomainStateCommitted { commit } = self.record.body() else {
            unreachable!("validated domain receipt")
        };
        commit
    }
}
