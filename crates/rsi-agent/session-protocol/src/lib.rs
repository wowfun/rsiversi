//! Validated durable session identities, immutable headers, and append-only Facts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use rsi_ai_protocol::{
    AiCapability, ImageRequest, LanguageEvent, MAX_IMAGE_OUTPUTS, ModelRef, PreparedCallSnapshot,
};
use rsi_approval_protocol::{ApprovalDecision, ApprovalOutcome};
use rsi_media_protocol::MediaRef;
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{ToolCall, ToolResult, ToolResultIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::Path;
use thiserror::Error;

/// Exact durable format accepted by this pre-release implementation.
pub const SESSION_FORMAT_VERSION: u32 = 6;
/// Maximum bytes in one session, turn, effect, profile, or error-code identity.
pub const MAXIMUM_AGENT_IDENTIFIER_BYTES: usize = 256;
/// Maximum bytes in one Agent preset directory-segment identity.
pub const MAXIMUM_AGENT_PRESET_ID_BYTES: usize = 255;
/// Maximum UTF-8 bytes in one user turn.
pub const MAXIMUM_TURN_TEXT_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 bytes in one frozen system instruction.
pub const MAXIMUM_SYSTEM_PROMPT_BYTES: usize = 128 * 1024;
/// Maximum UTF-8 bytes in one canonical workspace path.
pub const MAXIMUM_WORKSPACE_PATH_BYTES: usize = 16 * 1024;
/// Maximum compact encoded bytes in one immutable session header.
pub const MAXIMUM_SESSION_HEADER_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 bytes in one persisted safe diagnostic.
pub const MAXIMUM_AGENT_DIAGNOSTIC_BYTES: usize = 4 * 1024;
/// Maximum encoded bytes in one Fact, including one maximum Language event.
pub const MAXIMUM_SESSION_FACT_BYTES: usize = 36 * 1024 * 1024;
/// Maximum bytes in one opaque Context checkpoint cache entry.
pub const MAXIMUM_CONTEXT_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
/// Maximum Facts returned by one Store read.
pub const MAXIMUM_FACTS_PER_READ: usize = 512;
/// Hard maximum elapsed milliseconds for one accepted turn.
pub const MAXIMUM_TURN_ELAPSED_MS: u64 = 30 * 60 * 1_000;
/// Hard maximum provider attempts for one accepted turn.
pub const MAXIMUM_TURN_PROVIDER_ATTEMPTS: u64 = 64;
/// Hard maximum Tool calls for one accepted turn.
pub const MAXIMUM_TURN_TOOL_CALLS: u64 = 256;
/// Hard maximum executor-generated Facts for one accepted turn.
pub const MAXIMUM_TURN_GENERATED_FACTS: u64 = 65_536;
/// Hard maximum compact encoded bytes across executor-generated Facts.
pub const MAXIMUM_TURN_GENERATED_FACT_BYTES: u64 = 64 * 1024 * 1024;
/// Empty predecessor for the canonical durable Fact-prefix digest chain.
pub const EMPTY_FACT_PREFIX_DIGEST: [u8; 32] = [0; 32];
/// Empty predecessor for the canonical Agent-control digest chain.
pub const EMPTY_CONTROL_PREFIX_DIGEST: [u8; 32] = [0; 32];
const FACT_PREFIX_DOMAIN: &[u8] = b"rsi-agent-context-fact-prefix-v2\0";
const CONTROL_PREFIX_DOMAIN: &[u8] = b"rsi-agent-control-prefix-v1\0";
/// Maximum queued messages retained for one durable session.
pub const MAXIMUM_PENDING_AGENT_MESSAGES: usize = 64;
/// Maximum ordered content blocks retained by one Agent message.
pub const MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS: usize = 64;
/// Maximum canonical paths retained by one workspace-touch Fact.
pub const MAXIMUM_WORKSPACE_TOUCH_PATHS: usize = 64;
/// Maximum UTF-8 bytes in one Agent-to-Agent message.
pub const MAXIMUM_AGENT_MESSAGE_BYTES: usize = 64 * 1024;
/// Maximum child edges below one Agent-tree root.
pub const MAXIMUM_AGENT_TREE_DEPTH: usize = 3;
/// Maximum durable sessions retained by one Agent tree.
pub const MAXIMUM_DURABLE_AGENT_TREE_NODES: usize = 256;
/// Maximum simultaneously running Turns below one Agent-tree scheduler.
pub const MAXIMUM_RUNNING_AGENT_TREE_NODES: usize = 3;

macro_rules! string_identity {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Validated ", $kind, " identity.")]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }

        impl $name {
            #[doc = concat!("Creates a validated ", $kind, " identity.")]
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                validate_identifier($kind, &value)?;
                Ok(Self(value))
            }

            #[doc = concat!("Returns the exact ", $kind, " identity.")]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
    };
}

string_identity!(SessionId, "session");
string_identity!(TurnId, "turn");
string_identity!(EffectId, "effect");
string_identity!(MessageId, "message");
string_identity!(ActivationId, "activation");
string_identity!(StepId, "step");

/// Stable path from an Agent-tree root to one descendant.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AgentPath(Vec<u16>);

impl<'de> Deserialize<'de> for AgentPath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(Vec::<u16>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl AgentPath {
    /// Creates a root or bounded descendant path.
    pub fn new(segments: Vec<u16>) -> Result<Self> {
        if segments.len() > MAXIMUM_AGENT_TREE_DEPTH || segments.contains(&0) {
            return Err(SessionError::Invalid(format!(
                "Agent path must contain at most {MAXIMUM_AGENT_TREE_DEPTH} nonzero segments"
            )));
        }
        Ok(Self(segments))
    }

    /// Returns the root path.
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// Returns the ordered child segments.
    pub fn segments(&self) -> &[u16] {
        &self.0
    }

    /// Returns the number of child edges below the root.
    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

/// Requested completed-turn selection for one subagent fork.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkTurnSelection {
    /// Start without parent turns.
    None,
    /// Retain every available completed parent turn.
    All,
    /// Retain at most the newest positive number of completed turns.
    Last(u64),
}

impl<'de> Deserialize<'de> for ForkTurnSelection {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum WireSelection {
            None,
            All,
            Last(u64),
        }

        match WireSelection::deserialize(deserializer)? {
            WireSelection::None => Ok(Self::None),
            WireSelection::All => Ok(Self::All),
            WireSelection::Last(count) if count > 0 => Ok(Self::Last(count)),
            WireSelection::Last(_) => Err(serde::de::Error::custom(
                "fork last-turn selection must be positive",
            )),
        }
    }
}

impl ForkTurnSelection {
    /// Revalidates a programmatically constructed selection.
    pub fn validate(&self) -> Result<()> {
        if matches!(self, Self::Last(0)) {
            return Err(SessionError::Invalid(
                "fork last-turn selection must be positive".into(),
            ));
        }
        Ok(())
    }

    /// Parses the model-facing `fork_turns` value; omission is represented by [`Self::All`].
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "all" => Ok(Self::All),
            _ if value.is_empty()
                || value.len() > 20
                || !value.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                Err(SessionError::Invalid(
                    "fork_turns must be `none`, `all`, or a positive decimal u64".into(),
                ))
            }
            _ => value
                .parse::<u64>()
                .ok()
                .filter(|count| *count > 0)
                .map(Self::Last)
                .ok_or_else(|| {
                    SessionError::Invalid(
                        "fork_turns must be `none`, `all`, or a positive decimal u64".into(),
                    )
                }),
        }
    }
}

