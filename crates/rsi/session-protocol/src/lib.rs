//! Shared standard-product Session contracts and bounded observations.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    AgentMessage, AgentPresetId, FrozenAgentSettings, MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS,
    MessageId, SessionFact, SessionHeader, SessionId, TurnId, WorkspaceTrust,
};
use rsi_agent_turn_protocol::{
    CancelResult, CancelTarget, MessageReceipt, ObservationCursor, SessionObservationStream,
    SubmittedTurn,
};
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_sandbox::SandboxMode;
use rsi_workspace_protocol::WorkspaceId;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

mod interactions;
pub use interactions::{
    InteractionCollection, InteractionRetention, InteractionSnapshot, InteractionStream,
};

/// Maximum aggregate canonical media bytes referenced by one Session message.
pub const MAXIMUM_SESSION_INPUT_MEDIA_BYTES: usize = 64 * 1024 * 1024;

/// Current validated Agent defaults for newly created drafts.
pub trait AgentSettingsSource: fmt::Debug + Send + Sync + 'static {
    /// Reads one current snapshot; stale or unavailable registration is an error.
    fn current(&self) -> Result<FrozenAgentSettings>;
}

/// Nominal Local contract for fresh Agent defaults.
#[derive(Debug)]
pub struct AgentSettingsContract;
impl rsi_meta_contract::LocalContract for AgentSettingsContract {
    const KEY: &'static str = "rsi.agent.defaults";
    type Service = dyn AgentSettingsSource;
}

/// Live approval control injected into one Session adapter.
#[async_trait]
pub trait SessionApprovalControl: fmt::Debug + Send + Sync + 'static {
    /// Lists bounded pending requests for one exact session.
    async fn pending(&self, session_id: &SessionId) -> Result<Vec<ApprovalRequest>>;
    /// Collects selected Sessions in one bounded registry pass.
    async fn pending_for_sessions(&self, sessions: &[SessionId]) -> Result<Vec<ApprovalRequest>>;
    /// Subscribes before collecting the selected Sessions.
    fn watch_pending(
        &self,
        sessions: &[SessionId],
    ) -> Result<rsi_user_questions_protocol::PendingChanges>;
    /// Settles or confirms an identical retained answer; `false` means unavailable.
    async fn answer(
        &self,
        session_id: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<bool>;
}

/// Host-generation approval control supplied to the Session domain plugin.
#[derive(Debug)]
pub struct SessionApprovalControlContract;
impl rsi_meta_contract::LocalContract for SessionApprovalControlContract {
    const KEY: &'static str = "rsi.session.approvals";
    type Service = dyn SessionApprovalControl;
}

/// Approval control used by non-capable local applications.
#[derive(Debug, Default)]
pub struct NoApprovalControl;

#[async_trait]
impl SessionApprovalControl for NoApprovalControl {
    async fn pending_for_sessions(&self, _sessions: &[SessionId]) -> Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }
    fn watch_pending(
        &self,
        _sessions: &[SessionId],
    ) -> Result<rsi_user_questions_protocol::PendingChanges> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
    async fn pending(&self, _session_id: &SessionId) -> Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }

    async fn answer(
        &self,
        _session_id: &SessionId,
        _approval_id: &str,
        _decision: ApprovalDecision,
    ) -> Result<bool> {
        Ok(false)
    }
}

/// Request to create one process-local draft without durable mutation.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    /// Exact host registration resolved only for a newly admitted draft.
    pub workspace_id: WorkspaceId,
    /// Caller-preallocated identity retained through every create retry.
    pub session_id: SessionId,
    /// Explicit preset or the current catalog default.
    pub agent_preset_id: Option<AgentPresetId>,
    /// Explicit immutable authority for project-controlled instructions and skills.
    pub workspace_trust: WorkspaceTrust,
}

/// One atomic view of a still-unpublished draft.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDraftView {
    /// Current Header, including the selected preset.
    pub header: SessionHeader,
    /// Current lease-local mutation revision.
    pub revision: u64,
}

/// Compare-and-select request for a new unpublished preset generation.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectDraftPreset {
    /// Exact preset selected by the caller.
    pub preset_id: AgentPresetId,
    /// Exact draft revision observed by the caller.
    pub expected_revision: u64,
}

