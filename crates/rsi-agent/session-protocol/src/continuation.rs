//! Durable continuation provenance; live authorization is deliberately absent.

use crate::{DomainIdentity, DomainRequestId, DomainRevision, MessageId, Result, SessionError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Complete bounded frozen text for one reserved automatic round.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationInput {
    /// Owning Goal or equivalent linked domain identity.
    pub owner: DomainRequestId,
    /// Positive allocation ordinal; allocation policy belongs to the domain.
    pub round: u64,
    /// Exact retry message identity.
    pub message_id: MessageId,
    /// Complete frozen model-visible text, at most 16 KiB UTF-8.
    pub text: String,
}

impl ContinuationInput {
    /// Revalidates durable input bounds before encoding or admission.
    pub fn validate(&self) -> Result<()> {
        if self.round == 0 {
            return Err(SessionError::Invalid(
                "continuation round must be positive".into(),
            ));
        }
        crate::validate_safe_text("continuation input", &self.text, 16 * 1024, false)
    }
    /// Exact UTF-8 payload digest, independent of controller incarnation.
    pub fn text_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(self.text.as_bytes()))
    }
}

/// Canonical allocation evidence selected before message acceptance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationProvenance {
    /// First round staged in the actual fresh baseline.
    Baseline {
        /// Complete owning domain snapshot digest, including its codec identity.
        snapshot_sha256: String,
    },
    /// A committed internal reservation command.
    Command {
        /// Exact immutable request receipt.
        request_id: DomainRequestId,
    },
}

impl ContinuationProvenance {
    /// Checks digest shape without asserting live or semantic authority.
    pub fn validate(&self) -> Result<()> {
        if let Self::Baseline { snapshot_sha256 } = self {
            crate::validate_sha256("continuation baseline digest", snapshot_sha256)?;
        }
        Ok(())
    }
}

/// Immutable mailbox origin, authenticated by Kernel continuation admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationSource {
    /// Exact state codec authorizing this reservation.
    pub domain: DomainIdentity,
    /// Domain-selected logical owner.
    pub owner: DomainRequestId,
    /// Positive allocated round.
    pub round: u64,
    /// Domain revision at original reservation publication.
    pub reserved_revision: DomainRevision,
    /// Receipt or fresh baseline containing the allocation.
    pub provenance: ContinuationProvenance,
    /// Exact frozen text digest; message options must remain empty.
    pub text_sha256: String,
}

impl ContinuationSource {
    /// Checks one exact frozen text body without copying potentially oversized input.
    pub fn validate_text(&self, text: &str) -> Result<()> {
        crate::validate_safe_text("continuation input", text, 16 * 1024, false)?;
        if format!("{:x}", Sha256::digest(text.as_bytes())) != self.text_sha256 {
            return Err(SessionError::Invalid(
                "continuation text changed its frozen digest".into(),
            ));
        }
        Ok(())
    }
    /// Validates durable shape without granting execution authority.
    pub fn validate(&self) -> Result<()> {
        if self.round == 0 || self.reserved_revision.get() == 0 {
            return Err(SessionError::Invalid(
                "continuation source lacks a positive reservation".into(),
            ));
        }
        self.provenance.validate()?;
        crate::validate_sha256("continuation text digest", &self.text_sha256)
    }
}
