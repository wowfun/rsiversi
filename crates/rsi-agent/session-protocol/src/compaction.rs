//! Frozen semantic-compaction inputs; model output remains in its ordinary events.

use crate::{EffectId, Result, SessionError, SessionId, TurnId, validate_sha256};
use serde::{Deserialize, Serialize};

/// A trusted application's replacement for an inclusive span of the current model input.
#[derive(schemars::JsonSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCheckpoint {
    /// Enabled plugin whose summarizer produced the replacement.
    pub plugin: String,
    /// Native Fact prefix against which the application selected its range.
    pub through_seq: u64,
    /// First retained message after the protected system prompt, zero-based.
    pub start: u32,
    /// Last message, inclusive.
    pub end: u32,
    /// Exact user-role checkpoint, including the plugin's framing.
    pub message: rsi_ai_protocol::Message,
    /// Optional half-open range of replacement blocks containing the human-readable summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_blocks: Option<std::ops::Range<u32>>,
    /// Application presentation and provenance fields, bounded by the owning Fact envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<serde_json::Value>,
}

impl PluginCheckpoint {
    /// Checks externally supplied provenance, coordinates and replacement content.
    pub fn validate(&self) -> Result<()> {
        crate::validate_plugin_name(&self.plugin)?;
        if self.through_seq == 0 || self.start > self.end {
            return Err(SessionError::Invalid(
                "invalid plugin checkpoint range".into(),
            ));
        }
        Self::validate_message(&self.message)?;
        if self.summary_blocks.as_ref().is_some_and(|range| {
            range.start > range.end || range.end as usize > self.message.content().len()
        }) {
            return Err(SessionError::Invalid(
                "plugin summary blocks are outside the checkpoint".into(),
            ));
        }
        Ok(())
    }

    /// A checkpoint may contribute text with user authority.
    pub fn validate_message(message: &rsi_ai_protocol::Message) -> Result<()> {
        if message.role() != rsi_ai_protocol::MessageRole::User
            || !message
                .content()
                .iter()
                .all(|part| matches!(part, rsi_ai_protocol::MessageContent::Text { .. }))
        {
            return Err(SessionError::Invalid(
                "plugin checkpoint requires a user text message".into(),
            ));
        }
        Ok(())
    }
}

/// A paid compaction owned by an explicit command on an idle session.
/// The session fact envelope supplies ordering and time for each transition.
#[derive(schemars::JsonSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManualCompactionEvent {
    /// Exact source selection and prepared provider request, before dispatch.
    Prepared(Box<PreparedManualCompaction>),
    /// The paid call may have begun. A missing settlement carries no replay authority.
    Started,
    /// Normalized provider stream. Context derives installation from its actual output.
    Settled {
        /// Complete provider attempt, including failure and partial output.
        attempt: crate::ModelAttemptLog,
    },
    /// The pinned context builder installed the preceding settlement as its summary.
    Installed,
}

/// Frozen request and source selection for one idle summary.
#[derive(schemars::JsonSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct PreparedManualCompaction {
    /// Exact executable composition that selected the context builder.
    pub generation: crate::GenerationSelection,
    /// Frozen real conversation sources and applicability checks.
    pub plan: ContextCompactionPlan,
    /// Prepared route, usage limits and request fingerprint.
    pub snapshot: rsi_ai_protocol::PreparedCallSnapshot,
    /// Tariff in force for this request.
    pub price_quote: Option<crate::PriceQuote>,
    /// Canonical request evidence, subject to the ordinary capture budget.
    pub evidence: crate::RequestEvidence,
}

impl ManualCompactionEvent {
    /// Physical stage name used by the durable command index.
    pub const fn stage(&self) -> &'static str {
        match self {
            Self::Prepared(_) => "prepared",
            Self::Started => "started",
            Self::Settled { .. } => "settled",
            Self::Installed => "installed",
        }
    }
    /// The exact immediately preceding stage of the same paid effect.
    pub const fn predecessor(&self) -> Option<&'static str> {
        match self {
            Self::Prepared(_) => None,
            Self::Started => Some("prepared"),
            Self::Settled { .. } => Some("started"),
            Self::Installed => Some("settled"),
        }
    }
}

/// Borrowed projection of the common provider lifecycle for accounting and presentation.
#[derive(Clone, Copy, Debug)]
pub enum LanguageAttemptStage<'a> {
    /// Prepared request and its configured tariff.
    Prepared(
        &'a rsi_ai_protocol::PreparedCallSnapshot,
        ModelEventPurpose,
        Option<&'a crate::PriceQuote>,
    ),
    /// Durable pre-dispatch boundary.
    Started,
    /// Original normalized stream and its purpose.
    Settled(&'a crate::ModelAttemptLog, ModelEventPurpose),
}

/// Provider attempt with either a real conversation turn or a standalone command owner.
#[derive(Clone, Copy, Debug)]
pub struct LanguageAttemptFact<'a> {
    /// Absent for explicit idle compaction.
    pub turn: Option<&'a TurnId>,
    /// Stable provider effect identity.
    pub effect: &'a EffectId,
    /// Immutable stage represented by this fact.
    pub stage: LanguageAttemptStage<'a>,
}

