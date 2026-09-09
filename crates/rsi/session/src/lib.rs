//! Native standard-product Session domain service.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_composition_protocol::{AgentComposition, AgentSessionDraft};
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

mod commands;
mod drafts;
mod interactions;
mod plugin;
mod projections;
mod reads;
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
    projection_service: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
    projection_retention: rsi_session_protocol::ProjectionRetention,
    projection_stopped: tokio_util::sync::CancellationToken,
    execution: rsi_meta::Execution,
    turns: Arc<dyn TurnService>,
    commands: Arc<dyn rsi_agent_turn_protocol::SessionCommands>,
    draft_commands: Arc<commands::DraftCommands>,
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
        commands: Arc<dyn rsi_agent_turn_protocol::SessionCommands>,
        projections: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
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
            projection_service: projections,
            projection_retention: rsi_session_protocol::ProjectionRetention::default(),
            projection_stopped: tokio_util::sync::CancellationToken::new(),
            execution: execution.clone(),
            turns,
            commands,
            draft_commands: commands::DraftCommands::new(),
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

    fn handle_from_state(
        &self,
        state: HandleState,
        lease: Option<drafts::DraftLease>,
    ) -> Arc<LocalSessionHandle> {
        Arc::new(LocalSessionHandle {
            projection_service: self.projection_service.clone(),
            projection_retention: self.projection_retention.clone(),
            projection_stopped: self.projection_stopped.clone(),
            execution: self.execution.clone(),
            projection_changes: Arc::new(tokio::sync::watch::channel(()).0),
            session_id: state
                .header()
                .expect("new handle has a Header")
                .session_id()
                .clone(),
            published: Arc::new(std::sync::atomic::AtomicBool::new(matches!(
                state,
                HandleState::Attached(_)
            ))),
            lease,
            state: Arc::new(Mutex::new(state)),
            turns: Arc::clone(&self.turns),
            commands: self.commands.clone(),
            draft_commands: self.draft_commands.clone(),
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
    async fn attach_local(&self, session_id: &SessionId) -> Result<Arc<LocalSessionHandle>> {
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
        Ok(self.handle_from_state(HandleState::Attached(Box::new(header)), None))
    }

    /// Stops draft admission and waits for service-owned preparation and sweeping.
    pub async fn stop(&self) {
        self.projection_stopped.cancel();
        tokio::join!(self.draft_commands.stop(), self.drafts.stop());
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
        let draft = AgentSessionDraft::new(header, Arc::clone(&self.composition))
            .await
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        Ok(self.handle_from_state(HandleState::Fresh(Box::new(draft)), Some(lease)))
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
        self.attach_local(session_id)
            .await
            .map(|handle| handle as Arc<dyn SessionHandle>)
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
    Fresh(Box<AgentSessionDraft>),
    Attached(Box<SessionHeader>),
    Expired,
}

impl HandleState {
    fn header(&self) -> Result<&SessionHeader> {
        match self {
            Self::Fresh(draft) => Ok(draft.header()),
            Self::Attached(header) => Ok(header),
            Self::Expired => Err(SessionError::NotFound("draft lease".into())),
        }
    }
}

#[derive(Clone)]
struct LocalSessionHandle {
    projection_service: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
    projection_retention: rsi_session_protocol::ProjectionRetention,
    projection_stopped: tokio_util::sync::CancellationToken,
    execution: rsi_meta::Execution,
    projection_changes: Arc<tokio::sync::watch::Sender<()>>,
    session_id: SessionId,
    state: Arc<Mutex<HandleState>>,
    lease: Option<drafts::DraftLease>,
    published: Arc<std::sync::atomic::AtomicBool>,
    turns: Arc<dyn TurnService>,
    commands: Arc<dyn rsi_agent_turn_protocol::SessionCommands>,
    draft_commands: Arc<commands::DraftCommands>,
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
            .field("session_id", self.session_id())
            .finish_non_exhaustive()
    }
}

impl LocalSessionHandle {
    fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    async fn header_snapshot(&self) -> Result<SessionHeader> {
        self.state.lock().await.header().cloned()
    }

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
            self.projection_changed();
            drop(state);
            drop(old);
        }
    }

    async fn reconcile_fresh_submission(&self, state: &mut HandleState, accepted: bool) {
        let matching = if accepted {
            true
        } else {
            let Ok(header) = self.store.header(self.session_id()).await else {
                return;
            };
            state.header().is_ok_and(|candidate| candidate == &header)
        };
        self.finish_fresh(state, matching);
    }

    fn finish_fresh(&self, state: &mut HandleState, matching: bool) {
        if matching {
            let header = state
                .header()
                .expect("fresh publication has a Header")
                .clone();
            *state = HandleState::Attached(Box::new(header));
            self.published
                .store(true, std::sync::atomic::Ordering::Release);
        } else {
            *state = HandleState::Expired;
        }
        if let Some(lease) = &self.lease {
            lease.published();
        }
        self.projection_changed();
    }

    /// Serializes a fresh read with publication and returns whether Store history exists.
    async fn reconcile_fresh_read(&self) -> Result<bool> {
        let mut state = self.state.lock().await;
        match &*state {
            HandleState::Attached(_) => return Ok(true),
            HandleState::Expired => return Err(SessionError::NotFound("draft lease".into())),
            HandleState::Fresh(_) => {}
        }
        let durable = match self.store.header(self.session_id()).await {
            Ok(header) => header,
            Err(StoreError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(map_store_error(error)),
        };
        let matching = state.header().is_ok_and(|header| header == &durable);
        self.finish_fresh(&mut state, matching);
        if matching {
            Ok(true)
        } else {
            Err(SessionError::NotFound(
                "draft Header conflicts with durable Session".into(),
            ))
        }
    }

    async fn prepare_workspace(&self, header: &SessionHeader) -> Result<()> {
        let cwd = canonical_workspace_directory(Path::new(header.canonical_cwd())).await?;
        if cwd.to_str() != Some(header.canonical_cwd()) {
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

    async fn prepare_message(
        &self,
        request: SubmitInput,
        header: &SessionHeader,
    ) -> Result<AgentMessage> {
        self.prepare_workspace(header).await?;
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
    async fn observe_projections(&self) -> Result<rsi_session_protocol::ProjectionStream> {
        self.projection_stream().await
    }
    async fn draft_snapshot(&self) -> Result<rsi_session_protocol::SessionDraftView> {
        self.read_draft_snapshot().await
    }
    async fn select_preset(
        &self,
        request: rsi_session_protocol::SelectDraftPreset,
    ) -> Result<rsi_session_protocol::SessionDraftView> {
        self.draft_commands
            .select_preset(self.clone(), request)
            .await
    }
    async fn commands(&self) -> Result<rsi_agent_session_protocol::SessionCommandsView> {
        self.list_commands().await
    }
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> Result<rsi_agent_session_protocol::SessionCommandReceipt> {
        self.dispatch_command(invocation).await
    }
    async fn command_status(
        &self,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>> {
        self.lookup_command(request_id).await
    }
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
            .read_controls(self.session_id(), after, 1)
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
        self.header_snapshot().await
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
        let header = self.header_snapshot().await?;
        self.language
            .describe(
                request
                    .model
                    .as_ref()
                    .unwrap_or_else(|| header.settings().default_model()),
            )
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        if matches!(*state, HandleState::Attached(_)) {
            drop(state);
            let session = self
                .turns
                .prepare_resume(self.session_id())
                .await
                .map(SubmitSession::Resume)
                .map_err(map_turn_error)?;
            let message = self.prepare_message(request, &header).await?;
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
        let HandleState::Fresh(draft) = &*state else {
            return Err(SessionError::NotFound("draft lease".into()));
        };
        let session = SubmitSession::Fresh(draft.freeze());
        let message = self.prepare_message(request, &header).await?;
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
            .message_status(self.session_id(), message_id)
            .await
            .map_err(map_turn_error)
    }

    async fn generate_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt> {
        let _activity = self.begin_activity()?;
        self.image
            .describe(&request.model)
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        if matches!(*state, HandleState::Attached(_)) {
            drop(state);
            let session = self
                .turns
                .prepare_resume(self.session_id())
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
        let HandleState::Fresh(draft) = &*state else {
            return Err(SessionError::NotFound("draft lease".into()));
        };
        let session = SubmitSession::Fresh(draft.freeze());
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
            .cancel_target(self.session_id(), target, reason)
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
            .read_facts_before(self.session_id(), exclusive_before_seq.unwrap_or(0), limit)
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
            .observe_session(self.session_id(), cursor)
            .await
            .map_err(map_turn_error)
    }

    async fn inspect(&self) -> Result<rsi_agent_store_protocol::StoreSessionInspection> {
        let _activity = self.begin_activity()?;
        self.store
            .inspect_session(self.session_id())
            .await
            .map_err(|error| SessionError::Backend(error.to_string()))
    }

    async fn pending_questions(&self) -> Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        let _activity = self.begin_activity()?;
        let Some(questions) = &self.questions else {
            return Ok(Vec::new());
        };
        questions
            .pending(self.session_id().as_str())
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
            .answer(self.session_id().as_str(), id, answer)
            .await
            .map_err(map_question_error)
    }

    async fn pending_approvals(&self) -> Result<Vec<ApprovalRequest>> {
        let _activity = self.begin_activity()?;
        let sessions = self
            .turns
            .tree_sessions(self.session_id())
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
            .tree_sessions(self.session_id())
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
        let header = self.header_snapshot().await?;
        interactions::observe(
            self.session_id().clone(),
            header
                .fork_origin()
                .map_or(self.session_id(), |origin| &origin.root_session_id)
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
