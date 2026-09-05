//! Transport-independent standard-product Session interface and local adapter.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionPin, AgentSessionDraft, PreparedFreshSession,
};
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, AgentPresetId, FrozenAgentSettings,
    MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS, MAXIMUM_FACTS_PER_READ, MessageId, MessageOptions,
    MessageTarget, SessionFact, SessionHeader, SessionId, TurnId, WorkspaceTrust,
};
use rsi_agent_store_protocol::{
    MAXIMUM_SESSIONS_PER_READ, SessionStore, StoreError, StoreRecentSessionCursor,
};
use rsi_agent_turn_protocol::{
    CancelResult, CancelTarget, MessageReceipt, ObservationCursor, SessionObservationStream,
    SubmitImage, SubmitMessage as SubmitAgentMessage, SubmitSession, SubmittedTurn, TurnError,
    TurnService,
};
use rsi_ai_protocol::{ImageCall, ImageRequest, LanguageCall, ModelRef};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_media_protocol::{Media, MediaError};
use rsi_sandbox::SandboxMode;
use rsi_workspace::WorkspaceRegistry;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::Mutex;

/// Maximum aggregate encoded image bytes accepted by one Session message.
pub const MAXIMUM_SESSION_INPUT_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Current immutable Agent settings for newly created sessions.
pub trait AgentSettingsSource: fmt::Debug + Send + Sync + 'static {
    /// Returns one bounded redacted settings snapshot.
    fn current(&self) -> FrozenAgentSettings;
}

/// Live approval control injected into one Session adapter.
#[async_trait]
pub trait SessionApprovalControl: fmt::Debug + Send + Sync + 'static {
    /// Lists bounded pending requests for one exact session.
    async fn pending(&self, session_id: &SessionId) -> Result<Vec<ApprovalRequest>>;
    /// Attempts to settle one request; `false` means it was already settled.
    async fn answer(
        &self,
        session_id: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<bool>;
}

/// Approval control used by non-capable local applications.
#[derive(Debug, Default)]
pub struct NoApprovalControl;

#[async_trait]
impl SessionApprovalControl for NoApprovalControl {
    async fn pending(&self, _session_id: &SessionId) -> Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }

    async fn answer(
        &self,
        _session_id: &SessionId,
        _approval_id: &str,
        _decision: ApprovalDecision,
    ) -> Result<bool> {
        Err(SessionApplicationError::Invalid(
            "this Session client is not approval-capable".into(),
        ))
    }
}

/// Request to create one process-local draft without durable mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSession {
    /// Canonicalized before the draft Header is built.
    pub cwd: PathBuf,
    /// Optional caller-preallocated session identity.
    pub session_id: Option<SessionId>,
    /// Explicit preset or the current catalog default.
    pub agent_preset_id: Option<AgentPresetId>,
    /// Explicit immutable authority for project-controlled instructions and skills.
    pub workspace_trust: WorkspaceTrust,
}

/// One transport-independent user-input block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionInput {
    /// Safe UTF-8 text entering model context directly.
    Text {
        /// Exact text bytes.
        text: String,
    },
    /// Encoded image bytes imported through Media before durable admission.
    Image {
        /// Complete encoded image body.
        bytes: Arc<[u8]>,
    },
}

/// Validates one complete Session input before provider, Media, Store, or transport work.
pub fn validate_session_input(content: &[SessionInput]) -> Result<()> {
    if content.is_empty() || content.len() > MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS {
        return Err(SessionApplicationError::Invalid(format!(
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
                    return Err(SessionApplicationError::Invalid(format!(
                        "Session message text must contain 1..={} safe UTF-8 bytes",
                        rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES
                    )));
                }
                text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                    SessionApplicationError::Invalid(
                        "Session input text byte total overflowed".into(),
                    )
                })?;
            }
            SessionInput::Image { bytes } => {
                if bytes.is_empty() {
                    return Err(SessionApplicationError::Invalid(
                        "Session input image must not be empty".into(),
                    ));
                }
                image_bytes = image_bytes.checked_add(bytes.len()).ok_or_else(|| {
                    SessionApplicationError::Invalid("Session input image bytes overflowed".into())
                })?;
            }
        }
    }
    if text_bytes > rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES {
        return Err(SessionApplicationError::Invalid(format!(
            "Session message text exceeds {} aggregate bytes",
            rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES
        )));
    }
    if image_bytes > MAXIMUM_SESSION_INPUT_IMAGE_BYTES {
        return Err(SessionApplicationError::Invalid(format!(
            "Session input images exceed {MAXIMUM_SESSION_INPUT_IMAGE_BYTES} aggregate bytes"
        )));
    }
    Ok(())
}

