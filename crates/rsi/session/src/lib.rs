//! Native standard-product Session domain service.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionPin, AgentSessionDraft, PreparedFreshSession,
};
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, MAXIMUM_FACTS_PER_READ, MessageId,
    MessageOptions, SessionHeader, SessionId,
};
use rsi_agent_store_protocol::{
    MAXIMUM_SESSIONS_PER_READ, SessionStore, StoreError, StoreRecentSessionCursor,
};
use rsi_agent_turn_protocol::{
    CancelResult, CancelTarget, MessageReceipt, ObservationCursor, SessionObservationStream,
    SubmitImage, SubmitMessage as SubmitAgentMessage, SubmitSession, TurnError, TurnService,
};
use rsi_ai_protocol::{ImageCall, LanguageCall};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_media_protocol::{Media, MediaError};
use rsi_workspace_protocol::WorkspaceRegistry;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

mod drafts;
mod interactions;
mod plugin;
pub use plugin::SessionFactory;

use rsi_session_protocol::{
    AgentSettingsSource, CreateSession, InteractionRetention, RecentSessionCursor,
    RecentSessionPage, Result, SessionApprovalControl, SessionError, SessionHandle,
    SessionHistoryPage, SessionInput, SessionService, SessionSummary, SubmitDirectImage,
    SubmitInput, TurnReceipt, validate_session_input,
};

/// Process-local adapter over the Agent Kernel and mechanical Store.
#[derive(Clone)]
pub struct LocalSessionService {
    turns: Arc<dyn TurnService>,
    store: Arc<dyn SessionStore>,
    composition: Arc<dyn AgentComposition>,
    workspace: Arc<dyn WorkspaceRegistry>,
    settings: Arc<dyn AgentSettingsSource>,
    language: Arc<dyn LanguageCall>,
    image: Arc<dyn ImageCall>,
    media: Arc<dyn Media>,
    approvals: Arc<dyn SessionApprovalControl>,
    questions: Option<Arc<dyn rsi_user_questions_protocol::UserQuestions>>,
    interaction_retention: InteractionRetention,
    drafts: Arc<drafts::Drafts>,
}

impl fmt::Debug for LocalSessionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSessionService")
            .finish_non_exhaustive()
    }
}

impl LocalSessionService {
    /// Creates one local adapter from already-owned Host dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        execution: rsi_meta::Execution,
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
            questions: None,
            interaction_retention: InteractionRetention::default(),
            drafts: drafts::Drafts::new(execution),
        }
    }

    /// Supplies the Host-generation human question capability.
    #[must_use]
    pub fn with_questions(
        mut self,
        questions: Option<Arc<dyn rsi_user_questions_protocol::UserQuestions>>,
    ) -> Self {
        self.questions = questions;
        self
    }

    fn handle_from_header(
        &self,
        header: SessionHeader,
        state: HandleState,
        lease: Option<drafts::DraftLease>,
    ) -> Arc<LocalSessionHandle> {
        Arc::new(LocalSessionHandle {
            header,
            published: std::sync::atomic::AtomicBool::new(matches!(state, HandleState::Attached)),
            lease,
            state: Mutex::new(state),
            turns: Arc::clone(&self.turns),
            store: Arc::clone(&self.store),
            workspace: Arc::clone(&self.workspace),
            language: Arc::clone(&self.language),
            image: Arc::clone(&self.image),
            media: Arc::clone(&self.media),
            approvals: Arc::clone(&self.approvals),
            questions: self.questions.clone(),
            interaction_retention: self.interaction_retention.clone(),
        })
    }
}

impl LocalSessionService {
    /// Stops draft admission and waits for service-owned preparation and sweeping.
    pub async fn stop(&self) {
        self.drafts.stop().await;
    }

