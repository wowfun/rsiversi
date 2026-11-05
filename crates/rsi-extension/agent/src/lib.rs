//! Staged Agent journal segments with exact canonical receipt verification.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod adapter;
mod history;
pub use history::project_model_attempt;
mod questions;
pub use adapter::AgentAdapter;
mod factory;
pub use factory::AgentAdapterFactory;

use rsi_agent_composition_protocol::{
    DomainDefinition, DomainHandle, DomainReferencePolicy, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    DomainIdentity, DomainRevision, DomainStateValue, EXTENSION_JOURNAL_DOMAIN, ExtensionJournal,
    ExtensionSegment,
};
use rsi_agent_store_protocol::SessionStore;
use rsi_agent_turn_protocol::DomainMutationReceipt;
use std::sync::Arc;

/// Adapter failure before or after the separate canonical publication boundary.
#[derive(Debug, thiserror::Error)]
#[error("extension journal: {0}")]
pub struct Error(String);
fn error(value: impl std::fmt::Display) -> Error {
    Error(value.to_string())
}

/// Defines the same built-in envelope understood by offline reference maintenance.
///
/// # Errors
/// Rejects an invalid artifact digest or initial journal envelope.
pub fn definition(artifact_sha256: String) -> Result<DomainDefinition<ExtensionJournal>, Error> {
    DomainDefinition::new(
        DomainIdentity::new(EXTENSION_JOURNAL_DOMAIN, 1).map_err(error)?,
        &ExtensionJournal {
            artifact_sha256,
            segments: vec![],
            sessions: vec![],
            checkpoint: DomainStateValue::new(serde_json::Value::Null).map_err(error)?,
        },
        ExtensionJournal::validate,
        DomainReferencePolicy::Visit(ExtensionJournal::references),
    )
    .map(|definition| {
        definition.with_fork_policy(rsi_agent_composition_protocol::DomainForkPolicy::Transform)
    })
    .map_err(error)
}

/// Prepared proposal plus immutable CAS ownership. Dropping it never publishes a mutation.
#[derive(Debug)]
pub struct PreparedJournal {
    proposal: ValidatedDomainProposal,
}
impl PreparedJournal {
    /// Borrows the exact proposal to include in one atomic owner mutation.
    pub const fn proposal(&self) -> &ValidatedDomainProposal {
        &self.proposal
    }

    /// Verifies the canonical update, including predecessor and complete snapshot.
    /// The owner still binds the receipt's Session and request identity before dispatch.
    ///
    /// # Errors
    /// Rejects a missing, conflicting or revision-exhausted update.
    pub fn verify_receipt(&self, receipt: &DomainMutationReceipt) -> Result<DomainRevision, Error> {
        let update = receipt
            .commit()
            .updates()
            .iter()
            .find(|update| update.snapshot().identity() == self.proposal.snapshot().identity())
            .ok_or_else(|| error("receipt omits the staged journal"))?;
        if update.snapshot() != self.proposal.snapshot()
            || update.expected_revision() != self.proposal.expected_revision()
        {
            return Err(error("receipt does not identify the staged journal"));
        }
        self.proposal.expected_revision().next().map_err(error)
    }
}

/// Stages one pure extension operation for the caller's subsequent atomic commit.
///
/// # Errors
/// Rejects invalid or oversized state and propagates CAS staging failures.
pub async fn prepare(
    store: &dyn SessionStore,
    handle: &DomainHandle<ExtensionJournal>,
    revision: DomainRevision,
    current: &ExtensionJournal,
    events: Vec<serde_json::Value>,
    checkpoint: DomainStateValue,
    admission: rsi_api_protocol::ByteAdmission,
) -> Result<PreparedJournal, Error> {
    current.validate().map_err(error)?;
    let mut next = current.clone();
    next.checkpoint = checkpoint;
    let mut objects = Vec::new();
    if !events.is_empty() {
        if events.len() > 4096 {
            return Err(error("operation event count exceeded"));
        }
        let bounded = DomainStateValue::encode(&events).map_err(error)?;
        let mut bytes = serde_json::to_vec(bounded.value()).map_err(error)?;
        let mut count = events.len() as u64;
        if let Some(tail) = next.segments.last()
            && tail.count + count <= 4096
            && tail.byte_len + bytes.len() as u64 - 1 <= 256 * 1024
        {
            let mut merged = restore_segment(store, tail, admission).await?;
            merged.extend(events);
            bytes = serde_json::to_vec(&merged).map_err(error)?;
            count = merged.len() as u64;
            next.segments.pop();
        }
        let first = next
            .segments
            .last()
            .map_or(0, |entry| entry.first + entry.count);
        // Validate all aggregate limits before creating a CAS object.
        next.segments.push(ExtensionSegment {
            first,
            count,
            sha256: "0".repeat(64),
            byte_len: bytes.len() as u64,
        });
        next.validate().map_err(error)?;
        let object = store.put_cas(Arc::from(bytes)).await.map_err(error)?;
        next.segments
            .last_mut()
            .ok_or_else(|| error("missing staged segment"))?
            .sha256
            .clone_from(&object.sha256);
        objects.push(object);
    }
    for segment in next
        .segments
        .iter()
        .take(next.segments.len() - objects.len())
    {
        objects.push(
            store
                .retain_cas(&rsi_agent_store_protocol::CasObjectRef {
                    sha256: segment.sha256.clone(),
                    byte_len: segment.byte_len,
                })
                .await
                .map_err(error)?,
        );
    }
    let proposal = handle
        .propose(revision, &next)
        .map_err(error)?
        .with_retained(Arc::new(objects));
    Ok(PreparedJournal { proposal })
}

/// Authenticates all immutable segments before returning the complete bounded event prefix.
///
/// # Errors
/// Rejects invalid state, unavailable or corrupt CAS bytes, and count mismatches.
pub async fn restore(
    store: &dyn SessionStore,
    journal: &ExtensionJournal,
    admission: impl Fn() -> rsi_api_protocol::ByteAdmission,
) -> Result<Vec<serde_json::Value>, Error> {
    journal.validate().map_err(error)?;
    let mut events = Vec::new();
    for segment in &journal.segments {
        events.extend(restore_segment(store, segment, admission()).await?);
    }
    Ok(events)
}

async fn restore_segment(
    store: &dyn SessionStore,
    segment: &ExtensionSegment,
    admission: rsi_api_protocol::ByteAdmission,
) -> Result<Vec<serde_json::Value>, Error> {
    let object = rsi_agent_store_protocol::CasObjectRef {
        sha256: segment.sha256.clone(),
        byte_len: segment.byte_len,
    };
    let bytes = store.read_cas(&object, admission).await.map_err(error)?;
    let part: Vec<serde_json::Value> = serde_json::from_slice(bytes.as_ref()).map_err(error)?;
    if part.len() as u64 != segment.count {
        return Err(error("segment event count mismatch"));
    }
    Ok(part)
}