/// One idempotent multimodal mailbox submission.
#[derive(Clone, Debug)]
pub struct SubmitInput {
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
#[derive(Clone, Debug)]
pub struct SubmitDirectImage {
    /// Caller-preallocated durable identity.
    pub turn_id: TurnId,
    /// Exact invocation-scoped Image route.
    pub model: ModelRef,
    /// Complete provider-neutral request.
    pub request: ImageRequest,
}

/// Durable acceptance receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentSessionCursor {
    /// Durable creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Descending identity tie-breaker.
    pub session_id: SessionId,
}

/// One recent durable session summary.
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentSessionPage {
    /// Exact summaries in descending creation order.
    pub sessions: Vec<SessionSummary>,
    /// Whether a later page exists.
    pub has_more: bool,
}

/// One attached Session interface.
#[async_trait]
pub trait SessionHandle: fmt::Debug + Send + Sync + 'static {
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
    /// Lists live pending approvals for this complete Agent tree.
    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>>;
    /// Attempts to settle one live approval in this Agent tree.
    ///
    /// `false` means no matching pending request remains, including an unknown
    /// identity. An identity pending in multiple Sessions is rejected as ambiguous.
    async fn answer_approval(&self, approval_id: &str, decision: ApprovalDecision) -> Result<bool>;
}

/// Product-level Session application interface.
#[async_trait]
pub trait SessionApplication: fmt::Debug + Send + Sync + 'static {
    /// Creates one unpublished draft handle after rejecting a durable identity collision.
    async fn create(&self, request: CreateSession) -> Result<Arc<dyn SessionHandle>>;
    /// Attaches to one exact durable session.
    async fn attach(&self, session_id: &SessionId) -> Result<Arc<dyn SessionHandle>>;
    /// Lists one bounded creation-time-descending page.
    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> Result<RecentSessionPage>;
}

/// Process-local adapter over the Agent Kernel and mechanical Store.
#[derive(Clone)]
pub struct LocalSessionApplication {
    turns: Arc<dyn TurnService>,
    store: Arc<dyn SessionStore>,
    composition: Arc<dyn AgentComposition>,
    workspace: Arc<dyn WorkspaceRegistry>,
    settings: Arc<dyn AgentSettingsSource>,
    language: Arc<dyn LanguageCall>,
    image: Arc<dyn ImageCall>,
    media: Arc<dyn Media>,
    approvals: Arc<dyn SessionApprovalControl>,
}

impl fmt::Debug for LocalSessionApplication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSessionApplication")
            .finish_non_exhaustive()
    }
}

impl LocalSessionApplication {
    /// Creates one local adapter from already-owned Host dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        turns: Arc<dyn TurnService>,
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
        workspace: Arc<dyn WorkspaceRegistry>,
        settings: Arc<dyn AgentSettingsSource>,
        language: Arc<dyn LanguageCall>,
        image: Arc<dyn ImageCall>,
        media: Arc<dyn Media>,
        approvals: Arc<dyn SessionApprovalControl>,
    ) -> Self {
        Self {
            turns,
            store,
            composition,
            workspace,
            settings,
            language,
            image,
            media,
            approvals,
        }
    }

    fn handle_from_header(
        &self,
        header: SessionHeader,
        state: HandleState,
    ) -> Arc<dyn SessionHandle> {
        Arc::new(LocalSessionHandle {
            header,
            state: Mutex::new(state),
            turns: Arc::clone(&self.turns),
            store: Arc::clone(&self.store),
            workspace: Arc::clone(&self.workspace),
            language: Arc::clone(&self.language),
            image: Arc::clone(&self.image),
            media: Arc::clone(&self.media),
            approvals: Arc::clone(&self.approvals),
        })
    }
}