/// Immutable parent-history boundary retained by a forked session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkOrigin {
    /// Exact parent session.
    pub parent_session_id: SessionId,
    /// Exact Agent-tree root shared by every descendant.
    pub root_session_id: SessionId,
    /// Stable bounded path of this child below the root.
    pub path: AgentPath,
    /// Model-facing stable task name allocated by the parent.
    pub task_name: String,
    /// Fingerprint of the immutable parent Header.
    pub parent_header_fingerprint: String,
    /// Turn containing the spawn request and excluded from inherited history.
    pub invoking_turn_id: TurnId,
    /// Exclusive parent Fact cursor before the first inherited completed Turn.
    pub resolved_after_seq: u64,
    /// Last inherited terminal session sequence, or zero for `none`.
    pub resolved_terminal_seq: u64,
    /// Canonical Fact-prefix digest at the resolved terminal sequence.
    pub terminal_prefix_sha256: String,
    /// Caller-requested selection.
    pub requested_turns: ForkTurnSelection,
    /// Number of completed turns actually retained.
    pub effective_turns: u64,
}

impl ForkOrigin {
    /// Revalidates the immutable lineage boundary.
    pub fn validate(&self) -> Result<()> {
        self.requested_turns.validate()?;
        validate_sha256("parent header fingerprint", &self.parent_header_fingerprint)?;
        validate_sha256("fork terminal prefix", &self.terminal_prefix_sha256)?;
        validate_identifier("subagent task name", &self.task_name)?;
        if self.path.depth() == 0 {
            return Err(SessionError::Invalid(
                "fork lineage must describe a non-root child".into(),
            ));
        }
        let cursors_empty = self.resolved_after_seq == 0 && self.resolved_terminal_seq == 0;
        if (self.effective_turns == 0) != cursors_empty {
            return Err(SessionError::Invalid(
                "fork interval emptiness must match the effective turn count".into(),
            ));
        }
        let empty = cursors_empty && self.effective_turns == 0;
        if matches!(self.requested_turns, ForkTurnSelection::None) && !empty {
            return Err(SessionError::Invalid(
                "fork `none` selection must resolve to an empty parent prefix".into(),
            ));
        }
        if self.resolved_terminal_seq == 0 && !empty {
            return Err(SessionError::Invalid(
                "fork effective turn count requires a terminal boundary".into(),
            ));
        }
        if self.resolved_after_seq >= self.resolved_terminal_seq && self.resolved_terminal_seq != 0
        {
            return Err(SessionError::Invalid(
                "fork inherited Fact interval must be nonempty and ordered".into(),
            ));
        }
        Ok(())
    }
}

/// Model-visible durable content in one accepted mailbox message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Variant prose defines these transparent payload fields as one contract.
pub enum AgentMessageContent {
    /// Safe UTF-8 text.
    Text { text: String },
    /// Immutable imported image reference.
    Image { media: MediaRef },
}

/// Authority-neutral origin of one queued message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Variant prose defines each source payload as one closed contract.
pub enum AgentMessageSource {
    /// Direct application/user input.
    Human,
    /// Another Agent activation.
    Agent { source_session_id: SessionId },
    /// Child-settlement notification.
    Completion {
        child_session_id: SessionId,
        activation_id: ActivationId,
    },
}

/// Delivery horizon for one accepted message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageTarget {
    /// Enter a newly claimed Turn.
    NextTurn,
    /// Enter the next model Step of an already-running Turn.
    NextStep,
}

/// Optional execution controls which can only enter a newly claimed Turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MessageOptions {
    /// Optional exact invocation route.
    pub model: Option<ModelRef>,
    /// Optional sandbox override.
    pub sandbox: Option<SandboxMode>,
}

/// One validated message retained by the Agent control plane.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessage {
    /// Caller-preallocated durable identity.
    pub message_id: MessageId,
    /// Authority-neutral source classification.
    pub source: AgentMessageSource,
    /// Nonempty ordered mixed content.
    pub content: Vec<AgentMessageContent>,
    /// Invocation controls applied only on a new Turn.
    pub options: MessageOptions,
}

impl AgentMessage {
    /// Revalidates content, route, and byte bounds at admission.
    pub fn validate(&self) -> Result<()> {
        let text_limit = if matches!(self.source, AgentMessageSource::Human) {
            MAXIMUM_TURN_TEXT_BYTES
        } else {
            MAXIMUM_AGENT_MESSAGE_BYTES
        };
        validate_message_content(&self.content, text_limit)?;
        if let Some(model) = &self.options.model {
            model
                .validate()
                .map_err(|error| SessionError::Invalid(error.to_string()))?;
        }
        Ok(())
    }
}

fn validate_message_content(content: &[AgentMessageContent], text_limit: usize) -> Result<()> {
    if content.is_empty() || content.len() > MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS {
        return Err(SessionError::Invalid(format!(
            "Agent message must contain 1..={MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS} content blocks"
        )));
    }
    let mut text_bytes = 0_usize;
    for block in content {
        match block {
            AgentMessageContent::Text { text } => {
                validate_safe_text("Agent message text", text, text_limit, false)?;
                text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                    SessionError::Invalid("Agent message text size overflowed".into())
                })?;
            }
            AgentMessageContent::Image { media } => media
                .validate()
                .map_err(|error| SessionError::Invalid(error.to_string()))?,
        }
    }
    if text_bytes > text_limit {
        return Err(SessionError::TooLarge {
            kind: "Agent message text",
            maximum: text_limit,
            actual: text_bytes,
        });
    }
    Ok(())
}

/// Durable reason an accepted mailbox message will never be claimed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDiscardReason {
    /// Caller cancelled the message before claim.
    Cancelled,
}

/// Model-visible durable source of one entered input message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Variant prose defines each source payload as one closed contract.
pub enum InputMessageSource {
    /// Direct user/application input.
    Human { message_id: MessageId },
    /// Input sent by another Agent session.
    Agent {
        message_id: MessageId,
        source_session_id: SessionId,
    },
    /// Child-activation completion notice.
    Completion {
        message_id: MessageId,
        child_session_id: SessionId,
        activation_id: ActivationId,
    },
    /// Initial or refreshed Agent instruction text.
    AgentInstructions {
        source: String,
        sha256: String,
        replacement: bool,
        tombstone: bool,
    },
    /// Complete selected skill catalog replacement.
    SkillCatalog { sha256: String },
    /// Direct user invocation of one selected skill.
    UserSkillInvocation { name: String, source: String },
}

impl InputMessageSource {
    fn validate(&self) -> Result<()> {
        match self {
            Self::AgentInstructions { source, sha256, .. } => {
                validate_safe_text("input source", source, MAXIMUM_WORKSPACE_PATH_BYTES, false)?;
                validate_sha256("instruction digest", sha256)
            }
            Self::UserSkillInvocation { name, source } => {
                validate_identifier("skill name", name)?;
                validate_safe_text("input source", source, MAXIMUM_WORKSPACE_PATH_BYTES, false)
            }
            Self::SkillCatalog { sha256 } => validate_sha256("skill catalog digest", sha256),
            Self::Human { .. } | Self::Agent { .. } | Self::Completion { .. } => Ok(()),
        }
    }
}

/// Frozen authority for loading project-controlled instructions and skills.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrust {
    /// Project-controlled context is ignored; user-owned context remains eligible.
    #[default]
    Untrusted,
    /// Project-controlled context may enter the model under bounded discovery rules.
    Trusted,
}

/// Terminal result of one model Step within a Turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Variant prose defines each outcome payload.
pub enum StepOutcome {
    /// The Step produced a complete model result or tool request.
    Completed,
    /// The Step stopped because its Turn ended.
    Stopped { reason: String },
}

impl StepOutcome {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Completed => Ok(()),
            Self::Stopped { reason } => validate_safe_diagnostic("Step stop reason", reason),
        }
    }
}

