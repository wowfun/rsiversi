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
        /// Monotonic numeric identity assigned by the durable agent store.
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

/// Immutable request to run or resume one durable agent session.
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

/// Durable outcome and transcript boundary returned by a run.
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

    /// Returns the last transcript event committed by this run.
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

/// Durable terminal state of an agent run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunStatus {
    /// Model completed the turn with a final user-visible message.
    Completed { final_message: String },
    /// Run terminated with a classified failure.
    Failed { failure: Failure },
    /// Process stopped before a terminal semantic outcome was committed.
    Interrupted,
}

/// Bounded durable failure recorded in a transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    /// Stable failure classification used by recovery and callers.
    pub kind: FailureKind,
    /// Bounded human-readable failure summary.
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

/// Stable durable classification of a failed agent run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Language provider could not be selected or reached.
    ModelUnavailable,
    /// Language provider violated the semantic protocol.
    ModelProtocol,
    /// Tool provider or requested tool was unavailable.
    ToolUnavailable,
    /// Tool provider violated the semantic protocol.
    ToolProtocol,
    /// Run exhausted its configured reasoning-step budget.
    StepLimitExceeded,
    /// Run exhausted its configured tool-call budget.
    CallLimitExceeded,
    /// Model context exceeded its configured or provider limit.
    ContextLimitExceeded,
    /// A bounded execution phase exceeded its deadline.
    TimedOut,
}

/// Immutable provider and tool context committed before model execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSnapshot {
    /// Exact system prompt supplied to the model.
    pub system_prompt: String,
    /// Exact selected model identifier.
    pub model: String,
    /// Generation-pinned language provider instance.
    pub model_provider: String,
    /// Language service protocol version.
    pub model_protocol_version: u32,
    /// Generation-pinned tools provider instance.
    pub tools_provider: String,
    /// Tools service protocol version.
    pub tools_protocol_version: u32,
    /// Canonical tool catalog exposed to the model.
    pub tools: Vec<ToolDefinition>,
}

/// Normalized complete assistant response committed to the transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    /// User-visible assistant text, when emitted.
    pub content: Option<String>,
    /// Provider reasoning text, when exposed.
    pub reasoning: Option<String>,
    /// Tool calls requested by the model in provider order.
    pub tool_calls: Vec<ToolCall>,
    /// Normalized reason generation stopped.
    pub finish_reason: rsi_ai_protocol::FinishReason,
    /// Provider token usage totals, when reported.
    pub usage: Option<rsi_ai_protocol::TokenUsage>,
    /// Opaque validated provider state required for a later replay turn.
    pub replay: Option<rsi_ai_protocol::ProviderExtension>,
    /// Non-fatal normalized provider warnings.
    pub warnings: Vec<rsi_ai_protocol::Warning>,
    /// Source citations returned by hosted provider tools.
    pub sources: Vec<rsi_ai_protocol::Source>,
}

/// One validated tool invocation requested by the model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Model-supplied correlation identity.
    pub id: CallId,
    /// Exact callable name from the committed catalog.
    pub name: String,
    /// Original JSON text emitted by the model.
    pub arguments: String,
}

/// Durable outcome of crossing the external tool-effect boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolOutcome {
    /// Tool completed with a validated JSON value.
    Succeeded {
        /// Bounded semantic tool result.
        value: serde_json::Value,
    },
    /// Tool completed with a known semantic failure.
    Failed {
        /// Machine-readable tool failure code.
        code: String,
        /// Bounded human-readable failure summary.
        message: String,
    },
    /// Tool effect was proven not to have started.
    NotStarted {
        /// Bounded explanation of why dispatch did not begin.
        reason: String,
    },
    /// Dispatch may have started, so automatic replay is unsafe.
    OutcomeUnknown,
}

/// Canonical language request committed before provider preparation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequestSnapshot {
    /// Durable identifier correlating preparation and retries.
    pub request_id: String,
    /// Exact selected model identifier.
    pub model: String,
    /// Transcript boundary from which the request was derived.
    pub source_through: EventSeq,
    /// Canonical serialized semantic request.
    #[serde(with = "arc_str")]
    pub canonical_json: Arc<str>,
    /// Lowercase SHA-256 digest of `canonical_json` bytes.
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

/// Durable result of one model or tool boundary within a step or turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoundaryOutcome {
    /// Execution may continue to the next boundary.
    Continued,
    /// Agent reached a successful terminal answer.
    Completed,
    /// Boundary terminated with a classified failure.
    Failed { failure: Failure },
    /// Execution stopped before a safe terminal outcome was known.
    Interrupted,
}

/// Ordered durable transcript and current terminal session status.
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

    /// Returns events in strictly increasing durable sequence order.
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

/// One sequenced semantic event in a durable transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptEvent {
    seq: EventSeq,
    kind: TranscriptEventKind,
}

impl TranscriptEvent {
    /// Returns the event's monotonic durable sequence number.
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

/// Closed semantic event grammar persisted for session recovery and inspection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptEventKind {
    /// Binds a new session identity to immutable model and prompt inputs.
    SessionStarted {
        /// Exact selected model identifier.
        model: String,
        /// Lowercase SHA-256 digest of the original prompt bytes.
        prompt_sha256: String,
    },
    /// Marks the beginning of a caller-visible agent turn.
    TurnStarted,
    /// Marks the beginning of one bounded reasoning step.
    StepStarted {
        /// Monotonic step identity within the session.
        step: StepId,
    },
    /// Records the exact caller message for this turn.
    UserMessage {
        /// Validated user message text.
        content: String,
    },
    /// Freezes provider identities and the tool catalog for the step.
    ContextSnapshot { context: ContextSnapshot },
    /// Commits a canonical semantic model request before provider preparation.
    ModelRequestPrepared { request: ModelRequestSnapshot },
    /// Commits provider resolution and preparation before starting model I/O.
    ModelCallPrepared {
        /// Durable request identifier.
        request_id: String,
        /// Redacted generation-pinned provider snapshot.
        snapshot: PreparedCallSnapshot,
    },
    /// Records a safe automatic retry after a known failed attempt.
    ModelRetryScheduled {
        /// Durable request identifier shared by all attempts.
        request_id: String,
        /// One-based attempt number that failed.
        failed_attempt: u8,
        /// Typed provider failure that justified retry.
        error: rsi_ai_protocol::AiError,
        /// Scheduled delay before the next attempt, in milliseconds.
        delay_ms: u64,
    },
    /// Records one complete normalized assistant response.
    AssistantMessage { message: AssistantMessage },
    /// Commits a validated tool call before crossing its effect boundary.
    ToolCallPrepared { call: ToolCall },
    /// Marks the point after which tool outcome may be unknown on interruption.
    ToolDispatchStarted { call_id: CallId },
    /// Records the durable semantic outcome of a tool call.
    ToolResult {
        /// Correlated tool call identity.
        call_id: CallId,
        /// Known or unknown dispatch outcome.
        outcome: ToolOutcome,
    },
    /// Closes one reasoning step with its boundary outcome.
    StepEnded {
        step: StepId,
        /// Whether execution continues or terminates.
        outcome: BoundaryOutcome,
    },
    /// Closes the caller-visible turn.
    TurnEnded {
        /// Terminal or interrupted turn outcome.
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