#[async_trait]
impl SessionApplication for LocalSessionApplication {
    async fn create(&self, request: CreateSession) -> Result<Arc<dyn SessionHandle>> {
        let cwd = canonical_workspace_directory(&request.cwd).await?;
        let agent_preset_id = match request.agent_preset_id {
            Some(id) => id,
            None => self
                .composition
                .default_preset_id()
                .await
                .map_err(|error| SessionApplicationError::Backend(error.to_string()))?,
        };
        let settings = self.settings.current();
        settings
            .validate()
            .map_err(|error| SessionApplicationError::Invalid(error.to_string()))?;
        let session_id = request.session_id.map_or_else(generated_session_id, Ok)?;
        match self.store.header(&session_id).await {
            Ok(_) => {
                return Err(SessionApplicationError::Invalid(format!(
                    "Session identity `{session_id}` already exists in the durable Store"
                )));
            }
            Err(StoreError::NotFound(_)) => {}
            Err(error) => return Err(map_store_error(error)),
        }
        let canonical_cwd = cwd.to_str().ok_or_else(|| {
            SessionApplicationError::Invalid("canonical workspace path is not UTF-8".into())
        })?;
        let header = SessionHeader::new(
            session_id,
            now_ms()?,
            canonical_cwd,
            agent_preset_id,
            settings,
        )
        .and_then(|header| header.with_workspace_trust(request.workspace_trust))
        .map_err(|error| SessionApplicationError::Invalid(error.to_string()))?;
        let draft = AgentSessionDraft::new(header.clone(), Arc::clone(&self.composition))
            .await
            .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
        let composition = draft.composition().clone();
        Ok(self.handle_from_header(header, HandleState::Fresh(composition)))
    }

    async fn attach(&self, session_id: &SessionId) -> Result<Arc<dyn SessionHandle>> {
        let header = self
            .store
            .header(session_id)
            .await
            .map_err(map_store_error)?;
        Ok(self.handle_from_header(header, HandleState::Attached))
    }

    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> Result<RecentSessionPage> {
        if limit == 0 || limit > MAXIMUM_SESSIONS_PER_READ {
            return Err(SessionApplicationError::Invalid(format!(
                "recent-session limit must be within 1..={MAXIMUM_SESSIONS_PER_READ}"
            )));
        }
        let cursor = after.map(|cursor| StoreRecentSessionCursor {
            created_at_ms: cursor.created_at_ms,
            session_id: cursor.session_id.clone(),
        });
        let page = self
            .store
            .list_recent_sessions(cursor.as_ref(), limit)
            .await
            .map_err(map_store_error)?;
        let sessions = page
            .sessions
            .into_iter()
            .map(|row| SessionSummary { header: row.header })
            .collect();
        Ok(RecentSessionPage {
            sessions,
            has_more: page.has_more,
        })
    }
}

enum HandleState {
    Fresh(AgentCompositionPin),
    Attached,
}

struct LocalSessionHandle {
    header: SessionHeader,
    state: Mutex<HandleState>,
    turns: Arc<dyn TurnService>,
    store: Arc<dyn SessionStore>,
    workspace: Arc<dyn WorkspaceRegistry>,
    language: Arc<dyn LanguageCall>,
    image: Arc<dyn ImageCall>,
    media: Arc<dyn Media>,
    approvals: Arc<dyn SessionApprovalControl>,
}

impl fmt::Debug for LocalSessionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSessionHandle")
            .field("session_id", self.header.session_id())
            .finish_non_exhaustive()
    }
}

impl LocalSessionHandle {
    async fn prepare_workspace(&self) -> Result<()> {
        let cwd = canonical_workspace_directory(Path::new(self.header.canonical_cwd())).await?;
        if cwd.to_str() != Some(self.header.canonical_cwd()) {
            return Err(SessionApplicationError::Invalid(
                "durable Session workspace no longer resolves to its canonical path".into(),
            ));
        }
        self.workspace
            .get_or_create(&cwd)
            .await
            .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
        Ok(())
    }

    async fn prepare_message(&self, request: SubmitInput) -> Result<AgentMessage> {
        self.prepare_workspace().await?;
        let mut content = Vec::with_capacity(request.content.len());
        for block in request.content {
            content.push(match block {
                SessionInput::Text { text } => AgentMessageContent::Text { text },
                SessionInput::Image { bytes } => AgentMessageContent::Image {
                    media: self
                        .media
                        .import_image(bytes)
                        .await
                        .map_err(|error| map_media_import_error(&error))?,
                },
            });
        }
        let message = AgentMessage {
            message_id: request.message_id,
            source: AgentMessageSource::Human,
            content,
            options: MessageOptions {
                model: request.model,
                sandbox: request.sandbox,
            },
        };
        message
            .validate()
            .map_err(|error| SessionApplicationError::Invalid(error.to_string()))?;
        Ok(message)
    }
}

#[async_trait]
impl SessionHandle for LocalSessionHandle {
    async fn header(&self) -> Result<SessionHeader> {
        Ok(self.header.clone())
    }