/// Final state of one complete Agent activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Variant prose defines each closed outcome payload.
pub enum ActivationOutcome {
    /// The activation and every descendant settled successfully.
    Completed,
    /// A bounded failure ended the activation.
    Failed { code: String, message: String },
    /// Explicit close-tree cancellation ended the activation.
    Cancelled,
}

/// Winning cause which resumed one parked wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitResumeCause {
    /// A mailbox message became visible.
    Message,
    /// A child completion became visible.
    Completion,
    /// The requested wait deadline elapsed.
    Timeout,
    /// The current Turn was durably cancelled.
    Cancel,
}

/// Append-only non-Fact Agent control transition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Transition-level prose is the authoritative field contract.
pub enum AgentControlRecordBody {
    /// A bounded message entered the durable mailbox.
    MessageAccepted {
        message: AgentMessage,
        /// Durable Agent-tree root used by fair ready selection.
        root_session_id: SessionId,
        target: MessageTarget,
        wake_required: bool,
    },
    /// One message entered an exact activation/turn/step Fact boundary.
    MessageClaimed {
        message_id: MessageId,
        activation_id: ActivationId,
        turn_id: TurnId,
        step_id: StepId,
        entered_fact_seq: u64,
    },
    /// A pending next-Step completion became waking next-Turn input at activation end.
    MessagePromoted { message_id: MessageId },
    /// An accepted message was durably discarded before claim.
    MessageDiscarded {
        message_id: MessageId,
        reason: MessageDiscardReason,
    },
    /// One activation acquired its first waking message.
    ActivationStarted {
        activation_id: ActivationId,
        root_session_id: SessionId,
        parent_session_id: Option<SessionId>,
        path: AgentPath,
    },
    /// An activation has no active Turn but still owns descendants.
    ActivationWaitingForDescendants { activation_id: ActivationId },
    /// One activation truly settled after all descendants.
    ActivationSettled {
        activation_id: ActivationId,
        outcome: ActivationOutcome,
    },
    /// A wait Tool parked its executor lane.
    WaitParked {
        activation_id: ActivationId,
        turn_id: TurnId,
        step_id: StepId,
        deadline_ms: u64,
    },
    /// Exactly one contender resumed a parked wait.
    WaitResumed {
        activation_id: ActivationId,
        turn_id: TurnId,
        step_id: StepId,
        cause: WaitResumeCause,
    },
    /// Parent mailbox capacity is reserved for an eventual child completion.
    CompletionReserved {
        activation_id: ActivationId,
        parent_session_id: SessionId,
        maximum_bytes: u64,
    },
}

impl AgentControlRecordBody {
    /// Revalidates bounded values independent of Store state.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::MessageAccepted {
                message,
                root_session_id: _,
                target,
                wake_required,
            } => {
                message.validate()?;
                if *wake_required != (*target == MessageTarget::NextTurn) {
                    return Err(SessionError::Invalid(
                        "message wake requirement disagrees with its delivery target".into(),
                    ));
                }
                if *target == MessageTarget::NextStep
                    && message.options != MessageOptions::default()
                {
                    return Err(SessionError::Invalid(
                        "NextStep messages cannot carry new-Turn options".into(),
                    ));
                }
                Ok(())
            }
            Self::MessageClaimed {
                entered_fact_seq, ..
            } if *entered_fact_seq == 0 => Err(SessionError::Invalid(
                "message claim must reference a nonzero Fact sequence".into(),
            )),
            Self::ActivationStarted {
                root_session_id,
                parent_session_id,
                path,
                ..
            } => {
                if path.depth() == 0 && parent_session_id.is_some()
                    || path.depth() > 0 && parent_session_id.is_none()
                {
                    return Err(SessionError::Invalid(
                        "Agent path and parent session relationship disagree".into(),
                    ));
                }
                let _ = root_session_id;
                Ok(())
            }
            Self::ActivationSettled {
                outcome: ActivationOutcome::Failed { code, message },
                ..
            } => {
                validate_identifier("activation failure code", code)?;
                validate_safe_diagnostic("activation failure message", message)
            }
            Self::WaitParked { deadline_ms: 0, .. } => Err(SessionError::Invalid(
                "parked wait deadline must be nonzero".into(),
            )),
            Self::CompletionReserved { maximum_bytes, .. }
                if *maximum_bytes == 0
                    || *maximum_bytes
                        > u64::try_from(MAXIMUM_AGENT_MESSAGE_BYTES).unwrap_or(u64::MAX) =>
            {
                Err(SessionError::Invalid(format!(
                    "completion reservation must be within 1..={MAXIMUM_AGENT_MESSAGE_BYTES} bytes"
                )))
            }
            Self::MessageClaimed { .. }
            | Self::MessagePromoted { .. }
            | Self::MessageDiscarded { .. }
            | Self::ActivationWaitingForDescendants { .. }
            | Self::ActivationSettled { .. }
            | Self::WaitParked { .. }
            | Self::WaitResumed { .. }
            | Self::CompletionReserved { .. } => Ok(()),
        }
    }
}

/// One sequenced durable Agent-control record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentControlRecord {
    seq: u64,
    timestamp_ms: u64,
    #[serde(flatten)]
    body: AgentControlRecordBody,
    #[serde(skip)]
    encoded_len: usize,
}

impl<'de> Deserialize<'de> for AgentControlRecord {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRecord {
            seq: u64,
            timestamp_ms: u64,
            #[serde(flatten)]
            body: AgentControlRecordBody,
        }
        let wire = WireRecord::deserialize(deserializer)?;
        Self::new(wire.seq, wire.timestamp_ms, wire.body).map_err(serde::de::Error::custom)
    }
}

impl AgentControlRecord {
    /// Creates one bounded record.
    pub fn new(seq: u64, timestamp_ms: u64, body: AgentControlRecordBody) -> Result<Self> {
        if seq == 0 || timestamp_ms == 0 {
            return Err(SessionError::Invalid(
                "control sequence and timestamp must be nonzero".into(),
            ));
        }
        body.validate()?;
        let mut record = Self {
            seq,
            timestamp_ms,
            body,
            encoded_len: 0,
        };
        record.encoded_len = compact_json_len(&record)?;
        if record.encoded_len > MAXIMUM_SESSION_FACT_BYTES {
            return Err(SessionError::TooLarge {
                kind: "Agent control record",
                maximum: MAXIMUM_SESSION_FACT_BYTES,
                actual: record.encoded_len,
            });
        }
        Ok(record)
    }

    /// Returns the per-session control sequence.
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Returns the creation timestamp in Unix milliseconds.
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the validated control body.
    pub const fn body(&self) -> &AgentControlRecordBody {
        &self.body
    }

    /// Returns exact compact JSON bytes counted at construction.
    pub const fn encoded_len(&self) -> usize {
        self.encoded_len
    }
}

/// Validated durable Agent preset identity.
///
/// The lowercase alphanumeric-and-dash grammar is safe to use as one preset
/// directory segment. Filesystem resolution must still remain beneath the
/// preset provider's separately validated root.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AgentPresetId(String);

impl<'de> Deserialize<'de> for AgentPresetId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl AgentPresetId {
    /// Creates one bounded preset identity matching `[a-z0-9][a-z0-9-]*`.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let mut bytes = value.bytes();
        let valid_first = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if value.len() > MAXIMUM_AGENT_PRESET_ID_BYTES
            || !valid_first
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(SessionError::Invalid(
                "Agent preset identity must match [a-z0-9][a-z0-9-]* within the identifier bound"
                    .into(),
            ));
        }
        Ok(Self(value))
    }

    /// Returns the exact durable preset identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentPresetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for AgentPresetId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Immutable hard-stop budget for one accepted turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
