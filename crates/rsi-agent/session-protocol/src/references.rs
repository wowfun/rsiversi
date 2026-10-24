//! Pure bounded reference extraction from exact Domain codecs.

use crate::{DomainIdentity, DomainStateValue, SessionId};
use std::{any::TypeId, collections::BTreeSet, fmt};

/// Maximum distinct durable references in one canonical record.
pub const MAXIMUM_RECORD_REFERENCES: usize = 1024;

/// A typed durable edge; arbitrary strings and Media addresses are not Agent-CAS roots.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DurableReference {
    /// Another Session retained by this record.
    Session(SessionId),
    /// An exact immutable Agent-CAS object.
    Cas {
        /// Lowercase SHA-256 of the complete object.
        sha256: String,
        /// Exact nonzero encoded byte length.
        byte_len: u64,
    },
}

/// Bounded deduplicating output owned by the canonical record reader.
#[derive(Debug, Default)]
pub struct ReferenceCollector(BTreeSet<DurableReference>);
impl ReferenceCollector {
    /// Adds one typed Session edge, rejecting overflow before allocation.
    pub fn session(&mut self, session: &SessionId) -> Result<(), String> {
        self.insert(DurableReference::Session(session.clone()))
    }
    /// Adds one typed CAS edge after validating its durable scalar bounds.
    pub fn cas(&mut self, sha256: &str, byte_len: u64) -> Result<(), String> {
        crate::validate_sha256("durable CAS reference", sha256).map_err(|e| e.to_string())?;
        if byte_len == 0 || byte_len > 64 * 1024 * 1024 {
            return Err("durable CAS reference length exceeds its bound".into());
        }
        self.insert(DurableReference::Cas {
            sha256: sha256.into(),
            byte_len,
        })
    }
    fn insert(&mut self, reference: DurableReference) -> Result<(), String> {
        if !self.0.contains(&reference) && self.0.len() == MAXIMUM_RECORD_REFERENCES {
            return Err("canonical record exceeds its durable reference limit".into());
        }
        self.0.insert(reference);
        Ok(())
    }
    /// Iterates exact sorted edges without materializing a second collection.
    pub fn iter(&self) -> impl Iterator<Item = &DurableReference> {
        self.0.iter()
    }
}

/// Immutable effect-free exact-version decoder retained independently of execution.
pub trait DomainReferenceCodec: fmt::Debug + Send + Sync + 'static {
    /// Exact durable Domain identity/version.
    fn identity(&self) -> &DomainIdentity;
    /// Process-local definition identity; rebinding one version to another policy is forbidden.
    fn definition_key(&self) -> (TypeId, usize, usize);
    /// Decodes and validates the typed state before visiting its references.
    fn references(
        &self,
        state: &DomainStateValue,
        output: &mut ReferenceCollector,
    ) -> Result<(), String>;
}

/// Store-owned admission of pure codecs, without retaining their composition generation.
pub trait ReferenceCodecRegistration: fmt::Debug + Send + Sync + 'static {
    /// Installs a complete bounded set atomically; an exact identity cannot change semantics.
    fn install(&self, codecs: Vec<std::sync::Arc<dyn DomainReferenceCodec>>) -> Result<(), String>;
}
/// Optional Local maintenance capability supplied by the durable Store owner.
#[derive(Debug)]
pub struct ReferenceCodecRegistrationContract;
impl rsi_meta_contract::LocalContract for ReferenceCodecRegistrationContract {
    const KEY: &'static str = "rsi.agent.reference-codecs";
    type Service = dyn ReferenceCodecRegistration;
}