    async fn submit(&self, request: SubmitInput) -> Result<MessageReceipt> {
        validate_session_input(&request.content)?;
        self.language
            .describe(
                request
                    .model
                    .as_ref()
                    .unwrap_or_else(|| self.header.settings().default_model()),
            )
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        if matches!(*state, HandleState::Attached) {
            drop(state);
            let session = self
                .turns
                .prepare_resume(self.header.session_id())
                .await
                .map(SubmitSession::Resume)
                .map_err(map_turn_error)?;
            let message = self.prepare_message(request).await?;
            return self
                .turns
                .submit_message(SubmitAgentMessage {
                    session,
                    message,
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                })
                .await
                .map_err(map_turn_error);
        }
        let HandleState::Fresh(composition) = &*state else {
            unreachable!("attached state returned before fresh submission")
        };
        let session = PreparedFreshSession::new(self.header.clone(), composition.clone())
            .map(SubmitSession::Fresh)
            .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
        let message = self.prepare_message(request).await?;
        let result = self
            .turns
            .submit_message(SubmitAgentMessage {
                session,
                message,
                target: MessageTarget::NextTurn,
                wake_required: true,
            })
            .await;
        let durable_header_matches = if result.is_err() {
            self.store
                .header(self.header.session_id())
                .await
                .is_ok_and(|header| header == self.header)
        } else {
            false
        };
        if result.is_ok() || durable_header_matches {
            *state = HandleState::Attached;
        }
        result.map_err(map_turn_error)
    }

    async fn message_status(&self, message_id: &MessageId) -> Result<MessageReceipt> {
        self.turns
            .message_status(self.header.session_id(), message_id)
            .await
            .map_err(map_turn_error)
    }

    async fn generate_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt> {
        self.image
            .describe(&request.model)
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        if matches!(*state, HandleState::Attached) {
            drop(state);
            let session = self
                .turns
                .prepare_resume(self.header.session_id())
                .await
                .map(SubmitSession::Resume)
                .map_err(map_turn_error)?;
            return self
                .turns
                .submit_image(SubmitImage {
                    session,
                    turn_id: request.turn_id,
                    model: request.model,
                    request: request.request,
                })
                .await
                .map(TurnReceipt::from)
                .map_err(map_turn_error);
        }
        let HandleState::Fresh(composition) = &*state else {
            unreachable!("attached state returned before fresh image submission")
        };
        let session = PreparedFreshSession::new(self.header.clone(), composition.clone())
            .map(SubmitSession::Fresh)
            .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
        let result = self
            .turns
            .submit_image(SubmitImage {
                session,
                turn_id: request.turn_id,
                model: request.model,
                request: request.request,
            })
            .await;
        let durable_header_matches = if result.is_err() {
            self.store
                .header(self.header.session_id())
                .await
                .is_ok_and(|header| header == self.header)
        } else {
            false
        };
        if result.is_ok() || durable_header_matches {
            *state = HandleState::Attached;
        }
        result.map(TurnReceipt::from).map_err(map_turn_error)
    }

    async fn cancel(&self, target: CancelTarget, reason: Option<String>) -> Result<CancelResult> {
        self.turns
            .cancel_target(self.header.session_id(), target, reason)
            .await
            .map_err(map_turn_error)
    }

    async fn history_before(
        &self,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> Result<SessionHistoryPage> {
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(SessionApplicationError::Invalid(format!(
                "history limit must be within 1..={MAXIMUM_FACTS_PER_READ}"
            )));
        }
        if matches!(*self.state.lock().await, HandleState::Fresh(_)) {
            return Ok(SessionHistoryPage {
                before_seq: 1,
                facts: Vec::new(),
                durable_seq: 0,
                has_more: false,
            });
        }
        let page = self
            .store
            .read_facts_before(
                self.header.session_id(),
                exclusive_before_seq.unwrap_or(0),
                limit,
            )
            .await
            .map_err(map_store_error)?;
        Ok(SessionHistoryPage {
            before_seq: page.before_seq,
            facts: page.facts,
            durable_seq: page.durable_seq,
            has_more: page.has_more,
        })
    }

    async fn observe(&self, cursor: ObservationCursor) -> Result<SessionObservationStream> {
        self.turns
            .observe_session(self.header.session_id(), cursor)
            .await
            .map_err(map_turn_error)
    }

    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>> {
        let mut pending = Vec::new();
        for session_id in self
            .turns
            .tree_sessions(self.header.session_id())
            .await
            .map_err(map_turn_error)?
        {
            pending.extend(self.approvals.pending(&session_id).await?);
        }
        Ok(pending)
    }