pub struct TurnBudget {
    maximum_elapsed_ms: u64,
    maximum_provider_attempts: u64,
    maximum_tool_calls: u64,
    maximum_generated_facts: u64,
    maximum_generated_fact_bytes: u64,
}

impl<'de> Deserialize<'de> for TurnBudget {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(clippy::struct_field_names)]
        struct WireBudget {
            maximum_elapsed_ms: u64,
            maximum_provider_attempts: u64,
            maximum_tool_calls: u64,
            maximum_generated_facts: u64,
            maximum_generated_fact_bytes: u64,
        }

        let wire = WireBudget::deserialize(deserializer)?;
        Self::new(
            wire.maximum_elapsed_ms,
            wire.maximum_provider_attempts,
            wire.maximum_tool_calls,
            wire.maximum_generated_facts,
            wire.maximum_generated_fact_bytes,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            maximum_elapsed_ms: MAXIMUM_TURN_ELAPSED_MS,
            maximum_provider_attempts: MAXIMUM_TURN_PROVIDER_ATTEMPTS,
            maximum_tool_calls: MAXIMUM_TURN_TOOL_CALLS,
            maximum_generated_facts: MAXIMUM_TURN_GENERATED_FACTS,
            maximum_generated_fact_bytes: MAXIMUM_TURN_GENERATED_FACT_BYTES,
        }
    }
}

impl TurnBudget {
    /// Creates a positive budget no wider than the repository hard maxima.
    pub fn new(
        maximum_elapsed_ms: u64,
        maximum_provider_attempts: u64,
        maximum_tool_calls: u64,
        maximum_generated_facts: u64,
        maximum_generated_fact_bytes: u64,
    ) -> Result<Self> {
        let budget = Self {
            maximum_elapsed_ms,
            maximum_provider_attempts,
            maximum_tool_calls,
            maximum_generated_facts,
            maximum_generated_fact_bytes,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Revalidates positivity and every fixed hard maximum.
    pub fn validate(&self) -> Result<()> {
        validate_budget_dimension(
            "maximum_elapsed_ms",
            self.maximum_elapsed_ms,
            MAXIMUM_TURN_ELAPSED_MS,
        )?;
        validate_budget_dimension(
            "maximum_provider_attempts",
            self.maximum_provider_attempts,
            MAXIMUM_TURN_PROVIDER_ATTEMPTS,
        )?;
        validate_budget_dimension(
            "maximum_tool_calls",
            self.maximum_tool_calls,
            MAXIMUM_TURN_TOOL_CALLS,
        )?;
        validate_budget_dimension(
            "maximum_generated_facts",
            self.maximum_generated_facts,
            MAXIMUM_TURN_GENERATED_FACTS,
        )?;
        validate_budget_dimension(
            "maximum_generated_fact_bytes",
            self.maximum_generated_fact_bytes,
            MAXIMUM_TURN_GENERATED_FACT_BYTES,
        )
    }

    /// Returns the elapsed-time limit measured from durable acceptance time.
    pub const fn maximum_elapsed_ms(&self) -> u64 {
        self.maximum_elapsed_ms
    }

    /// Returns the provider-attempt limit.
    pub const fn maximum_provider_attempts(&self) -> u64 {
        self.maximum_provider_attempts
    }

    /// Returns the Tool-call limit.
    pub const fn maximum_tool_calls(&self) -> u64 {
        self.maximum_tool_calls
    }

    /// Returns the executor-generated Fact-count limit.
    pub const fn maximum_generated_facts(&self) -> u64 {
        self.maximum_generated_facts
    }

    /// Returns the executor-generated compact-byte limit.
    pub const fn maximum_generated_fact_bytes(&self) -> u64 {
        self.maximum_generated_fact_bytes
    }
}

fn validate_budget_dimension(name: &'static str, value: u64, maximum: u64) -> Result<()> {
    if value == 0 || value > maximum {
        return Err(SessionError::Invalid(format!(
            "{name} must be within 1..={maximum}"
        )));
    }
    Ok(())
}

/// Turn-budget dimension that prevented further work admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    /// Wall time since `TurnAccepted`.
    Elapsed,
    /// Language or Image provider attempts.
    ProviderAttempts,
    /// Tool invocations.
    ToolCalls,
    /// Executor-generated Fact count.
    GeneratedFacts,
    /// Compact encoded bytes across executor-generated Facts.
    GeneratedFactBytes,
}

impl BudgetDimension {
    /// Returns the repository hard maximum for this durable dimension.
    pub const fn hard_maximum(self) -> u64 {
        match self {
            Self::Elapsed => MAXIMUM_TURN_ELAPSED_MS,
            Self::ProviderAttempts => MAXIMUM_TURN_PROVIDER_ATTEMPTS,
            Self::ToolCalls => MAXIMUM_TURN_TOOL_CALLS,
            Self::GeneratedFacts => MAXIMUM_TURN_GENERATED_FACTS,
            Self::GeneratedFactBytes => MAXIMUM_TURN_GENERATED_FACT_BYTES,
        }
    }
}

/// Immutable redacted settings captured when one session becomes durable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenAgentSettings {
    settings_id: String,
    system_prompt: String,
    default_model: ModelRef,
    sandbox: SandboxMode,
    require_approval: bool,
    turn_budget: TurnBudget,
}

impl<'de> Deserialize<'de> for FrozenAgentSettings {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSettings {
            settings_id: String,
            system_prompt: String,
            default_model: ModelRef,
            sandbox: SandboxMode,
            require_approval: bool,
            turn_budget: TurnBudget,
        }