impl crate::SessionFactBody {
    /// Transfers the selected request's evidence without copying its sections.
    pub fn into_request_evidence(self) -> Option<(ModelEventPurpose, crate::RequestEvidence)> {
        match self {
            Self::ModelIntent {
                purpose, evidence, ..
            } => Some((purpose.event_purpose(), evidence)),
            Self::ManualCompaction {
                event: ManualCompactionEvent::Prepared(prepared),
                ..
            } => Some((ModelEventPurpose::ContextCompaction, prepared.evidence)),
            _ => None,
        }
    }
    /// Evidence captured at a real prepared request of either owner.
    pub fn request_evidence(&self) -> Option<&crate::RequestEvidence> {
        match self {
            Self::ModelIntent { evidence, .. } => Some(evidence),
            Self::ManualCompaction {
                event: ManualCompactionEvent::Prepared(prepared),
                ..
            } => Some(&prepared.evidence),
            _ => None,
        }
    }
    /// Reads either owner without converting a command into a conversation turn.
    pub fn language_attempt(&self) -> Option<LanguageAttemptFact<'_>> {
        use LanguageAttemptStage as Stage;
        let (turn, effect, stage) = match self {
            Self::ModelIntent {
                turn_id,
                effect_id,
                snapshot,
                purpose,
                price_quote,
                ..
            } => (
                Some(turn_id),
                effect_id,
                Stage::Prepared(snapshot, purpose.event_purpose(), price_quote.as_ref()),
            ),
            Self::ModelStarted { turn_id, effect_id } => (Some(turn_id), effect_id, Stage::Started),
            Self::ModelAttemptSettled {
                turn_id,
                effect_id,
                purpose,
                attempt,
            } => (Some(turn_id), effect_id, Stage::Settled(attempt, *purpose)),
            Self::ManualCompaction {
                effect_id, event, ..
            } => (
                None,
                effect_id,
                match event {
                    ManualCompactionEvent::Prepared(prepared) => Stage::Prepared(
                        &prepared.snapshot,
                        ModelEventPurpose::ContextCompaction,
                        prepared.price_quote.as_ref(),
                    ),
                    ManualCompactionEvent::Installed => return None,
                    ManualCompactionEvent::Started => Stage::Started,
                    ManualCompactionEvent::Settled { attempt } => {
                        Stage::Settled(attempt, ModelEventPurpose::ContextCompaction)
                    }
                },
            ),
            _ => return None,
        };
        Some(LanguageAttemptFact {
            turn,
            effect,
            stage,
        })
    }
}

/// Maximum compact JSON bytes for a frozen plan, including source bindings.
pub const MAXIMUM_COMPACTION_PLAN_BYTES: usize = 256 * 1024;

/// Purpose of one serial model effect, fixed before provider I/O.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(schemars::JsonSchema, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEventPurpose {
    /// Ordinary assistant output.
    Conversation,
    /// Internal context summary output, with its plan owned by the intent.
    ContextCompaction,
}

impl ModelPurpose {
    /// Whether the effect summarizes a region chosen by a trusted plugin during a Turn.
    pub fn is_region(&self) -> bool {
        matches!(self, Self::ContextCompaction(plan) if matches!(plan.trigger, CompactionTrigger::Region { .. }))
    }

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
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionSelection {
    /// Exact source Session; Turn identities are Session-local.
    pub session: SessionId,
    /// Exact projected Turn identity.
    pub turn: TurnId,
    /// Zero-based current message offset, before installation.
    pub first: u32,
    /// Number of complete messages replaced.
    pub count: u32,
}

/// Prior installed summary included in the new summary's input.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompactionTrigger {
    /// An explicit request to compact eligible history in an idle Session.
    Manual,
    /// A trusted caller selected one contiguous interval in the open Turn's history.
    Region {
        /// Inclusive first conversation position after the system prompt.
        start: u32,
        /// Inclusive last conversation position after the system prompt.
        end: u32,
    },
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
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCompactionPlan {
    /// Session in which this model effect was planned.
    pub session: SessionId,
    /// Policy and summary format version; currently two.
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
        let prior_only = matches!(self.trigger, CompactionTrigger::Region { start: 0, end: 0 })
            && self.prior.is_some()
            && self.selections.is_empty();
        if matches!(self.trigger, CompactionTrigger::Region { start, end } if start > end) {
            return Err(SessionError::Invalid("reversed compaction region".into()));
        }
        if self.version == 0
            || self.through_seq == 0
            || self.original_bytes == 0
            || self.maximum_text_bytes == 0
            || self.maximum_text_bytes > 32 * 1024
            || self.maximum_output_tokens == 0
            || self.maximum_output_tokens > 8192
            || (self.sources.is_empty() && !prior_only)
            || self.sources.len() > 1024
            || (self.selections.is_empty() && !prior_only)
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