    async fn answer_approval(&self, approval_id: &str, decision: ApprovalDecision) -> Result<bool> {
        let mut selected = None;
        for session_id in self
            .turns
            .tree_sessions(self.header.session_id())
            .await
            .map_err(map_turn_error)?
        {
            if self
                .approvals
                .pending(&session_id)
                .await?
                .iter()
                .any(|request| request.id == approval_id)
                && selected.replace(session_id).is_some()
            {
                return Err(SessionApplicationError::Invalid(
                    "approval identity is ambiguous within the Agent tree".into(),
                ));
            }
        }
        let Some(session_id) = selected else {
            return Ok(false);
        };
        self.approvals
            .answer(&session_id, approval_id, decision)
            .await
    }
}

/// Resolves and validates one caller-owned workspace directory.
///
/// Remote adapters call this before transport so relative paths retain the
/// caller's working-directory meaning. The owning local adapter repeats the
/// check before constructing or using durable state.
pub async fn canonical_workspace_directory(path: &Path) -> Result<PathBuf> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| SessionApplicationError::Invalid(format!("workspace: {error}")))?;
    let metadata = tokio::fs::symlink_metadata(&canonical)
        .await
        .map_err(|error| SessionApplicationError::Invalid(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(SessionApplicationError::Invalid(
            "workspace path is not a directory".into(),
        ));
    }
    Ok(canonical)
}

fn generated_session_id() -> Result<SessionId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| SessionApplicationError::Backend(format!("OS entropy failed: {error}")))?;
    SessionId::new(format!("session-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| SessionApplicationError::Invalid(error.to_string()))
}

fn now_ms() -> Result<u64> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SessionApplicationError::Backend(error.to_string()))?;
    Ok(u64::try_from(value.as_millis()).unwrap_or(u64::MAX).max(1))
}

fn map_turn_error(error: TurnError) -> SessionApplicationError {
    match error {
        TurnError::Invalid(message) => SessionApplicationError::Invalid(message),
        TurnError::SessionNotFound(session) => SessionApplicationError::NotFound(session),
        TurnError::TurnNotFound { session, turn } => {
            SessionApplicationError::NotFound(format!("{session}/{turn}"))
        }
        TurnError::SubmissionConflict { session, turn } => {
            SessionApplicationError::Conflict { session, turn }
        }
        TurnError::MessageConflict { session, message } => {
            SessionApplicationError::MessageConflict { session, message }
        }
        TurnError::Capacity | TurnError::ObserverCapacity => SessionApplicationError::Capacity,
        TurnError::ShuttingDown => SessionApplicationError::ShuttingDown,
        other => SessionApplicationError::Backend(other.to_string()),
    }
}

fn map_store_error(error: StoreError) -> SessionApplicationError {
    match error {
        StoreError::Invalid(message) => SessionApplicationError::Invalid(message),
        StoreError::NotFound(value) => SessionApplicationError::NotFound(value),
        StoreError::TurnNotFound { session, turn } => {
            SessionApplicationError::NotFound(format!("{session}/{turn}"))
        }
        other => SessionApplicationError::Backend(other.to_string()),
    }
}

fn map_ai_error(error: &rsi_ai_protocol::AiError) -> SessionApplicationError {
    SessionApplicationError::Invalid(error.to_string())
}

fn map_media_import_error(error: &MediaError) -> SessionApplicationError {
    let message = error.to_string();
    match error {
        MediaError::InvalidInput(_) | MediaError::Codec(_) => {
            SessionApplicationError::Invalid(message)
        }
        MediaError::AdmissionFull(_) => SessionApplicationError::Capacity,
        MediaError::NotFound(_) | MediaError::Corrupt(_) | MediaError::Io(_) => {
            SessionApplicationError::Backend(message)
        }
    }
}

/// Closed Session application failure taxonomy shared by all adapters.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SessionApplicationError {
    /// Malformed, oversized, or state-incompatible request.
    #[error("invalid Session operation: {0}")]
    Invalid(String),
    /// Selected durable identity is absent.
    #[error("Session object was not found: {0}")]
    NotFound(String),
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
    #[error("Session Host is shutting down")]
    ShuttingDown,
    /// Local implementation or durable dependency failed.
    #[error("Session backend failed: {0}")]
    Backend(String),
}

/// Session application result.
pub type Result<T> = std::result::Result<T, SessionApplicationError>;