        let wire = WireSettings::deserialize(deserializer)?;
        Self::new_with_budget(
            wire.settings_id,
            wire.system_prompt,
            wire.default_model,
            wire.sandbox,
            wire.require_approval,
            wire.turn_budget,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl FrozenAgentSettings {
    /// Creates bounded immutable Agent settings without resolved secrets.
    pub fn new(
        settings_id: impl Into<String>,
        system_prompt: impl Into<String>,
        default_model: ModelRef,
        sandbox: SandboxMode,
        require_approval: bool,
    ) -> Result<Self> {
        Self::new_with_budget(
            settings_id,
            system_prompt,
            default_model,
            sandbox,
            require_approval,
            TurnBudget::default(),
        )
    }

    /// Creates bounded immutable Agent settings with an explicit tightened turn budget.
    pub fn new_with_budget(
        settings_id: impl Into<String>,
        system_prompt: impl Into<String>,
        default_model: ModelRef,
        sandbox: SandboxMode,
        require_approval: bool,
        turn_budget: TurnBudget,
    ) -> Result<Self> {
        let settings = Self {
            settings_id: settings_id.into(),
            system_prompt: system_prompt.into(),
            default_model,
            sandbox,
            require_approval,
            turn_budget,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Revalidates decoded Agent settings.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("Agent settings", &self.settings_id)?;
        validate_safe_text(
            "system prompt",
            &self.system_prompt,
            MAXIMUM_SYSTEM_PROMPT_BYTES,
            true,
        )?;
        self.default_model
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.turn_budget.validate()?;
        if self.sandbox == SandboxMode::DangerFullAccess && !self.require_approval {
            return Err(SessionError::Invalid(
                "danger-full-access requires live approval".into(),
            ));
        }
        Ok(())
    }

    /// Returns the exact Agent-settings identity.
    pub fn settings_id(&self) -> &str {
        &self.settings_id
    }

    /// Returns the frozen system instruction.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Returns the creation-time exact default route.
    pub const fn default_model(&self) -> &ModelRef {
        &self.default_model
    }

    /// Returns the creation-time sandbox default.
    pub const fn sandbox(&self) -> SandboxMode {
        self.sandbox
    }

    /// Returns whether live approval was required at creation.
    pub const fn require_approval(&self) -> bool {
        self.require_approval
    }

    /// Returns the immutable turn budget captured at session creation.
    pub const fn turn_budget(&self) -> &TurnBudget {
        &self.turn_budget
    }

    /// Returns the lowercase SHA-256 of the canonical redacted settings.
    pub fn fingerprint(&self) -> Result<String> {
        let bytes =
            serde_json::to_vec(self).map_err(|error| SessionError::Encoding(error.to_string()))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

/// Immutable durable session header written with the first durable admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHeader {
    format_version: u32,
    session_id: SessionId,
    created_at_ms: u64,
    canonical_cwd: String,
    workspace_trust: WorkspaceTrust,
    agent_preset_id: AgentPresetId,
    settings: FrozenAgentSettings,
    fork_origin: Option<ForkOrigin>,
}

impl<'de> Deserialize<'de> for SessionHeader {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireHeader {
            format_version: u32,
            session_id: Option<serde_json::Value>,
            created_at_ms: Option<serde_json::Value>,
            canonical_cwd: Option<serde_json::Value>,
            workspace_trust: Option<serde_json::Value>,
            agent_preset_id: Option<serde_json::Value>,
            settings: Option<serde_json::Value>,
            fork_origin: Option<ForkOrigin>,
        }

        let wire = WireHeader::deserialize(deserializer)?;
        if wire.format_version != SESSION_FORMAT_VERSION {
            return Err(serde::de::Error::custom(SessionError::UnsupportedFormat(
                wire.format_version,
            )));
        }
        let header = Self {
            format_version: wire.format_version,
            session_id: decode_header_field(wire.session_id, "session_id")?,
            created_at_ms: decode_header_field(wire.created_at_ms, "created_at_ms")?,
            canonical_cwd: decode_header_field(wire.canonical_cwd, "canonical_cwd")?,
            workspace_trust: decode_header_field(wire.workspace_trust, "workspace_trust")?,
            agent_preset_id: decode_header_field(wire.agent_preset_id, "agent_preset_id")?,
            settings: decode_header_field(wire.settings, "settings")?,
            fork_origin: wire.fork_origin,
        };
        header
            .validate()
            .map(|()| header)
            .map_err(serde::de::Error::custom)
    }
}

fn decode_header_field<T, E>(
    value: Option<serde_json::Value>,
    name: &'static str,
) -> std::result::Result<T, E>
where
    T: serde::de::DeserializeOwned,
    E: serde::de::Error,
{
    let value = value.ok_or_else(|| E::missing_field(name))?;
    serde_json::from_value(value).map_err(E::custom)
}

impl SessionHeader {
    /// Creates the exact current durable header.
    pub fn new(
        session_id: SessionId,
        created_at_ms: u64,
        canonical_cwd: impl Into<String>,
        agent_preset_id: AgentPresetId,
        settings: FrozenAgentSettings,
    ) -> Result<Self> {
        let header = Self {
            format_version: SESSION_FORMAT_VERSION,
            session_id,
            created_at_ms,
            canonical_cwd: canonical_cwd.into(),
            workspace_trust: WorkspaceTrust::Untrusted,
            agent_preset_id,
            settings,
            fork_origin: None,
        };
        header.validate()?;
        Ok(header)
    }

    /// Revalidates the exact current format and all durable fields.
    pub fn validate(&self) -> Result<()> {
        if self.format_version != SESSION_FORMAT_VERSION {
            return Err(SessionError::UnsupportedFormat(self.format_version));
        }
        if self.created_at_ms == 0 {
            return Err(SessionError::Invalid(
                "session creation timestamp must be nonzero".into(),
            ));
        }
        validate_canonical_path(&self.canonical_cwd)?;
        self.settings.validate()?;
        if let Some(origin) = &self.fork_origin {
            origin.validate()?;
            if origin.parent_session_id == self.session_id {
                return Err(SessionError::Invalid(
                    "forked session cannot name itself as its parent".into(),
                ));
            }
        }
        let encoded_len = compact_json_len(self)?;
        if encoded_len > MAXIMUM_SESSION_HEADER_BYTES {
            return Err(SessionError::TooLarge {
                kind: "session header",
                maximum: MAXIMUM_SESSION_HEADER_BYTES,
                actual: encoded_len,
            });
        }
        Ok(())
    }

    /// Returns the exact format version.
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Returns the session identity.
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the creation timestamp in Unix milliseconds.
    pub const fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    /// Returns the canonical creation-time workspace path.
    pub fn canonical_cwd(&self) -> &str {
        &self.canonical_cwd
    }

    /// Returns the immutable creation-time workspace trust decision.
    pub const fn workspace_trust(&self) -> WorkspaceTrust {
        self.workspace_trust
    }

    /// Selects the explicit creation-time workspace trust decision.
    pub fn with_workspace_trust(mut self, workspace_trust: WorkspaceTrust) -> Result<Self> {
        self.workspace_trust = workspace_trust;
        self.validate()?;
        Ok(self)
    }

    /// Returns the durable Agent preset identity selected for this session.
    pub const fn agent_preset_id(&self) -> &AgentPresetId {
        &self.agent_preset_id
    }

    /// Rebinds a process-local draft to a different validated preset identity.
    ///
    /// This does not persist a header. The caller must still submit the final
    /// header atomically with its first accepted Message or direct Turn.
    pub fn with_agent_preset_id(mut self, agent_preset_id: AgentPresetId) -> Result<Self> {
        self.agent_preset_id = agent_preset_id;
        self.validate()?;
        Ok(self)
    }

    /// Returns the frozen creation-time Agent settings.
    pub const fn settings(&self) -> &FrozenAgentSettings {
        &self.settings
    }

    /// Adds one immutable fork lineage to a not-yet-durable child Header.
    pub fn with_fork_origin(mut self, origin: ForkOrigin) -> Result<Self> {
        if self.fork_origin.is_some() {
            return Err(SessionError::Invalid(
                "session Header already contains fork lineage".into(),
            ));
        }
        self.fork_origin = Some(origin);
        self.validate()?;
        Ok(self)
    }

    /// Derives a fresh child Header while preserving the parent's exact route and policy.
    pub fn forked_child(
        &self,
        session_id: SessionId,
        created_at_ms: u64,
        origin: ForkOrigin,
    ) -> Result<Self> {
        if origin.parent_session_id != self.session_id {
            return Err(SessionError::Invalid(
                "fork origin parent does not match the source Header".into(),
            ));
        }
        Self::new(
            session_id,
            created_at_ms,
            self.canonical_cwd.clone(),
            self.agent_preset_id.clone(),
            self.settings.clone(),
        )?
        .with_workspace_trust(self.workspace_trust)?
        .with_fork_origin(origin)
    }

    /// Returns the optional immutable fork lineage.
    pub const fn fork_origin(&self) -> Option<&ForkOrigin> {
        self.fork_origin.as_ref()
    }

    /// Returns lowercase SHA-256 of the exact canonical immutable header.
    pub fn fingerprint(&self) -> Result<String> {
        let bytes =
            serde_json::to_vec(self).map_err(|error| SessionError::Encoding(error.to_string()))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

/// External effect family used in interrupted outcomes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// Language model effect.
    Model,
    /// Image model effect.
    Image,
    /// Caller-executed Tool effect.
    Tool,
}

/// Single terminal outcome of one accepted turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnOutcome {
    /// The Language stream reached a successful terminal event.
    Completed,
    /// Durable cancellation won terminal classification.
    Cancelled,
    /// A bounded runtime or provider failure ended the turn.
    Failed {
        /// Stable failure category.
        code: String,
        /// Safe bounded summary.
        message: String,
    },
    /// At least one image was committed before a later image-operation failure.
    PartialFailed {
        /// Ordered durable outputs committed before failure.
        media: Vec<MediaRef>,
        /// Stable failure category.
        code: String,
        /// Safe bounded summary.
        message: String,
    },
    /// Startup recovery observed a turn whose safe continuation was unknowable.
    Interrupted {
        /// External effect that may have started, if one existed.
        effect: Option<EffectKind>,
        /// Safe bounded reason.
        reason: String,
    },
    /// One immutable turn-budget dimension prevented further work.
    BudgetExceeded {
        /// Exhausted dimension.
        dimension: BudgetDimension,
        /// Amount already consumed when admission stopped.
        consumed: u64,
        /// Frozen limit for this turn.
        limit: u64,
    },
}

impl TurnOutcome {
    /// Revalidates bounded terminal diagnostics.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Completed | Self::Cancelled => Ok(()),
            Self::Failed { code, message } => {
                validate_identifier("turn failure code", code)?;
                validate_safe_text(
                    "turn failure message",
                    message,
                    MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
                    false,
                )
            }
            Self::PartialFailed {
                media,
                code,
                message,
            } => {
                if media.is_empty() || media.len() > usize::from(MAX_IMAGE_OUTPUTS) {
                    return Err(SessionError::Invalid(format!(
                        "partial image outcome must contain 1..={MAX_IMAGE_OUTPUTS} Media references"
                    )));
                }
                for reference in media {
                    reference
                        .validate()
                        .map_err(|error| SessionError::Invalid(error.to_string()))?;
                }
                validate_identifier("turn failure code", code)?;
                validate_safe_text(
                    "turn failure message",
                    message,
                    MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
                    false,
                )
            }
            Self::Interrupted { reason, .. } => validate_safe_text(
                "turn interruption reason",
                reason,
                MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
                false,
            ),
            Self::BudgetExceeded {
                dimension,
                consumed,
                limit,
            } => validate_budget_exhaustion(*dimension, *consumed, *limit),
        }
    }
}

/// Append-only semantic body of one durable or speculative Fact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionFactBody {
    /// One user turn entered the session log.
    TurnAccepted {
        /// Exact turn identity.
        turn_id: TurnId,
        /// Exact user text.
        text: String,
        /// Invocation-scoped model override, if present.
        model: Option<ModelRef>,
        /// Exact resolved sandbox mode for this turn.
        sandbox: SandboxMode,
        /// Whether every Tool effect requires a live one-shot approval.
        require_approval: bool,
    },
    /// One or more durable mailbox messages were atomically claimed into a Turn.
    MessageTurnAccepted {
        /// Exact turn identity.
        turn_id: TurnId,
        /// Owning Agent activation.
        activation_id: ActivationId,
        /// Nonempty ordered claimed message identities.
        message_ids: Vec<MessageId>,
        /// Invocation-scoped model override, if present.
        model: Option<ModelRef>,
        /// Exact resolved sandbox mode for this Turn.
        sandbox: SandboxMode,
        /// Whether every Tool effect requires a live one-shot approval.
        require_approval: bool,
    },
    /// One model Step began within an accepted Turn.
    StepStarted {
        /// Exact target Turn.
        turn_id: TurnId,
        /// Exact Step identity.
        step_id: StepId,
    },
    /// One accepted message or instruction entered model-visible context.
    InputMessageEntered {
        /// Exact target Turn.
        turn_id: TurnId,
        /// Exact target Step.
        step_id: StepId,
        /// Durable source classification.
        source: InputMessageSource,
        /// Nonempty ordered content.
        content: Vec<AgentMessageContent>,
    },
    /// One model Step ended before another Step or the Turn terminal.
    StepEnded {
        /// Exact target Turn.
        turn_id: TurnId,
        /// Exact Step identity.
        step_id: StepId,
        /// Closed Step outcome.
        outcome: StepOutcome,
    },
    /// Explicit workspace paths were touched by one effect.
    WorkspaceTouched {
        /// Exact target Turn.
        turn_id: TurnId,
        /// Exact target Step.
        step_id: StepId,
        /// Effect which caused the change.
        effect_id: EffectId,
        /// Nonempty bounded canonical paths.
        paths: Vec<String>,
    },
    /// One direct Image request entered the session log.
    ImageRequested {
        /// Exact turn identity.
        turn_id: TurnId,
        /// Exact Image route.
        model: ModelRef,
        /// Complete bounded provider-neutral request.
        request: ImageRequest,
    },
    /// Idempotent cancellation was requested.
    CancelRequested {
        /// Exact target turn.
        turn_id: TurnId,
        /// Optional safe caller reason.
        reason: Option<String>,
    },
    /// Further work stopped at one immutable turn-budget dimension.
    BudgetExhausted {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exhausted dimension.
        dimension: BudgetDimension,
        /// Amount already consumed when admission stopped.
        consumed: u64,
        /// Frozen limit for this turn.
        limit: u64,
    },
    /// Immutable Language provider input was prepared before I/O.
    ModelIntent {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Redacted provider preparation snapshot.
        snapshot: PreparedCallSnapshot,
    },
    /// The prepared Language call was authorized to start after intent durability.
    ModelStarted {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
    },
    /// Immutable Image provider input was prepared before I/O.
    ImageIntent {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Redacted provider preparation snapshot.
        snapshot: PreparedCallSnapshot,
    },
    /// The prepared Image call was authorized to start after intent durability.
    ImageStarted {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
    },
    /// One generated image became durable Media and then a durable Agent Fact.
    ImageOutput {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Zero-based contiguous provider output index.
        index: u32,
        /// Immutable Media reference, never media bytes.
        media: MediaRef,
    },
    /// One normalized Language stream event.
    ModelEvent {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Validated provider-neutral event.
        event: LanguageEvent,
    },
    /// One Tool call was pinned before execution.
    ToolIntent {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Exact retained-result identity.
        identity: ToolResultIdentity,
        /// Exact registered Tool name.
        name: String,
        /// Canonical bounded arguments.
        arguments: serde_json::Value,
        /// One-shot live approval evidence when required by the resolved turn policy.
        approval: Option<ApprovalOutcome>,
        /// Tool-owner scheduling proof copied from the sealed definition.
        parallel_safe: bool,
    },
    /// The prepared Tool call was authorized to start after intent durability.
    ToolStarted {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Exact retained-result identity.
        identity: ToolResultIdentity,
    },
    /// One complete Tool outcome containing Media references, never media bytes.
    ToolResult {
        /// Exact target turn.
        turn_id: TurnId,
        /// Exact effect identity.
        effect_id: EffectId,
        /// Exact retained-result identity.
        identity: ToolResultIdentity,
        /// Bounded result.
        result: ToolResult,
    },
    /// Sole terminal Fact for one turn.
    TurnTerminal {
        /// Exact target turn.
        turn_id: TurnId,
        /// Canonical terminal outcome.
        outcome: TurnOutcome,
    },
}