    async fn prepare_draft(
        &self,
        request: CreateSession,
        lease: drafts::DraftLease,
    ) -> Result<Arc<LocalSessionHandle>> {
        let workspace = self
            .workspace
            .get(&request.workspace_id)
            .await
            .map_err(|error| map_workspace_error(&error))?;
        let cwd = workspace.path;
        let agent_preset_id = match request.agent_preset_id {
            Some(id) => id,
            None => self
                .composition
                .default_preset_id()
                .await
                .map_err(|error| SessionError::Backend(error.to_string()))?,
        };
        let settings = self.settings.current()?;
        settings
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let session_id = request.session_id;
        match self.store.header(&session_id).await {
            Ok(_) => {
                return Err(SessionError::Invalid(format!(
                    "Session identity `{session_id}` already exists in the durable Store"
                )));
            }
            Err(StoreError::NotFound(_)) => {}
            Err(error) => return Err(map_store_error(error)),
        }
        let canonical_cwd = cwd
            .to_str()
            .ok_or_else(|| SessionError::Invalid("canonical workspace path is not UTF-8".into()))?;
        let header = SessionHeader::new(
            session_id,
            now_ms()?,
            canonical_cwd,
            agent_preset_id,
            settings,
        )
        .and_then(|header| header.with_workspace_trust(request.workspace_trust))
        .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let draft = AgentSessionDraft::new(header.clone(), Arc::clone(&self.composition))
            .await
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        let composition = draft.composition().clone();
        Ok(self.handle_from_header(header, HandleState::Fresh(composition), Some(lease)))
    }
}

#[async_trait]
impl SessionService for LocalSessionService {
    async fn create(&self, request: CreateSession) -> Result<Arc<dyn SessionHandle>> {
        self.drafts.accepting()?;
        self.drafts
            .create(self.clone(), request, None)
            .await
            .map(|handle| handle as Arc<dyn SessionHandle>)
    }

