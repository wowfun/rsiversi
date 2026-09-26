//! Typed domain mutation inputs and canonical durable receipts.

use crate::{Result, TurnError};
use rsi_agent_composition_protocol::ValidatedDomainProposal;
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, DomainIdentity, DomainRequestId, DomainRevision,
    DomainStateCommit, SessionFactBody, SessionId,
};

/// Read-only prerequisite checked under the same Session admission as a domain commit.
#[derive(Clone, Debug)]
pub struct DomainReadGuard {
    /// Exact codec identity of the observed domain.
    pub domain: DomainIdentity,
    /// Revision whose state authorized the mutation.
    pub revision: DomainRevision,
}

/// One source-free execution request; only Kernel may assign its Turn provenance.
#[derive(Debug)]
pub struct DomainMutation {
    /// Fences cancellation at mutation admission, including exact Tool-result settlement.
    /// An already committed same-request receipt remains authoritative.
    pub require_uncancelled_turn: bool,
    /// Stable caller-allocated idempotency identity.
    pub request_id: DomainRequestId,
    /// Bounded read dependencies. Canonical committed retries precede these checks.
    pub guards: Vec<DomainReadGuard>,
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