impl SessionFactBody {
    /// Revalidates all bounded semantic fields.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::TurnAccepted {
                text,
                model,
                turn_id: _,
                sandbox,
                require_approval,
            } => validate_turn_acceptance(text, model.as_ref(), *sandbox, *require_approval),
            Self::MessageTurnAccepted {
                message_ids,
                model,
                sandbox,
                require_approval,
                ..
            } => validate_message_turn_acceptance(
                message_ids,
                model.as_ref(),
                *sandbox,
                *require_approval,
            ),
            Self::StepStarted { .. } => Ok(()),
            Self::InputMessageEntered {
                source, content, ..
            } => {
                source.validate()?;
                let text_limit = if matches!(
                    source,
                    InputMessageSource::Agent { .. } | InputMessageSource::Completion { .. }
                ) {
                    MAXIMUM_AGENT_MESSAGE_BYTES
                } else {
                    MAXIMUM_TURN_TEXT_BYTES
                };
                validate_message_content(content, text_limit)
            }
            Self::StepEnded { outcome, .. } => outcome.validate(),
            Self::WorkspaceTouched { paths, .. } => validate_workspace_touch(paths),
            Self::ImageRequested { model, request, .. } => {
                model
                    .validate()
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
                request
                    .validate()
                    .map_err(|error| SessionError::Invalid(error.to_string()))
            }
            Self::CancelRequested { reason, .. } => {
                if let Some(reason) = reason {
                    validate_safe_text(
                        "cancellation reason",
                        reason,
                        MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
                        false,
                    )?;
                }
                Ok(())
            }
            Self::BudgetExhausted {
                dimension,
                consumed,
                limit,
                ..
            } => validate_budget_exhaustion(*dimension, *consumed, *limit),
            Self::ModelIntent { snapshot, .. } => validate_snapshot_capability(
                snapshot,
                AiCapability::Language,
                "Agent model intent must target the Language capability",
            ),
            Self::ImageIntent { snapshot, .. } => validate_snapshot_capability(
                snapshot,
                AiCapability::Image,
                "Agent image intent must target the Image capability",
            ),
            Self::ImageOutput { index, media, .. } => {
                if *index >= u32::from(MAX_IMAGE_OUTPUTS) {
                    return Err(SessionError::Invalid(
                        "image output index exceeds the protocol bound".into(),
                    ));
                }
                media
                    .validate()
                    .map_err(|error| SessionError::Invalid(error.to_string()))
            }
            Self::ModelStarted { .. } | Self::ImageStarted { .. } | Self::ToolStarted { .. } => {
                Ok(())
            }
            Self::ModelEvent { event, .. } => event
                .validate()
                .map_err(|error| SessionError::Invalid(error.to_string())),
            Self::ToolIntent {
                identity,
                name,
                arguments,
                approval,
                ..
            } => validate_tool_intent(identity, name, arguments, approval.as_ref()),
            Self::ToolResult { result, .. } => result
                .validate()
                .map_err(|error| SessionError::Invalid(error.to_string())),
            Self::TurnTerminal { outcome, .. } => outcome.validate(),
        }
    }

    /// Returns the turn identity affected by this Fact.
    pub const fn turn_id(&self) -> &TurnId {
        match self {
            Self::TurnAccepted { turn_id, .. }
            | Self::MessageTurnAccepted { turn_id, .. }
            | Self::StepStarted { turn_id, .. }
            | Self::InputMessageEntered { turn_id, .. }
            | Self::StepEnded { turn_id, .. }
            | Self::WorkspaceTouched { turn_id, .. }
            | Self::ImageRequested { turn_id, .. }
            | Self::CancelRequested { turn_id, .. }
            | Self::BudgetExhausted { turn_id, .. }
            | Self::ModelIntent { turn_id, .. }
            | Self::ModelStarted { turn_id, .. }
            | Self::ImageIntent { turn_id, .. }
            | Self::ImageStarted { turn_id, .. }
            | Self::ImageOutput { turn_id, .. }
            | Self::ModelEvent { turn_id, .. }
            | Self::ToolIntent { turn_id, .. }
            | Self::ToolStarted { turn_id, .. }
            | Self::ToolResult { turn_id, .. }
            | Self::TurnTerminal { turn_id, .. } => turn_id,
        }
    }
}

