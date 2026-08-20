use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rsi_agent_protocol::{MAX_ID_BYTES, ToolDefinition, is_wire_identifier};
use rsi_ai_meta::PreparedCallSnapshot;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{AgentError, MAX_PROMPT_BYTES, Result};

pub(crate) const MAX_FAILURE_MESSAGE_BYTES: usize = 4 * 1024;

/// Product-owned directory containing the private agent store and lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentWorkspace(PathBuf);

impl AgentWorkspace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self(root.into())
    }

    pub fn root(&self) -> &Path {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn database_path(&self) -> PathBuf {
        self.0.join("agent.sqlite3")
    }
}

macro_rules! numeric_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub struct $name(u64);

        impl $name {
            pub const fn get(self) -> u64 {
                self.0
            }

            pub(crate) const fn new(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

numeric_id!(EventSeq);
numeric_id!(StepId);

/// Caller-owned durable identity for a direct image, transcription, speech,
/// or Realtime operation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AiOperationId(String);

impl AiOperationId {
    /// Constructs a bounded wire-safe durable operation identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity violates the wire-id grammar.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_wire_id("ai_operation_id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_stored(value: String) -> Result<Self> {
        Self::new(value).map_err(|error| AgentError::CorruptStore {
            message: format!("stored AI operation id is invalid: {error}"),
        })
    }
}

impl fmt::Display for AiOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AiOperationId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Caller-owned durable retry identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Creates an identity containing 1–255 printable ASCII bytes without spaces.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidInput`] when the identity is empty,
    /// oversized, contains whitespace, or contains non-ASCII bytes.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_wire_id("session_id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_stored(value: String) -> Result<Self> {
        Self::new(value).map_err(|error| AgentError::CorruptStore {
            message: format!("stored session id is invalid: {error}"),
        })
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Model-supplied tool-call correlation identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CallId(String);

impl CallId {
    /// Creates a bounded model/tool correlation identity.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidInput`] when the identity is outside the
    /// printable, non-whitespace wire syntax.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_wire_id("call_id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_validated(value: String) -> Self {
        debug_assert!(validate_wire_id("call_id", &value).is_ok());
        Self(value)
    }
}

impl fmt::Display for CallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CallId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest {
    session_id: SessionId,
    model: Arc<str>,
    prompt: Arc<str>,
}