    async fn attach(&self, session_id: &SessionId) -> Result<Arc<dyn SessionHandle>> {
        if let Some(handle) = self.drafts.get(session_id).await? {
            match handle.header().await {
                Ok(_) => return Ok(handle),
                Err(SessionError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let header = self
            .store
            .header(session_id)
            .await
            .map_err(map_store_error)?;
        Ok(self.handle_from_header(header, HandleState::Attached, None))
    }

    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> Result<RecentSessionPage> {
        if limit == 0 || limit > MAXIMUM_SESSIONS_PER_READ {
            return Err(SessionError::Invalid(format!(
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

#[async_trait]
impl rsi_session_protocol::SessionIngress for LocalSessionService {
    async fn create_from(
        &self,
        request: CreateSession,
        origin: rsi_api_protocol::CallOrigin,
    ) -> Result<Arc<dyn SessionHandle>> {
        let device = match origin {
            rsi_api_protocol::CallOrigin::Local => None,
            rsi_api_protocol::CallOrigin::Device(device) => Some(device.id),
        };
        self.drafts
            .create(self.clone(), request, device)
            .await
            .map(|handle| handle as Arc<dyn SessionHandle>)
    }
}

enum HandleState {
    Fresh(AgentCompositionPin),
    Attached,
    Expired,
}

struct LocalSessionHandle {
    header: SessionHeader,
    state: Mutex<HandleState>,
    lease: Option<drafts::DraftLease>,
    published: std::sync::atomic::AtomicBool,
    turns: Arc<dyn TurnService>,
    store: Arc<dyn SessionStore>,
    workspace: Arc<dyn WorkspaceRegistry>,
    language: Arc<dyn LanguageCall>,
    image: Arc<dyn ImageCall>,
    media: Arc<dyn Media>,
    approvals: Arc<dyn SessionApprovalControl>,
    questions: Option<Arc<dyn rsi_user_questions_protocol::UserQuestions>>,
    interaction_retention: InteractionRetention,
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
    fn begin_activity(&self) -> Result<Option<drafts::Activity>> {
        if self.published.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(None);
        }
        self.lease
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("draft lease".into()))?
            .begin()
            .map(Some)
    }

    fn expire_draft(&self) {
        if self.published.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut state = self
            .state
            .try_lock()
            .expect("only an inactive final draft lease expires");
        if matches!(*state, HandleState::Fresh(_)) {
            let old = std::mem::replace(&mut *state, HandleState::Expired);
            drop(state);
            drop(old);
        }
    }

    async fn reconcile_fresh_submission(&self, state: &mut HandleState, accepted: bool) {
        let matching = if accepted {
            true
        } else {
            let Ok(header) = self.store.header(self.header.session_id()).await else {
                return;
            };
            header == self.header
        };
        self.finish_fresh(state, matching);
    }

    fn finish_fresh(&self, state: &mut HandleState, matching: bool) {
        if matching {
            *state = HandleState::Attached;
            self.published
                .store(true, std::sync::atomic::Ordering::Release);
        } else {
            *state = HandleState::Expired;
        }
        if let Some(lease) = &self.lease {
            lease.published();
        }
    }

    /// Serializes a fresh read with publication and returns whether Store history exists.
    async fn reconcile_fresh_read(&self) -> Result<bool> {
        let mut state = self.state.lock().await;
        match &*state {
            HandleState::Attached => return Ok(true),
            HandleState::Expired => return Err(SessionError::NotFound("draft lease".into())),
            HandleState::Fresh(_) => {}
        }
        let durable = match self.store.header(self.header.session_id()).await {
            Ok(header) => header,
            Err(StoreError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(map_store_error(error)),
        };
        let matching = durable == self.header;
        self.finish_fresh(&mut state, matching);
        if matching {
            Ok(true)
        } else {
            Err(SessionError::NotFound(
                "draft Header conflicts with durable Session".into(),
            ))
        }
    }

    async fn prepare_workspace(&self) -> Result<()> {
        let cwd = canonical_workspace_directory(Path::new(self.header.canonical_cwd())).await?;
        if cwd.to_str() != Some(self.header.canonical_cwd()) {
            return Err(SessionError::Invalid(
                "durable Session workspace no longer resolves to its canonical path".into(),
            ));
        }
        self.workspace
            .get_or_create(&cwd)
            .await
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        Ok(())
    }

    async fn prepare_message(&self, request: SubmitInput) -> Result<AgentMessage> {
        self.prepare_workspace().await?;
        let mut content = Vec::with_capacity(request.content.len());
        for block in request.content {
            content.push(match block {
                SessionInput::Text { text } => AgentMessageContent::Text { text },
                SessionInput::Image { media } => {
                    self.media
                        .read(&media)
                        .await
                        .map_err(|error| map_media_error(&error))?;
                    AgentMessageContent::Image { media }
                }
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
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(message)
    }
}

#[async_trait]
impl SessionHandle for LocalSessionHandle {
    async fn read_message(
        &self,
        message_id: &MessageId,
        accepted_control_seq: u64,
    ) -> Result<AgentMessage> {
        let _activity = self.begin_activity()?;
        let after = accepted_control_seq.checked_sub(1).ok_or_else(|| {
            SessionError::Invalid("acceptance control cursor must be positive".into())
        })?;
        let page = self
            .store
            .read_controls(self.header.session_id(), after, 1)
            .await
            .map_err(map_store_error)?;
        let record = page
            .records
            .first()
            .filter(|record| record.seq() == accepted_control_seq)
            .ok_or_else(|| SessionError::NotFound("exact message acceptance".into()))?;
        if let rsi_agent_session_protocol::AgentControlRecordBody::MessageAccepted {
            message, ..
        } = record.body()
            && message.message_id == *message_id
        {
            return Ok(message.clone());
        }
        Err(SessionError::NotFound("exact message acceptance".into()))
    }
    async fn header(&self) -> Result<SessionHeader> {
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        Ok(self.header.clone())
    }

    async fn submit(&self, request: SubmitInput) -> Result<MessageReceipt> {
        let _activity = self.begin_activity()?;
        validate_session_input(&request.content)?;
        let delivery = request.delivery;
        if delivery == rsi_agent_session_protocol::MessageDelivery::NextStep {
            return Err(SessionError::Invalid(
                "human Session input requires NextTurn or Steer intent".into(),
            ));
        }
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
                    delivery,
                })
                .await
                .map_err(map_turn_error);
        }
        let HandleState::Fresh(composition) = &*state else {
            return Err(SessionError::NotFound("draft lease".into()));
        };
        let session = PreparedFreshSession::new(self.header.clone(), composition.clone())
            .map(SubmitSession::Fresh)
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        let message = self.prepare_message(request).await?;
        let result = self
            .turns
            .submit_message(SubmitAgentMessage {
                session,
                message,
                delivery,
            })
            .await;
        self.reconcile_fresh_submission(&mut state, result.is_ok())
            .await;
        result.map_err(map_turn_error)
    }

    async fn message_status(&self, message_id: &MessageId) -> Result<MessageReceipt> {
        let _activity = self.begin_activity()?;
        self.turns
            .message_status(self.header.session_id(), message_id)
            .await
            .map_err(map_turn_error)
    }

    async fn generate_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt> {
        let _activity = self.begin_activity()?;
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
            return Err(SessionError::NotFound("draft lease".into()));
        };
        let session = PreparedFreshSession::new(self.header.clone(), composition.clone())
            .map(SubmitSession::Fresh)
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        let result = self
            .turns
            .submit_image(SubmitImage {
                session,
                turn_id: request.turn_id,
                model: request.model,
                request: request.request,
            })
            .await;
        self.reconcile_fresh_submission(&mut state, result.is_ok())
            .await;
        result.map(TurnReceipt::from).map_err(map_turn_error)
    }

    async fn cancel(&self, target: CancelTarget, reason: Option<String>) -> Result<CancelResult> {
        let _activity = self.begin_activity()?;
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
        let _activity = self.begin_activity()?;
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(SessionError::Invalid(format!(
                "history limit must be within 1..={MAXIMUM_FACTS_PER_READ}"
            )));
        }
        if !self.reconcile_fresh_read().await? {
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
        let _activity = self.begin_activity()?;
        self.turns
            .observe_session(self.header.session_id(), cursor)
            .await
            .map_err(map_turn_error)
    }

    async fn inspect(&self) -> Result<rsi_agent_store_protocol::StoreSessionInspection> {
        let _activity = self.begin_activity()?;
        self.store
            .inspect_session(self.header.session_id())
            .await
            .map_err(|error| SessionError::Backend(error.to_string()))
    }

    async fn pending_questions(&self) -> Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        let _activity = self.begin_activity()?;
        let Some(questions) = &self.questions else {
            return Ok(Vec::new());
        };
        questions
            .pending(self.header.session_id().as_str())
            .await
            .map_err(map_question_error)
    }

    async fn answer_question(
        &self,
        id: &str,
        answer: rsi_user_questions_protocol::QuestionAnswer,
    ) -> Result<bool> {
        let _activity = self.begin_activity()?;
        let questions = self.questions.as_ref().ok_or_else(|| {
            SessionError::Invalid("human questions are unavailable in this Host".into())
        })?;
        questions
            .answer(self.header.session_id().as_str(), id, answer)
            .await
            .map_err(map_question_error)
    }

    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>> {
        let _activity = self.begin_activity()?;
        let sessions = self
            .turns
            .tree_sessions(self.header.session_id())
            .await
            .map_err(map_turn_error)?;
        self.approvals.pending_for_sessions(&sessions).await
    }

    async fn answer_approval(
        &self,
        owner: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<bool> {
        let _activity = self.begin_activity()?;
        let sessions = self
            .turns
            .tree_sessions(self.header.session_id())
            .await
            .map_err(map_turn_error)?;
        if !sessions.contains(owner) {
            return Err(SessionError::Invalid(
                "approval owner is outside the Agent tree".into(),
            ));
        }
        self.approvals.answer(owner, approval_id, decision).await
    }

    async fn observe_interactions(&self) -> Result<rsi_session_protocol::InteractionStream> {
        let _activity = self.begin_activity()?;
        interactions::observe(
            self.header.session_id().clone(),
            self.header
                .fork_origin()
                .map_or(self.header.session_id(), |origin| &origin.root_session_id)
                .clone(),
            self.turns.clone(),
            self.approvals.clone(),
            self.questions.clone(),
            self.interaction_retention.clone(),
        )
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
        .map_err(|error| SessionError::Invalid(format!("workspace: {error}")))?;
    let metadata = tokio::fs::symlink_metadata(&canonical)
        .await
        .map_err(|error| SessionError::Invalid(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(SessionError::Invalid(
            "workspace path is not a directory".into(),
        ));
    }
    Ok(canonical)
}

fn now_ms() -> Result<u64> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SessionError::Backend(error.to_string()))?;
    Ok(u64::try_from(value.as_millis()).unwrap_or(u64::MAX).max(1))
}

fn map_turn_error(error: TurnError) -> SessionError {
    match error {
        TurnError::Invalid(message) => SessionError::Invalid(message),
        TurnError::SessionNotFound(session) => SessionError::NotFound(session),
        TurnError::MessageNotFound { session, message } => {
            SessionError::NotFound(format!("{session}/{message}"))
        }
        TurnError::TurnNotFound { session, turn } => {
            SessionError::NotFound(format!("{session}/{turn}"))
        }
        TurnError::SubmissionConflict { session, turn } => SessionError::Conflict { session, turn },
        TurnError::MessageConflict { session, message } => {
            SessionError::MessageConflict { session, message }
        }
        TurnError::Capacity | TurnError::ObserverCapacity => SessionError::Capacity,
        TurnError::ShuttingDown => SessionError::ShuttingDown,
        other => SessionError::Backend(other.to_string()),
    }
}

fn map_store_error(error: StoreError) -> SessionError {
    match error {
        StoreError::Invalid(message) => SessionError::Invalid(message),
        StoreError::NotFound(value) => SessionError::NotFound(value),
        StoreError::TurnNotFound { session, turn } => {
            SessionError::NotFound(format!("{session}/{turn}"))
        }
        other => SessionError::Backend(other.to_string()),
    }
}

fn map_ai_error(error: &rsi_ai_protocol::AiError) -> SessionError {
    SessionError::Invalid(error.to_string())
}

fn map_workspace_error(error: &rsi_workspace_protocol::WorkspaceError) -> SessionError {
    use rsi_workspace_protocol::WorkspaceError;
    match error {
        WorkspaceError::Api(error) => SessionError::Api(error.clone()),
        WorkspaceError::InvalidInput(_) | WorkspaceError::Unknown(_) => {
            SessionError::Invalid(error.to_string())
        }
        WorkspaceError::Capacity => SessionError::Capacity,
        WorkspaceError::ShuttingDown => SessionError::ShuttingDown,
        WorkspaceError::Storage(_) | WorkspaceError::Corrupt(_) => {
            SessionError::Backend(error.to_string())
        }
    }
}

fn map_media_error(error: &MediaError) -> SessionError {
    let message = error.to_string();
    match error {
        MediaError::Api(error) => SessionError::Api(error.clone()),
        MediaError::InvalidInput(_) | MediaError::Codec(_) => SessionError::Invalid(message),
        MediaError::AdmissionFull(_) => SessionError::Capacity,
        MediaError::NotFound(_) | MediaError::Corrupt(_) | MediaError::Io(_) => {
            SessionError::Backend(message)
        }
    }
}

fn map_question_error(error: rsi_user_questions_protocol::QuestionError) -> SessionError {
    use rsi_user_questions_protocol::QuestionError;
    match error {
        QuestionError::Cancelled => SessionError::ShuttingDown,
        QuestionError::Capacity => SessionError::Capacity,
        error @ (QuestionError::Invalid(_) | QuestionError::Conflict) => {
            SessionError::Invalid(error.to_string())
        }
    }
}