fn validate_turn_acceptance(
    text: &str,
    model: Option<&ModelRef>,
    sandbox: SandboxMode,
    require_approval: bool,
) -> Result<()> {
    validate_turn_text(text)?;
    validate_resolved_turn_policy(model, sandbox, require_approval)
}

fn validate_message_turn_acceptance(
    message_ids: &[MessageId],
    model: Option<&ModelRef>,
    sandbox: SandboxMode,
    require_approval: bool,
) -> Result<()> {
    if message_ids.is_empty()
        || message_ids.len() > MAXIMUM_PENDING_AGENT_MESSAGES
        || message_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != message_ids.len()
    {
        return Err(SessionError::Invalid(format!(
            "message Turn must claim 1..={MAXIMUM_PENDING_AGENT_MESSAGES} unique messages"
        )));
    }
    validate_resolved_turn_policy(model, sandbox, require_approval)
}

fn validate_resolved_turn_policy(
    model: Option<&ModelRef>,
    sandbox: SandboxMode,
    require_approval: bool,
) -> Result<()> {
    if let Some(model) = model {
        model
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
    }
    if sandbox == SandboxMode::DangerFullAccess && !require_approval {
        return Err(SessionError::Invalid(
            "danger-full-access turn requires live approval".into(),
        ));
    }
    Ok(())
}

fn validate_workspace_touch(paths: &[String]) -> Result<()> {
    if paths.is_empty() || paths.len() > MAXIMUM_WORKSPACE_TOUCH_PATHS {
        return Err(SessionError::Invalid(format!(
            "workspace touch must contain 1..={MAXIMUM_WORKSPACE_TOUCH_PATHS} paths"
        )));
    }
    for path in paths {
        validate_canonical_path(path)?;
    }
    Ok(())
}

fn validate_budget_exhaustion(dimension: BudgetDimension, consumed: u64, limit: u64) -> Result<()> {
    if limit == 0 || limit > dimension.hard_maximum() || consumed < limit {
        return Err(SessionError::Invalid(
            "budget exhaustion requires a positive reached dimension limit".into(),
        ));
    }
    Ok(())
}

fn validate_snapshot_capability(
    snapshot: &PreparedCallSnapshot,
    expected: AiCapability,
    mismatch: &'static str,
) -> Result<()> {
    snapshot
        .validate()
        .map_err(|error| SessionError::Invalid(error.to_string()))?;
    if snapshot.capability != expected {
        return Err(SessionError::Invalid(mismatch.into()));
    }
    Ok(())
}

fn validate_tool_intent(
    identity: &ToolResultIdentity,
    name: &str,
    arguments: &serde_json::Value,
    approval: Option<&ApprovalOutcome>,
) -> Result<()> {
    ToolCall {
        id: identity.call_id().into(),
        name: name.into(),
        arguments: arguments.clone(),
    }
    .validate()
    .map_err(|error| SessionError::Invalid(error.to_string()))?;
    if let Some(approval) = approval {
        approval
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        if approval.decision != ApprovalDecision::AllowOnce {
            return Err(SessionError::Invalid(
                "durable Tool intent cannot contain denied approval".into(),
            ));
        }
    }
    Ok(())
}

/// One sequenced append-only session Fact.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFact {
    seq: u64,
    timestamp_ms: u64,
    #[serde(flatten)]
    body: SessionFactBody,
    #[serde(skip)]
    encoded_len: usize,
}

impl<'de> Deserialize<'de> for SessionFact {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireFact {
            seq: u64,
            timestamp_ms: u64,
            #[serde(flatten)]
            body: SessionFactBody,
        }

        let wire = WireFact::deserialize(deserializer)?;
        Self::new(wire.seq, wire.timestamp_ms, wire.body).map_err(serde::de::Error::custom)
    }
}

impl SessionFact {
    /// Creates and validates one exact sequenced Fact.
    pub fn new(seq: u64, timestamp_ms: u64, body: SessionFactBody) -> Result<Self> {
        if seq == 0 || timestamp_ms == 0 {
            return Err(SessionError::Invalid(
                "Fact sequence and timestamp must be nonzero".into(),
            ));
        }
        body.validate()?;
        let mut fact = Self {
            seq,
            timestamp_ms,
            body,
            encoded_len: 0,
        };
        let encoded_len = compact_json_len(&fact)?;
        if encoded_len > MAXIMUM_SESSION_FACT_BYTES {
            return Err(SessionError::TooLarge {
                kind: "session Fact",
                maximum: MAXIMUM_SESSION_FACT_BYTES,
                actual: encoded_len,
            });
        }
        fact.encoded_len = encoded_len;
        Ok(fact)
    }

