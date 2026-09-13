//! Frozen semantic-compaction inputs; model output remains in its ordinary events.

use crate::{EffectId, Result, SessionError, SessionId, TurnId, validate_sha256};
use serde::{Deserialize, Serialize};

/// Maximum compact JSON bytes for a frozen plan, including source bindings.
pub const MAXIMUM_COMPACTION_PLAN_BYTES: usize = 256 * 1024;

/// Purpose of one serial model effect, fixed before provider I/O.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "plan",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ModelPurpose {
    /// Ordinary assistant interaction.
    Conversation,
    /// Internal summary; never an assistant final answer or a Tool producer.
    ContextCompaction(Box<ContextCompactionPlan>),
}

/// Self-contained interpretation for a model event in a partial history page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEventPurpose {
    /// Ordinary assistant output.
    Conversation,
    /// Internal context summary output, with its plan owned by the intent.
    ContextCompaction,
}

impl ModelPurpose {
    pub(crate) fn validate_snapshot(
        &self,
        snapshot: &rsi_ai_protocol::PreparedCallSnapshot,
    ) -> Result<()> {
        crate::validate_snapshot_capability(
            snapshot,
            rsi_ai_protocol::AiCapability::Language,
            "Agent model intent must target the Language capability",
        )?;
        if let Self::ContextCompaction(plan) = self {
            plan.validate()?;
        }
        Ok(())
    }

    /// Returns the small event tag checked against this exact intent.
    pub const fn event_purpose(&self) -> ModelEventPurpose {
        match self {
            Self::Conversation => ModelEventPurpose::Conversation,
            Self::ContextCompaction(_) => ModelEventPurpose::ContextCompaction,
        }
    }
}

/// Exact builder implementation and normalized configuration binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionBuilder {
    /// Stable contribution identity.
    pub id: String,
    /// Semantic version controlling summary interpretation.
    pub semantic_version: String,
    /// Canonical builder configuration digest.
    pub config_sha256: String,
}

/// Complete visible Facts of one Turn inside an exact source interval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionSource {
    /// Originating Session, including a selected fork parent.
    pub session: SessionId,
    /// Turn whose visible Facts contribute to this digest.
    pub turn: TurnId,
    /// Exclusive source sequence start.
    pub after_seq: u64,
    /// Inclusive source sequence horizon.
    pub through_seq: u64,
    /// SHA-256 chain of the selected Turn's canonical Facts in this interval.
    pub facts_sha256: String,
}

/// Contiguous complete messages selected from one current projected Turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionSelection {
    /// Exact projected Turn identity.
    pub turn: TurnId,
    /// Zero-based current message offset, before installation.
    pub first: u32,
    /// Number of complete messages replaced.
    pub count: u32,
}

/// Prior installed summary included in the new summary's input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionPrior {
    /// Originating Session; inherited sequence numbers stay in that Session.
    pub session: SessionId,
    /// Summary effect identity in the current Session.
    pub effect: EffectId,
    /// Sole installing Finished event sequence.
    pub finished_seq: u64,
    /// Digest of the prior visible summary text.
    pub text_sha256: String,
}

/// Evidence that caused one pressure event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompactionTrigger {
    /// Last successful ordinary response reported this input Usage.
    Usage {
        /// Originating Session of this successful Conversation effect.
        session: SessionId,
        /// Finished event carrying the successful attempt horizon.
        finished_seq: u64,
        /// Reported cumulative input tokens for that ordinary request.
        input_tokens: u64,
    },
    /// Canonical message count or byte precheck rejected the ordinary view.
    CanonicalLimit,
    /// Provider explicitly rejected ordinary input capacity.
    ProviderContextLimit,
}

/// Frozen versioned input and installation predicate for one summary effect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCompactionPlan {
    /// Session in which this model effect was planned.
    pub session: SessionId,
    /// Policy and summary format version; currently one.
    pub version: u32,
    /// Selected builder identity.
    pub builder: CompactionBuilder,
    /// Immutable source Header fingerprint.
    pub header_fingerprint: String,
    /// Exact newly selected source spans; the installed prior binds transitive coverage.
    pub sources: Vec<CompactionSource>,
    /// Current projected message ranges to replace, in source order.
    pub selections: Vec<CompactionSelection>,
    /// Installed prior summary, when replacing one.
    pub prior: Option<CompactionPrior>,
    /// Trigger evidence; describes input capacity, not a token estimate.
    pub trigger: CompactionTrigger,
    /// Child Fact scan horizon when the input view was frozen.
    pub through_seq: u64,
    /// Canonical ordinary message-array digest, before replacement.
    pub view_sha256: String,
    /// Canonical ordinary message-array bytes, before replacement.
    pub original_bytes: u64,
    /// Maximum visible UTF-8 summary text bytes, at most 32 KiB.
    pub maximum_text_bytes: u32,
    /// Requested output token cap, at most 8192.
    pub maximum_output_tokens: u32,
}

impl ContextCompactionPlan {
    /// Validates bounded durable data without reading source Facts or live services.
    pub fn validate(&self) -> Result<()> {
        if self.version == 0
            || self.through_seq == 0
            || self.original_bytes == 0
            || self.maximum_text_bytes == 0
            || self.maximum_text_bytes > 32 * 1024
            || self.maximum_output_tokens == 0
            || self.maximum_output_tokens > 8192
            || self.sources.is_empty()
            || self.sources.len() > 1024
            || self.selections.is_empty()
            || self.selections.len() > 4096
        {
            return Err(SessionError::Invalid(
                "invalid compaction plan version or bounds".into(),
            ));
        }
        for (value, maximum) in [
            (&self.builder.id, 256),
            (&self.builder.semantic_version, 64),
        ] {
            if value.is_empty()
                || value.len() > maximum
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
            {
                return Err(SessionError::Invalid(
                    "invalid compaction builder identity".into(),
                ));
            }
        }
        validate_sha256(
            "compaction builder configuration",
            &self.builder.config_sha256,
        )?;
        validate_sha256("compaction Header", &self.header_fingerprint)?;
        validate_sha256("compaction view", &self.view_sha256)?;
        for source in &self.sources {
            if source.after_seq >= source.through_seq {
                return Err(SessionError::Invalid(
                    "empty compaction source interval".into(),
                ));
            }
            validate_sha256("compaction source", &source.facts_sha256)?;
        }
        for selection in &self.selections {
            if selection.count == 0 || selection.first.checked_add(selection.count).is_none() {
                return Err(SessionError::Invalid(
                    "invalid compaction message selection".into(),
                ));
            }
        }
        if let Some(prior) = &self.prior {
            if prior.finished_seq == 0
                || (prior.session == self.session && prior.finished_seq > self.through_seq)
            {
                return Err(SessionError::Invalid(
                    "compaction prior is outside its horizon".into(),
                ));
            }
            validate_sha256("compaction prior summary", &prior.text_sha256)?;
        }
        if let CompactionTrigger::Usage {
            session,
            finished_seq,
            ..
        } = &self.trigger
            && (*finished_seq == 0
                || (session == &self.session && *finished_seq > self.through_seq))
        {
            return Err(SessionError::Invalid(
                "compaction Usage is outside its horizon".into(),
            ));
        }
        if serde_json::to_vec(self)
            .map_err(|error| SessionError::Invalid(error.to_string()))?
            .len()
            > MAXIMUM_COMPACTION_PLAN_BYTES
        {
            return Err(SessionError::Invalid(
                "compaction plan exceeds encoded-byte bound".into(),
            ));
        }
        Ok(())
    }
}