/// One transport-independent user-input block.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionInput {
    /// Safe UTF-8 text entering model context directly.
    Text {
        /// Exact text bytes.
        text: String,
    },
    /// Already uploaded canonical image, verified before durable admission.
    Image {
        /// Exact immutable reference returned by Media import.
        media: rsi_media_protocol::MediaRef,
    },
}

/// Validates one complete Session input before provider, Media, Store, or transport work.
pub fn validate_session_input(content: &[SessionInput]) -> Result<()> {
    if content.is_empty() || content.len() > MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS {
        return Err(SessionError::Invalid(format!(
            "Session input must contain 1..={MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS} blocks"
        )));
    }
    let mut text_bytes = 0_usize;
    let mut image_bytes = 0_usize;
    for block in content {
        match block {
            SessionInput::Text { text } => {
                if text.is_empty()
                    || text.len() > rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES
                    || text
                        .chars()
                        .any(|character| character == '\0' || character == '\u{7f}')
                {
                    return Err(SessionError::Invalid(format!(
                        "Session message text must contain 1..={} safe UTF-8 bytes",
                        rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES
                    )));
                }
                text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                    SessionError::Invalid("Session input text byte total overflowed".into())
                })?;
            }
            SessionInput::Image { media } => {
                media
                    .validate()
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
                let bytes = usize::try_from(media.bytes).map_err(|_| {
                    SessionError::Invalid("Session media byte length is unsupported".into())
                })?;
                image_bytes = image_bytes.checked_add(bytes).ok_or_else(|| {
                    SessionError::Invalid("Session media bytes overflowed".into())
                })?;
            }
        }
    }
    if text_bytes > rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES {
        return Err(SessionError::Invalid(format!(
            "Session message text exceeds {} aggregate bytes",
            rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES
        )));
    }
    if image_bytes > MAXIMUM_SESSION_INPUT_MEDIA_BYTES {
        return Err(SessionError::Invalid(format!(
            "Session input images exceed {MAXIMUM_SESSION_INPUT_MEDIA_BYTES} aggregate bytes"
        )));
    }
    Ok(())
}

/// One idempotent multimodal mailbox submission.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitInput {
    /// Immutable human ingress intent: `NextTurn` or `Steer`.
    pub delivery: rsi_agent_session_protocol::MessageDelivery,
    /// Caller-preallocated durable message identity.
    pub message_id: MessageId,
    /// Nonempty ordered text and image content.
    pub content: Vec<SessionInput>,
    /// Optional invocation-scoped model route.
    pub model: Option<ModelRef>,
    /// Optional invocation-scoped sandbox mode.
    pub sandbox: Option<SandboxMode>,
}

/// One idempotent direct Image submission.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitDirectImage {
    /// Caller-preallocated durable identity.
    pub turn_id: TurnId,
    /// Exact invocation-scoped Image route.
    pub model: ModelRef,
    /// Complete provider-neutral request.
    pub request: ImageRequest,
}

/// Durable acceptance receipt.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnReceipt {
    /// Exact session identity.
    pub session_id: SessionId,
    /// Exact caller-preallocated turn identity.
    pub turn_id: TurnId,
    /// Durable acceptance sequence.
    pub accepted_seq: u64,
}

impl From<SubmittedTurn> for TurnReceipt {
    fn from(value: SubmittedTurn) -> Self {
        Self {
            session_id: value.session_id,
            turn_id: value.turn_id,
            accepted_seq: value.accepted_seq,
        }
    }
}

/// One bounded ascending page immediately before an exclusive cursor.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHistoryPage {
    /// Effective exclusive cursor used by the Store.
    pub before_seq: u64,
    /// Contiguous Facts in ascending sequence order.
    pub facts: Vec<SessionFact>,
    /// Exact durable tail at read time.
    pub durable_seq: u64,
    /// Whether an earlier page exists.
    pub has_more: bool,
}

/// Public cursor for recent-session listing.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentSessionCursor {
    /// Durable creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Descending identity tie-breaker.
    pub session_id: SessionId,
}

/// One recent durable session summary.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    /// Complete immutable durable Header.
    pub header: SessionHeader,
}