    /// Verifies the immutable construction proof retained by this typed Fact.
    pub fn validate(&self) -> Result<()> {
        if self.encoded_len == 0 || self.encoded_len > MAXIMUM_SESSION_FACT_BYTES {
            return Err(SessionError::TooLarge {
                kind: "session Fact",
                maximum: MAXIMUM_SESSION_FACT_BYTES,
                actual: self.encoded_len,
            });
        }
        Ok(())
    }

    /// Returns the exact compact-JSON byte length proven at construction.
    pub const fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    /// Returns the nonzero sequence within its session.
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Returns the creation timestamp in Unix milliseconds.
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the semantic body.
    pub const fn body(&self) -> &SessionFactBody {
        &self.body
    }

    /// Consumes the Fact and returns its body.
    pub fn into_body(self) -> SessionFactBody {
        self.body
    }
}

/// Advances the canonical digest chain by one validated Fact.
pub fn advance_fact_prefix_digest(previous: [u8; 32], fact: &SessionFact) -> Result<[u8; 32]> {
    struct DigestWriter<'a>(&'a mut Sha256);

    impl std::io::Write for DigestWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fact.validate()?;
    let mut digest = Sha256::new();
    digest.update(FACT_PREFIX_DOMAIN);
    digest.update(previous);
    serde_json::to_writer(DigestWriter(&mut digest), fact)
        .map_err(|error| SessionError::Encoding(error.to_string()))?;
    Ok(digest.finalize().into())
}

/// Computes lowercase SHA-256 for one exact canonical Fact prefix.
pub fn fact_prefix_sha256<'a>(facts: impl IntoIterator<Item = &'a SessionFact>) -> Result<String> {
    let mut digest = EMPTY_FACT_PREFIX_DIGEST;
    for fact in facts {
        digest = advance_fact_prefix_digest(digest, fact)?;
    }
    Ok(hex::encode(digest))
}

/// Advances the canonical digest chain by one validated Agent-control record.
pub fn advance_control_prefix_digest(
    previous: [u8; 32],
    record: &AgentControlRecord,
) -> Result<[u8; 32]> {
    struct DigestWriter<'a>(&'a mut Sha256);

    impl std::io::Write for DigestWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut digest = Sha256::new();
    digest.update(CONTROL_PREFIX_DOMAIN);
    digest.update(previous);
    serde_json::to_writer(DigestWriter(&mut digest), record)
        .map_err(|error| SessionError::Encoding(error.to_string()))?;
    Ok(digest.finalize().into())
}

/// Computes lowercase SHA-256 for one exact Agent-control prefix.
pub fn control_prefix_sha256<'a>(
    records: impl IntoIterator<Item = &'a AgentControlRecord>,
) -> Result<String> {
    let mut digest = EMPTY_CONTROL_PREFIX_DIGEST;
    for record in records {
        digest = advance_control_prefix_digest(digest, record)?;
    }
    Ok(hex::encode(digest))
}

fn compact_json_len(value: &(impl Serialize + ?Sized)) -> Result<usize> {
    struct Counter(usize);

    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("encoded JSON length overflowed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|error| SessionError::Encoding(error.to_string()))?;
    Ok(counter.0)
}

/// Validates one contiguous Fact sequence after an explicit cursor.
pub fn validate_fact_sequence(after_seq: u64, facts: &[SessionFact]) -> Result<()> {
    if facts.len() > MAXIMUM_FACTS_PER_READ {
        return Err(SessionError::TooLarge {
            kind: "Fact page",
            maximum: MAXIMUM_FACTS_PER_READ,
            actual: facts.len(),
        });
    }
    let mut expected = after_seq
        .checked_add(1)
        .ok_or_else(|| SessionError::Invalid("Fact sequence exhausted".into()))?;
    for fact in facts {
        fact.validate()?;
        if fact.seq != expected {
            return Err(SessionError::Invalid(format!(
                "Fact sequence is not contiguous: expected {expected}, got {}",
                fact.seq
            )));
        }
        expected = expected
            .checked_add(1)
            .ok_or_else(|| SessionError::Invalid("Fact sequence exhausted".into()))?;
    }
    Ok(())
}

/// Validates one contiguous Agent-control sequence after an explicit cursor.
pub fn validate_control_sequence(after_seq: u64, records: &[AgentControlRecord]) -> Result<()> {
    if records.len() > MAXIMUM_FACTS_PER_READ {
        return Err(SessionError::TooLarge {
            kind: "Agent control page",
            maximum: MAXIMUM_FACTS_PER_READ,
            actual: records.len(),
        });
    }
    let mut expected = after_seq
        .checked_add(1)
        .ok_or_else(|| SessionError::Invalid("control sequence exhausted".into()))?;
    for record in records {
        if record.seq() != expected {
            return Err(SessionError::Invalid(format!(
                "control sequence is not contiguous: expected {expected}, got {}",
                record.seq()
            )));
        }
        expected = expected
            .checked_add(1)
            .ok_or_else(|| SessionError::Invalid("control sequence exhausted".into()))?;
    }
    Ok(())
}

/// Closed durable-session contract failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SessionError {
    /// Malformed or semantically invalid durable value.
    #[error("invalid session value: {0}")]
    Invalid(String),
    /// Exact schema/format mismatch; migration is intentionally absent.
    #[error("unsupported session format version {0}")]
    UnsupportedFormat(u32),
    /// Encoded durable value exceeded a contract bound.
    #[error("{kind} is too large: maximum {maximum}, actual {actual}")]
    TooLarge {
        /// Value family.
        kind: &'static str,
        /// Maximum accepted count or bytes.
        maximum: usize,
        /// Actual count or bytes.
        actual: usize,
    },
    /// Canonical encoding failed.
    #[error("session encoding failed: {0}")]
    Encoding(String),
}

/// Session contract result.
pub type Result<T> = std::result::Result<T, SessionError>;

/// Validates one durable identity grammar.
pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_AGENT_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(SessionError::Invalid(format!(
            "{kind} identity must be bounded nonempty ASCII"
        )));
    }
    Ok(())
}

fn validate_sha256(kind: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(SessionError::Invalid(format!(
            "{kind} must be lowercase SHA-256"
        )));
    }
    Ok(())
}

/// Validates one nonempty bounded diagnostic using the durable safety rules.
pub fn validate_safe_diagnostic(kind: &str, value: &str) -> Result<()> {
    validate_safe_text(kind, value, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, false)
}

/// Validates one user turn text using the durable Fact contract.
pub fn validate_turn_text(value: &str) -> Result<()> {
    validate_safe_text("turn text", value, MAXIMUM_TURN_TEXT_BYTES, false)
}

fn validate_safe_text(kind: &str, value: &str, maximum: usize, allow_empty: bool) -> Result<()> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum
        || value
            .chars()
            .any(|character| character == '\0' || character == '\u{7f}')
    {
        return Err(SessionError::Invalid(format!(
            "{kind} must contain {}..={maximum} safe UTF-8 bytes",
            usize::from(!allow_empty)
        )));
    }
    Ok(())
}

fn validate_canonical_path(value: &str) -> Result<()> {
    validate_safe_text(
        "canonical workspace path",
        value,
        MAXIMUM_WORKSPACE_PATH_BYTES,
        false,
    )?;
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(SessionError::Invalid(
            "workspace path must be absolute and lexically normalized".into(),
        ));
    }
    Ok(())
}