impl RunRequest {
    /// Binds an exact prompt to a caller-owned durable session identity.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidInput`] for an empty or oversized prompt.
    pub fn new(session_id: SessionId, prompt: impl Into<String>) -> Result<Self> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(AgentError::InvalidInput {
                field: "prompt",
                message: "must contain non-whitespace text".to_owned(),
            });
        }
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(AgentError::InvalidInput {
                field: "prompt",
                message: format!("exceeds {MAX_PROMPT_BYTES} UTF-8 bytes"),
            });
        }
        if prompt
            .chars()
            .any(|character| character == '\0' || character == '\u{007f}')
        {
            return Err(AgentError::InvalidInput {
                field: "prompt",
                message: "must not contain NUL or DEL".to_owned(),
            });
        }
        Ok(Self {
            session_id,
            model: Arc::from("default"),
            prompt: prompt.into(),
        })
    }

    /// Selects the exact model identifier sent to the generation-pinned language provider.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier violates the wire-id grammar.
    pub fn with_model(mut self, model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        validate_wire_id("model", &model)?;
        self.model = model.into();
        Ok(self)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn into_parts(self) -> (SessionId, Arc<str>, Arc<str>) {
        (self.session_id, self.model, self.prompt)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRecord(Arc<RunRecordInner>);

#[derive(Debug, Eq, PartialEq)]
struct RunRecordInner {
    session_id: SessionId,
    transcript_through: EventSeq,
    status: RunStatus,
}

impl RunRecord {
    pub fn session_id(&self) -> &SessionId {
        &self.0.session_id
    }

    pub fn transcript_through(&self) -> EventSeq {
        self.0.transcript_through
    }

    pub fn status(&self) -> &RunStatus {
        &self.0.status
    }

    pub(crate) fn new(
        session_id: SessionId,
        transcript_through: EventSeq,
        status: RunStatus,
    ) -> Self {
        Self(Arc::new(RunRecordInner {
            session_id,
            transcript_through,
            status,
        }))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunStatus {
    Completed { final_message: String },
    Failed { failure: Failure },
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub(crate) fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: bounded_message(message.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    ModelUnavailable,
    ModelProtocol,
    ToolUnavailable,
    ToolProtocol,
    StepLimitExceeded,
    CallLimitExceeded,
    ContextLimitExceeded,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSnapshot {
    pub system_prompt: String,
    pub model: String,
    pub model_provider: String,
    pub model_protocol_version: u32,
    pub tools_provider: String,
    pub tools_protocol_version: u32,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: rsi_ai_protocol::FinishReason,
    pub usage: Option<rsi_ai_protocol::TokenUsage>,
    pub replay: Option<rsi_ai_protocol::ProviderExtension>,
    pub warnings: Vec<rsi_ai_protocol::Warning>,
    pub sources: Vec<rsi_ai_protocol::Source>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    /// Original JSON text emitted by the model.
    pub arguments: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolOutcome {
    Succeeded { value: serde_json::Value },
    Failed { code: String, message: String },
    NotStarted { reason: String },
    OutcomeUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequestSnapshot {
    pub request_id: String,
    pub model: String,
    pub source_through: EventSeq,
    #[serde(with = "arc_str")]
    pub canonical_json: Arc<str>,
    pub sha256: String,
}

mod arc_str {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(
        value: &Arc<str>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Arc<str>, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Arc::from)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoundaryOutcome {
    Continued,
    Completed,
    Failed { failure: Failure },
    Interrupted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    session_id: SessionId,
    events: Vec<TranscriptEvent>,
    status: RunStatus,
}

impl Transcript {
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn events(&self) -> &[TranscriptEvent] {
        &self.events
    }

    pub const fn status(&self) -> &RunStatus {
        &self.status
    }

    pub(crate) fn new(
        session_id: SessionId,
        events: Vec<TranscriptEvent>,
        status: RunStatus,
    ) -> Self {
        Self {
            session_id,
            events,
            status,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptEvent {
    seq: EventSeq,
    kind: TranscriptEventKind,
}

impl TranscriptEvent {
    pub const fn seq(&self) -> EventSeq {
        self.seq
    }

    pub const fn kind(&self) -> &TranscriptEventKind {
        &self.kind
    }

    pub(crate) const fn new(seq: EventSeq, kind: TranscriptEventKind) -> Self {
        Self { seq, kind }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptEventKind {
    SessionStarted {
        model: String,
        prompt_sha256: String,
    },
    TurnStarted,
    StepStarted {
        step: StepId,
    },
    UserMessage {
        content: String,
    },
    ContextSnapshot {
        context: ContextSnapshot,
    },
    ModelRequestPrepared {
        request: ModelRequestSnapshot,
    },
    ModelCallPrepared {
        request_id: String,
        snapshot: PreparedCallSnapshot,
    },
    ModelRetryScheduled {
        request_id: String,
        failed_attempt: u8,
        error: rsi_ai_protocol::AiError,
        delay_ms: u64,
    },
    AssistantMessage {
        message: AssistantMessage,
    },
    ToolCallPrepared {
        call: ToolCall,
    },
    ToolDispatchStarted {
        call_id: CallId,
    },
    ToolResult {
        call_id: CallId,
        outcome: ToolOutcome,
    },
    StepEnded {
        step: StepId,
        outcome: BoundaryOutcome,
    },
    TurnEnded {
        outcome: BoundaryOutcome,
    },
}

pub(crate) fn validate_wire_id(field: &'static str, value: &str) -> Result<()> {
    if !is_wire_identifier(value) {
        return Err(AgentError::InvalidInput {
            field,
            message: format!(
                "must contain 1 to {MAX_ID_BYTES} printable ASCII bytes without spaces"
            ),
        });
    }
    Ok(())
}

fn bounded_message(mut message: String) -> String {
    message.retain(|character| character != '\0' && character != '\u{007f}');
    if message.is_empty() {
        message.push_str("unspecified failure");
    }
    if message.len() <= MAX_FAILURE_MESSAGE_BYTES {
        return message;
    }
    let mut end = MAX_FAILURE_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}
