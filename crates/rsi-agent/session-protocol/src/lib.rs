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
pub const SESSION_FORMAT_VERSION: u32 = 1;
/// Maximum bytes in one session, turn, effect, profile, or error-code identity.
pub const MAXIMUM_AGENT_IDENTIFIER_BYTES: usize = 256;
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
/// Maximum Facts returned by one Store read.
pub const MAXIMUM_FACTS_PER_READ: usize = 512;

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

/// Immutable redacted settings captured when one session becomes durable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenAgentProfile {
    profile_id: String,
    system_prompt: String,
    default_model: ModelRef,
    sandbox: SandboxMode,
    require_approval: bool,
}

impl<'de> Deserialize<'de> for FrozenAgentProfile {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireProfile {
            profile_id: String,
            system_prompt: String,
            default_model: ModelRef,
            sandbox: SandboxMode,
            require_approval: bool,
        }

        let wire = WireProfile::deserialize(deserializer)?;
        Self::new(
            wire.profile_id,
            wire.system_prompt,
            wire.default_model,
            wire.sandbox,
            wire.require_approval,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl FrozenAgentProfile {
    /// Creates a bounded immutable profile without resolved secrets.
    pub fn new(
        profile_id: impl Into<String>,
        system_prompt: impl Into<String>,
        default_model: ModelRef,
        sandbox: SandboxMode,
        require_approval: bool,
    ) -> Result<Self> {
        let profile = Self {
            profile_id: profile_id.into(),
            system_prompt: system_prompt.into(),
            default_model,
            sandbox,
            require_approval,
        };
        profile.validate()?;
        Ok(profile)
    }

    /// Revalidates a decoded profile.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("profile", &self.profile_id)?;
        validate_safe_text(
            "system prompt",
            &self.system_prompt,
            MAXIMUM_SYSTEM_PROMPT_BYTES,
            true,
        )?;
        self.default_model
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        if self.sandbox == SandboxMode::DangerFullAccess && !self.require_approval {
            return Err(SessionError::Invalid(
                "danger-full-access requires live approval".into(),
            ));
        }
        Ok(())
    }

    /// Returns the exact profile identity.
    pub fn profile_id(&self) -> &str {
        &self.profile_id
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

    /// Returns the lowercase SHA-256 of the canonical redacted profile.
    pub fn fingerprint(&self) -> Result<String> {
        let bytes =
            serde_json::to_vec(self).map_err(|error| SessionError::Encoding(error.to_string()))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

/// Immutable durable session header written with the first accepted turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHeader {
    format_version: u32,
    session_id: SessionId,
    created_at_ms: u64,
    canonical_cwd: String,
    profile: FrozenAgentProfile,
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
            session_id: SessionId,
            created_at_ms: u64,
            canonical_cwd: String,
            profile: FrozenAgentProfile,
        }

        let wire = WireHeader::deserialize(deserializer)?;
        let header = Self {
            format_version: wire.format_version,
            session_id: wire.session_id,
            created_at_ms: wire.created_at_ms,
            canonical_cwd: wire.canonical_cwd,
            profile: wire.profile,
        };
        header
            .validate()
            .map(|()| header)
            .map_err(serde::de::Error::custom)
    }
}

impl SessionHeader {
    /// Creates the exact v1 durable header.
    pub fn new(
        session_id: SessionId,
        created_at_ms: u64,
        canonical_cwd: impl Into<String>,
        profile: FrozenAgentProfile,
    ) -> Result<Self> {
        let header = Self {
            format_version: SESSION_FORMAT_VERSION,
            session_id,
            created_at_ms,
            canonical_cwd: canonical_cwd.into(),
            profile,
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
        self.profile.validate()?;
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

    /// Returns the frozen creation-time profile.
    pub const fn profile(&self) -> &FrozenAgentProfile {
        &self.profile
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
            } => {
                validate_safe_text("turn text", text, MAXIMUM_TURN_TEXT_BYTES, false)?;
                if let Some(model) = model {
                    model
                        .validate()
                        .map_err(|error| SessionError::Invalid(error.to_string()))?;
                }
                if *sandbox == SandboxMode::DangerFullAccess && !require_approval {
                    return Err(SessionError::Invalid(
                        "danger-full-access turn requires live approval".into(),
                    ));
                }
                Ok(())
            }
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
            | Self::ImageRequested { turn_id, .. }
            | Self::CancelRequested { turn_id, .. }
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
