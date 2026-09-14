use crate::{Result, SessionError};
use rsi_agent_session_protocol::{
    EvidenceSection, EvidenceUnavailable, MAXIMUM_REQUEST_EVIDENCE_BYTES,
};
use serde::{Deserialize, Serialize};

/// Exact attached-Session request section and bounded source page.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRead {
    /// Positive sequence of the exact prepared intent.
    pub intent_seq: u64,
    /// Requested source section.
    pub section: EvidenceSection,
    /// Requested decoded source byte offset, aligned forward to a scalar.
    pub offset: u32,
    /// Maximum returned source bytes, between four and 256 KiB.
    pub maximum_bytes: u32,
}
impl EvidenceRead {
    /// Validates the request before Store or remote API access.
    pub fn validate(&self) -> Result<()> {
        if self.intent_seq == 0
            || self.offset as usize > MAXIMUM_REQUEST_EVIDENCE_BYTES
            || !(4..=256 * 1024).contains(&self.maximum_bytes)
        {
            return Err(SessionError::Invalid(
                "invalid request evidence page bounds".into(),
            ));
        }
        Ok(())
    }
}

/// Bounded exact source bytes or an explicit package-level absence.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidencePageContent {
    /// One contiguous source window; identity refers to the complete section.
    Available {
        /// Complete section's lowercase SHA-256.
        sha256: String,
        /// Complete decoded section length.
        total_bytes: u32,
        /// Actual scalar-aligned first byte.
        start: u32,
        /// Exact source bytes without presentation labels.
        text: String,
        /// Whether another page follows this text.
        more: bool,
    },
    /// The intent explicitly omitted all request evidence.
    Unavailable {
        /// Original persisted omission reason.
        reason: EvidenceUnavailable,
    },
}
/// Response identity is independent of any original section's storage location.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidencePage {
    /// Echoed selected intent, regardless of original inline storage location.
    pub intent_seq: u64,
    /// Echoed requested section.
    pub section: EvidenceSection,
    /// Exact page or a typed absence.
    pub content: EvidencePageContent,
}
impl EvidencePage {
    /// Validates remote response identity, scalar-aligned progress and bounded payload.
    pub fn validate(&self, request: &EvidenceRead) -> Result<()> {
        request.validate()?;
        if self.intent_seq != request.intent_seq || self.section != request.section {
            return Err(SessionError::Invalid(
                "request evidence response identity mismatch".into(),
            ));
        }
        if let EvidencePageContent::Available {
            sha256,
            total_bytes,
            start,
            text,
            more,
        } = &self.content
        {
            let end = (*start as usize)
                .checked_add(text.len())
                .ok_or_else(|| SessionError::Invalid("request evidence page overflow".into()))?;
            let expected_start = request.offset.min(*total_bytes);
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                || *total_bytes as usize > MAXIMUM_REQUEST_EVIDENCE_BYTES
                || text.len() > request.maximum_bytes as usize
                || *start < expected_start
                || *start - expected_start > 3
                || end > *total_bytes as usize
                || *more != (end < *total_bytes as usize)
                || (*more && text.is_empty())
            {
                return Err(SessionError::Invalid(
                    "invalid request evidence source window".into(),
                ));
            }
        }
        Ok(())
    }
}
