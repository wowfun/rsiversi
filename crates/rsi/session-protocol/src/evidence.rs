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
    /// Request names and a comparison with the preceding comparable intent.
    #[serde(default)]
    pub tool_summary: bool,
    /// Requested decoded source byte offset, aligned forward to a scalar.
    pub offset: u32,
    /// Maximum returned source bytes, between four and 256 KiB.
    pub maximum_bytes: u32,
}
impl EvidenceRead {
    /// Validates the request before Store or remote API access.
    pub fn validate(&self) -> Result<()> {
        if self.intent_seq == 0
            || (self.tool_summary && (self.section != EvidenceSection::Tools || self.offset != 0))
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
    /// Compact projection of the captured tools; schemas stay in the source section.
    ToolSummary {
        /// Function names in the recorded provider-facing order.
        names: Vec<String>,
        /// Provider-hosted tools and their options.
        hosted: Vec<rsi_ai_protocol::HostedTool>,
        /// Selection constraint in force for this intent.
        choice: rsi_ai_protocol::ToolChoice,
        /// Earlier intent and changes, if one exists.
        comparison: ToolComparison,
    },
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

/// Comparison against the preceding intent with the same purpose in this Session.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolComparison {
    /// No preceding comparable request.
    First,
    /// An earlier intent exists but its capture is unavailable.
    Unavailable {
        /// Earlier comparable intent.
        intent_seq: u64,
        /// Recorded reason the earlier capture is absent.
        reason: EvidenceUnavailable,
    },
    /// Differences between recorded definitions, including tools never called.
    Compared {
        /// Earlier comparable intent.
        intent_seq: u64,
        /// Functions newly advertised.
        added: Vec<String>,
        /// Functions no longer advertised.
        removed: Vec<String>,
        /// Retained functions with different captured definitions.
        changed: Vec<String>,
        /// Whether provider-hosted tool configuration changed.
        hosted_changed: bool,
        /// Whether the model-facing selection constraint changed.
        choice_changed: bool,
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
    /// Concise operator-facing description shared by attached Session clients.
    pub fn tool_summary_text(&self) -> Option<String> {
        use std::fmt::Write as _;
        let EvidencePageContent::ToolSummary { names, hosted, choice, comparison } = &self.content else { return None };
        let mut text = format!("Request {} · {} advertised functions\n{}", self.intent_seq, names.len(), names.join(", "));
        let _ = write!(text, "\nTool choice: {choice:?}");
        if !hosted.is_empty() { let _ = write!(text, "\nHosted tools: {hosted:?}"); }
        match comparison {
            ToolComparison::First => text.push_str("\nFirst recorded request of this kind."),
            ToolComparison::Unavailable { intent_seq, reason } => { let _ = write!(text, "\nRequest {intent_seq}: comparison unavailable ({reason:?})."); }
            ToolComparison::Compared { intent_seq, added, removed, changed, hosted_changed, choice_changed } => {
                let _ = write!(text, "\nCompared with request {intent_seq}");
                if added.is_empty() && removed.is_empty() && changed.is_empty() && !hosted_changed && !choice_changed {
                    text.push_str(": same tool definitions and choice.");
                } else {
                    for (label, list) in [("Added", added), ("Removed", removed), ("Changed", changed)] {
                        if !list.is_empty() { let _ = write!(text, "\n{label}: {}", list.join(", ")); }
                    }
                    if *hosted_changed { text.push_str("\nHosted tools changed."); }
                    if *choice_changed { text.push_str("\nTool choice changed."); }
                }
            }
        }
        Some(text)
    }

    /// Validates remote response identity, scalar-aligned progress and bounded payload.
    pub fn validate(&self, request: &EvidenceRead) -> Result<()> {
        request.validate()?;
        if self.intent_seq != request.intent_seq || self.section != request.section {
            return Err(SessionError::Invalid(
                "request evidence response identity mismatch".into(),
            ));
        }
        let validate_names = |names: &[String]| names.len() <= rsi_ai_protocol::MAX_TOOLS
            && names.iter().all(|name| !name.is_empty() && name.len() <= 256);
        let valid = match &self.content {
            EvidencePageContent::ToolSummary { names, comparison, .. } => request.tool_summary && validate_names(names) && match comparison {
                ToolComparison::First => true,
                ToolComparison::Unavailable { intent_seq, .. } => (1..request.intent_seq).contains(intent_seq),
                ToolComparison::Compared { intent_seq, added, removed, changed, .. } => (1..request.intent_seq).contains(intent_seq) && [added, removed, changed].into_iter().all(|list| validate_names(list)),
            },
            EvidencePageContent::Available { .. } => !request.tool_summary,
            EvidencePageContent::Unavailable { .. } => true,
        };
        if !valid { return Err(SessionError::Invalid("invalid tool inspection response".into())); }
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
