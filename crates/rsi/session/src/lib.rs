//! Transport-independent standard-product Session interface and local adapter.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionPin, AgentSessionDraft, PreparedFreshSession,
};
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, MAXIMUM_FACTS_PER_READ, SessionFact, SessionHeader,
    SessionId, TurnId,
};
use rsi_agent_store_protocol::{
    MAXIMUM_SESSIONS_PER_READ, SessionStore, StoreError, StoreRecentSessionCursor,
};
use rsi_agent_turn_protocol::{
    CancelResult, SubmitImage, SubmitSession, SubmitTurn, SubmittedTurn, TurnError,
    TurnObservation, TurnService,
};
use rsi_ai_protocol::{ImageCall, ImageRequest, LanguageCall, ModelRef};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_sandbox::SandboxMode;
use rsi_workspace::WorkspaceRegistry;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::Mutex;

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
}

/// One idempotent text submission.
#[derive(Clone, Debug)]
pub struct SubmitText {
    /// Caller-preallocated durable identity.
    pub turn_id: TurnId,
    /// Exact user text.
    pub text: String,
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
    /// Accepts one text turn and waits for durable acceptance.
    async fn submit_text(&self, request: SubmitText) -> Result<TurnReceipt>;
    /// Accepts one Image turn and waits for durable acceptance.
    async fn submit_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt>;
    /// Idempotently requests durable cancellation.
    async fn cancel(&self, turn_id: &TurnId, reason: Option<String>) -> Result<CancelResult>;
    /// Reads one bounded backward history page.
    async fn history_before(
        &self,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> Result<SessionHistoryPage>;
    /// Subscribes strictly after one live sequence.
    async fn subscribe(&self, after_seq: u64) -> Result<TurnObservation>;
    /// Lists live pending approvals for this session.
    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>>;
    /// Attempts to settle one pending approval.
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
    async fn submit_selection(&self, state: &mut HandleState) -> Result<SubmitSession> {
        match state {
            HandleState::Fresh(composition) => {
                PreparedFreshSession::new(self.header.clone(), composition.clone())
                    .map(SubmitSession::Fresh)
                    .map_err(|error| SessionApplicationError::Backend(error.to_string()))
            }
            HandleState::Attached => self
                .turns
                .prepare_resume(self.header.session_id())
                .await
                .map(SubmitSession::Resume)
                .map_err(map_turn_error),
        }
    }

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
}

#[async_trait]
impl SessionHandle for LocalSessionHandle {
    async fn header(&self) -> Result<SessionHeader> {
        Ok(self.header.clone())
    }

    async fn submit_text(&self, request: SubmitText) -> Result<TurnReceipt> {
        self.language
            .describe(
                request
                    .model
                    .as_ref()
                    .unwrap_or_else(|| self.header.settings().default_model()),
            )
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        let session = self.submit_selection(&mut state).await?;
        self.prepare_workspace().await?;
        let result = self
            .turns
            .submit(SubmitTurn {
                session,
                turn_id: request.turn_id,
                text: request.text,
                model: request.model,
                sandbox: request.sandbox,
            })
            .await;
        if result.is_ok() || matches!(&result, Err(TurnError::SubmissionConflict { .. })) {
            *state = HandleState::Attached;
        }
        result.map(TurnReceipt::from).map_err(map_turn_error)
    }

    async fn submit_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt> {
        self.image
            .describe(&request.model)
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        let session = self.submit_selection(&mut state).await?;
        let result = self
            .turns
            .submit_image(SubmitImage {
                session,
                turn_id: request.turn_id,
                model: request.model,
                request: request.request,
            })
            .await;
        if result.is_ok() || matches!(&result, Err(TurnError::SubmissionConflict { .. })) {
            *state = HandleState::Attached;
        }
        result.map(TurnReceipt::from).map_err(map_turn_error)
    }

    async fn cancel(&self, turn_id: &TurnId, reason: Option<String>) -> Result<CancelResult> {
        self.turns
            .cancel(self.header.session_id(), turn_id, reason)
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

    async fn subscribe(&self, after_seq: u64) -> Result<TurnObservation> {
        self.turns
            .observe(self.header.session_id(), after_seq)
            .await
            .map_err(map_turn_error)
    }

    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>> {
        self.approvals.pending(self.header.session_id()).await
    }

    async fn answer_approval(&self, approval_id: &str, decision: ApprovalDecision) -> Result<bool> {
        self.approvals
            .answer(self.header.session_id(), approval_id, decision)
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
    let metadata = tokio::fs::metadata(&canonical)
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