impl SessionSummary {
    /// Returns the cursor selecting summaries after this one.
    pub fn cursor(&self) -> RecentSessionCursor {
        RecentSessionCursor {
            created_at_ms: self.header.created_at_ms(),
            session_id: self.header.session_id().clone(),
        }
    }
}

/// One bounded recent-session page.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentSessionPage {
    /// Exact summaries in descending creation order.
    pub sessions: Vec<SessionSummary>,
    /// Whether a later page exists.
    pub has_more: bool,
}

/// One attached Session interface.
#[async_trait]
pub trait SessionHandle: fmt::Debug + Send + Sync + 'static {
    /// Reads the current Header and revision from one live draft snapshot.
    async fn draft_snapshot(&self) -> Result<SessionDraftView>;
    /// Selects a fully prepared preset only at the expected unpublished revision.
    async fn select_preset(&self, request: SelectDraftPreset) -> Result<SessionDraftView>;
    /// Lists the pinned command catalog and exact draft or durable predecessor.
    async fn commands(&self) -> Result<rsi_agent_session_protocol::SessionCommandsView>;
    /// Executes one frozen logical invocation; no callback is implicitly replayed.
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> Result<rsi_agent_session_protocol::SessionCommandReceipt>;
    /// Queries the original compact receipt in the current draft lease or durable Session.
    async fn command_status(
        &self,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>>;
    /// Reads only the immutable acceptance record matching this exact identity and cursor.
    async fn read_message(
        &self,
        message_id: &MessageId,
        accepted_control_seq: u64,
    ) -> Result<AgentMessage>;
    /// Reads the immutable candidate or durable Header.
    async fn header(&self) -> Result<SessionHeader>;
    /// Accepts one multimodal message and waits for durable mailbox acceptance.
    async fn submit(&self, request: SubmitInput) -> Result<MessageReceipt>;
    /// Reads the latest durable claim or discard state for one message.
    async fn message_status(&self, message_id: &MessageId) -> Result<MessageReceipt>;
    /// Accepts one direct Image generation turn and waits for durable acceptance.
    async fn generate_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt>;
    /// Idempotently cancels an unclaimed message or an accepted Turn.
    async fn cancel(&self, target: CancelTarget, reason: Option<String>) -> Result<CancelResult>;
    /// Reads one bounded backward history page.
    async fn history_before(
        &self,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> Result<SessionHistoryPage>;
    /// Reconnectably observes durable control records and Facts after exact cursors.
    async fn observe(&self, cursor: ObservationCursor) -> Result<SessionObservationStream>;
    /// Observes a complete initial live interaction snapshot and coalesced changes.
    async fn observe_interactions(&self) -> Result<InteractionStream>;
    /// Captures one atomic durable inspection of this Session and subtree.
    async fn inspect(&self) -> Result<rsi_agent_store_protocol::StoreSessionInspection>;
    /// Lists this root Session's live pending human questions.
    async fn pending_questions(&self) -> Result<Vec<rsi_user_questions_protocol::QuestionRequest>>;
    /// Accepts or retries a live answer without promising durable Tool settlement.
    async fn answer_question(
        &self,
        id: &str,
        answer: rsi_user_questions_protocol::QuestionAnswer,
    ) -> Result<bool>;

    /// Lists live pending approvals for this complete Agent tree.
    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>>;
    /// Settles the exact approval tuple after validating its owner in this Agent tree.
    async fn answer_approval(
        &self,
        owner: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<bool>;
}

/// Transport-independent Session domain service.
#[async_trait]
pub trait SessionService: fmt::Debug + Send + Sync + 'static {
    /// Creates one unpublished draft handle after rejecting a durable identity collision.
    async fn create(&self, request: CreateSession) -> Result<Arc<dyn SessionHandle>>;
    /// Resolves a live draft or attaches to one exact durable session from Store alone.
    async fn attach(&self, session_id: &SessionId) -> Result<Arc<dyn SessionHandle>>;
    /// Lists one bounded creation-time-descending page.
    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> Result<RecentSessionPage>;
}

/// Trusted server ingress to the same draft owner used by ordinary local callers.
#[async_trait]
pub trait SessionIngress: fmt::Debug + Send + Sync + 'static {
    /// Creates or reconciles a live draft with an authenticated, non-serialized origin.
    async fn create_from(
        &self,
        request: CreateSession,
        origin: rsi_api_protocol::CallOrigin,
    ) -> Result<Arc<dyn SessionHandle>>;
}

