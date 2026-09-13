//! Canonical domain positions and bounded mechanical revision projection.

use crate::{Result, StoreError};
use rsi_agent_session_protocol::{
    DomainIdentity, DomainMutationSource, DomainRevision, DomainSnapshot, DomainStateCommit,
    MAXIMUM_DOMAIN_BASELINE_BYTES, MAXIMUM_SESSION_DOMAINS,
};

/// Derived Turn-attributed control usage at one durable Session snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StoreTurnDomainUsage {
    /// Fact tail of the selected snapshot.
    pub durable_fact_seq: u64,
    /// Control tail of the selected snapshot.
    pub durable_control_seq: u64,
    /// Number of canonical domain request controls from this Turn.
    pub records: u64,
    /// Complete canonical envelope bytes of those controls.
    pub bytes: u64,
}

/// Derived location of a complete domain state; no duplicate state bytes are indexed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreDomainHead {
    /// Exact domain codec identity copied from the canonical update.
    pub identity: DomainIdentity,
    /// Successor revision represented by that update.
    pub revision: DomainRevision,
    /// Canonical control containing the replacement.
    pub control_seq: u64,
    /// Zero-based update offset inside that control.
    pub update_index: usize,
    /// Canonical encoded snapshot bytes, used only for derived capacity admission.
    pub snapshot_bytes: usize,
}

/// One opaque state and its exact canonical source position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreDomainState {
    /// Canonical position and derived revision.
    pub head: StoreDomainHead,
    /// Complete canonical value; semantic decoding requires the owning codec.
    pub snapshot: DomainSnapshot,
}

/// Complete bounded domain set at an explicit control horizon in one Store snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreDomainStatePage {
    /// Durable Fact tail of the same read snapshot.
    pub durable_fact_seq: u64,
    /// Durable control tail of the same read snapshot.
    pub durable_control_seq: u64,
    /// Selected as-of horizon, at most the read-time control tail.
    pub selected_control_seq: u64,
    /// Complete domain states in ascending durable-name order.
    pub states: Vec<StoreDomainState>,
}

impl StoreDomainStatePage {
    /// Validates positions, complete-set bounds and canonical snapshot correspondence.
    pub fn validate(&self) -> Result<()> {
        if self.selected_control_seq > self.durable_control_seq {
            return Err(StoreError::Corrupt(
                "domain horizon exceeds the read snapshot".into(),
            ));
        }
        let heads: Vec<_> = self.states.iter().map(|entry| entry.head.clone()).collect();
        validate_domain_heads(&heads)?;
        for entry in &self.states {
            if entry.head.control_seq > self.selected_control_seq
                || &entry.head.identity != entry.snapshot.identity()
                || entry.head.snapshot_bytes
                    != entry
                        .snapshot
                        .encoded_len()
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?
            {
                return Err(StoreError::Corrupt(
                    "domain snapshot disagrees with its canonical position".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Projects one canonical request into derived positions without changing its predecessor set.
/// Store adapters separately validate Turn provenance and request uniqueness in their transaction.
pub fn domain_heads_after(
    heads: &[StoreDomainHead],
    control_seq: u64,
    commit: &DomainStateCommit,
) -> Result<Vec<StoreDomainHead>> {
    validate_domain_heads(heads)?;
    let baseline = matches!(commit.source(), DomainMutationSource::Baseline);
    if let DomainMutationSource::Command { invocation }
    | DomainMutationSource::Continuation { invocation, .. } = commit.source()
        && invocation.expected_revision
            != (rsi_agent_session_protocol::CommandRevision::Durable {
                control_seq: control_seq.saturating_sub(1),
            })
    {
        return Err(StoreError::Invalid(
            "command control revision differs from its canonical predecessor".into(),
        ));
    }
    if control_seq == 0 || (baseline && (control_seq != 1 || !heads.is_empty())) {
        return Err(StoreError::Invalid(
            "domain baseline must be the first control of a fresh empty set".into(),
        ));
    }
    let mut next = heads.to_vec();
    for (update_index, update) in commit.updates().iter().enumerate() {
        let identity = update.snapshot().identity();
        let slot = next.binary_search_by(|head| head.identity.id().cmp(identity.id()));
        let actual = slot
            .as_ref()
            .map_or(DomainRevision::new(0), |index| next[*index].revision);
        if actual != update.expected_revision() {
            return Err(StoreError::DomainRevisionConflict {
                domain: identity.id().into(),
                expected: update.expected_revision().get(),
                actual: actual.get(),
            });
        }
        if let Ok(index) = slot {
            if next[index].identity != *identity || next[index].control_seq >= control_seq {
                return Err(StoreError::Invalid(
                    "domain codec or canonical order cannot change within a Session".into(),
                ));
            }
        } else if !baseline {
            return Err(StoreError::Invalid(
                "only a fresh baseline may create a domain".into(),
            ));
        }
        let head = StoreDomainHead {
            identity: identity.clone(),
            revision: update.revision(),
            control_seq,
            update_index,
            snapshot_bytes: update
                .snapshot()
                .encoded_len()
                .map_err(|error| StoreError::Invalid(error.to_string()))?,
        };
        match slot {
            Ok(index) => next[index] = head,
            Err(index) => next.insert(index, head),
        }
    }
    validate_domain_heads(&next).map_err(|error| StoreError::Invalid(error.to_string()))?;
    Ok(next)
}

fn validate_domain_heads(heads: &[StoreDomainHead]) -> Result<()> {
    if heads.len() > MAXIMUM_SESSION_DOMAINS
        || heads
            .windows(2)
            .any(|pair| pair[0].identity.id() >= pair[1].identity.id())
    {
        return Err(StoreError::Corrupt(
            "domain heads exceed their count or name-order bounds".into(),
        ));
    }
    let mut bytes = 0_usize;
    for head in heads {
        if head.control_seq == 0
            || head.revision.get() == 0
            || head.update_index >= MAXIMUM_SESSION_DOMAINS
            || head.snapshot_bytes == 0
        {
            return Err(StoreError::Corrupt(
                "invalid domain canonical position".into(),
            ));
        }
        bytes = bytes
            .checked_add(head.snapshot_bytes)
            .ok_or_else(|| StoreError::Corrupt("domain capacity arithmetic overflow".into()))?;
    }
    if bytes > MAXIMUM_DOMAIN_BASELINE_BYTES {
        return Err(StoreError::Corrupt(
            "complete domain state set exceeds its byte capacity".into(),
        ));
    }
    Ok(())
}