/// Server-only creation authority; ordinary clients never publish this contract.
#[derive(Debug)]
pub struct SessionIngressContract;
impl rsi_meta_contract::LocalContract for SessionIngressContract {
    const KEY: &'static str = "rsi.session.ingress";
    type Service = dyn SessionIngress;
}

/// One shared Session domain service in a composition generation.
#[derive(Debug)]
pub struct SessionContract;
impl rsi_meta_contract::LocalContract for SessionContract {
    const KEY: &'static str = "rsi.session";
    type Service = dyn SessionService;
}

/// Closed Session application failure taxonomy shared by all adapters.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SessionError {
    /// An existing command identity names different logical input.
    #[error("command request {request_id} conflicts with its original invocation")]
    CommandConflict {
        /// Exact Session-scoped idempotency identity.
        request_id: rsi_agent_session_protocol::DomainRequestId,
    },
    /// A command no longer names the current draft or durable predecessor.
    #[error("command revision conflict: expected {expected:?}, actual {actual:?}")]
    CommandRevisionConflict {
        /// Frozen caller predecessor.
        expected: rsi_agent_session_protocol::CommandRevision,
        /// Current owning revision.
        actual: rsi_agent_session_protocol::CommandRevision,
    },
    /// Delivery or Store reconciliation cannot establish the command's result.
    #[error("command request {request_id} has an unknown outcome; query its original identity")]
    CommandOutcomeUnknown {
        /// Exact original request identity; never replaced automatically.
        request_id: rsi_agent_session_protocol::DomainRequestId,
    },
    /// Common API failure, including uncertain mutation outcomes.
    #[error(transparent)]
    Api(rsi_api_protocol::ApiError),
    /// Malformed, oversized, or state-incompatible request.
    #[error("invalid Session operation: {0}")]
    Invalid(String),
    /// Selected durable identity is absent.
    #[error("Session object was not found: {0}")]
    NotFound(String),
    /// A live draft identity was reused with different canonical creation input.
    #[error("Session draft `{session}` conflicts with existing creation input")]
    DraftConflict {
        /// Exact caller-owned Session identity.
        session: String,
    },
    /// A preallocated Turn identity names a different canonical submission.
    #[error("Session `{session}` turn `{turn}` conflicts with an existing submission")]
    Conflict {
        /// Exact session identity.
        session: String,
        /// Exact turn identity.
        turn: String,
    },
    /// A preallocated Message identity names different canonical input.
    #[error("Session `{session}` message `{message}` conflicts with accepted input")]
    MessageConflict {
        /// Exact Session identity.
        session: String,
        /// Exact Message identity.
        message: String,
    },
    /// Transport failed after a caller-owned idempotency identity was allocated;
    /// retry or query that exact message identity to reconcile the durable outcome.
    #[error(
        "Session `{session}` message `{message}` has an unknown durable outcome; retry with the same message identity"
    )]
    MessageOutcomeUnknown {
        /// Exact Session identity.
        session: String,
        /// Caller-owned Message identity safe to retry or query.
        message: String,
    },
    /// A bounded live resource is exhausted.
    #[error("Session capacity is exhausted")]
    Capacity,
    /// Host admission has stopped.
    #[error("Service Host is shutting down")]
    ShuttingDown,
    /// Local implementation or durable dependency failed.
    #[error("Session backend failed: {0}")]
    Backend(String),
}

/// Session application result.
pub type Result<T> = std::result::Result<T, SessionError>;

pub(crate) fn map_question_error(
    error: rsi_user_questions_protocol::QuestionError,
) -> SessionError {
    use rsi_user_questions_protocol::QuestionError;
    match error {
        QuestionError::Cancelled => SessionError::ShuttingDown,
        QuestionError::Capacity => SessionError::Capacity,
        error @ (QuestionError::Invalid(_) | QuestionError::Conflict) => {
            SessionError::Invalid(error.to_string())
        }
    }
}
