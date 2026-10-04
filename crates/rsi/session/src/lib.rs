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
    CancelResult, CancelTarget, MessageReceipt, ObservationCursor, SubmitImage,
    SubmitMessage as SubmitAgentMessage, SubmitSession, TurnError, TurnService,
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

mod access;
mod activity;
mod commands;
mod drafts;
mod goal;
mod interactions;
mod plugin;
mod projections;
mod reads;
mod references;
mod resources;
mod terminal;
mod workflow;
pub use plugin::SessionFactory;
use references::map_reference_error;

use rsi_session_protocol::{
    AgentSettingsSource, CreateSession, InteractionRetention, RecentSessionCursor,
    RecentSessionPage, Result, SessionApprovalControl, SessionError, SessionHandle,
    SessionHistoryPage, SessionInput, SessionObservationStream, SessionService, SessionSummary,
    SubmitDirectImage, SubmitInput, TurnReceipt, validate_session_input,
};

/// Process-local adapter over the Agent Kernel and mechanical Store.
#[derive(Clone)]
pub struct LocalSessionService {
    origin: rsi_api_protocol::CallOrigin,
    workflow: Option<Arc<dyn rsi_session_protocol::WorkflowReadinessSource>>,
    resolver: Option<Arc<dyn rsi_execution::ExecutionResolver>>,
    activity: Arc<activity::Managed>,
    terminals: Option<Arc<terminal::Terminals>>,
    resources: Option<Arc<dyn rsi_agent_turn_protocol::SessionResources>>,
    references: Option<Arc<rsi_agent_references::References>>,
    resource_retention: rsi_session_protocol::ResourceRetention,
    jobs: Option<Arc<dyn rsi_agent_turn_protocol::TurnJobs>>,
    jobs_retention: rsi_session_protocol::JobsRetention,
    preview_workers: Arc<tokio::sync::Semaphore>,
    export_workers: Arc<tokio::sync::Semaphore>,
    goals: Option<Arc<dyn rsi_goal::GoalController>>,
    continuations: Option<Arc<dyn rsi_agent_turn_protocol::SessionContinuations>>,
    projection_service: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
    projection_retention: rsi_session_protocol::ProjectionRetention,
    projection_stopped: tokio_util::sync::CancellationToken,
    metrics: Arc<metrics::MetricsCache>,
    execution: rsi_meta::Execution,
    turns: Arc<dyn TurnService>,
    commands: Arc<dyn rsi_agent_turn_protocol::SessionCommands>,
    draft_commands: Arc<commands::DraftCommands>,
    store: Arc<dyn SessionStore>,
    context_budget: rsi_agent_context::ContextBudget,
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
    /// Supplies optional product-owned Workflow requirements; absence never blocks standard Sessions.
    #[must_use]
    pub fn with_workflow(
        mut self,
        source: Option<Arc<dyn rsi_session_protocol::WorkflowReadinessSource>>,
    ) -> Self {
        self.workflow = source;
        self
    }
    /// Supplies target selection and revocable caller admission for this service generation.
    #[must_use]
    pub fn with_execution(mut self, resolver: Arc<dyn rsi_execution::ExecutionResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }
    /// Supplies one generation-owned terminal registry, independent of Agent residency.
    #[must_use]
    pub fn with_terminals(
        mut self,
        provider: Arc<dyn rsi_pty_protocol::PtyProvider>,
        sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    ) -> Self {
        self.terminals = Some(Arc::new(terminal::Terminals::new(provider, sandbox)));
        self
    }
    /// Supplies the bounded immutable conversation reference owner.
    #[must_use]
    pub fn with_references(mut self, references: Arc<rsi_agent_references::References>) -> Self {
        self.references = Some(references);
        self
    }
    /// Supplies the independent finite resource reader from the owning Kernel.
    #[must_use]
    pub fn with_resources(
        mut self,
        resources: Arc<dyn rsi_agent_turn_protocol::SessionResources>,
    ) -> Self {
        self.resources = Some(resources);
        self
    }
    /// Creates one local adapter from already-owned Host dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        execution: rsi_meta::Execution,
        commands: Arc<dyn rsi_agent_turn_protocol::SessionCommands>,
        projections: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
        turns: Arc<dyn TurnService>,
        store: Arc<dyn SessionStore>,
        context_budget: rsi_agent_context::ContextBudget,
        composition: Arc<dyn AgentComposition>,
        workspace: Arc<dyn WorkspaceRegistry>,
        settings: Arc<dyn AgentSettingsSource>,
        language: Arc<dyn LanguageCall>,
        image: Arc<dyn ImageCall>,
        media: Arc<dyn Media>,
        approvals: Arc<dyn SessionApprovalControl>,
    ) -> Self {
        Self {
            origin: rsi_api_protocol::CallOrigin::Local,
            workflow: None,
            resolver: None,
            activity: Arc::new(activity::Managed::default()),
            terminals: None,
            resources: None,
            references: None,
            resource_retention: rsi_session_protocol::ResourceRetention::default(),
            goals: None,
            jobs: None,
            jobs_retention: rsi_session_protocol::JobsRetention::default(),
            preview_workers: Arc::new(tokio::sync::Semaphore::new(2)),
            export_workers: Arc::new(tokio::sync::Semaphore::new(2)),
            continuations: None,
            projection_service: projections,
            projection_retention: rsi_session_protocol::ProjectionRetention::default(),
            projection_stopped: tokio_util::sync::CancellationToken::new(),
            metrics: Arc::new(metrics::MetricsCache::default()),
            execution: execution.clone(),
            turns,
            commands,
            draft_commands: commands::DraftCommands::new(),
            store,
            context_budget,
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

    /// Supplies the independently owned Host Goal controller and its Kernel admission seam.
    #[must_use]
    pub fn with_goals(
        mut self,
        goals: Arc<dyn rsi_goal::GoalController>,
        continuations: Arc<dyn rsi_agent_turn_protocol::SessionContinuations>,
    ) -> Self {
        self.goals = Some(goals);
        self.continuations = Some(continuations);
        self
    }

    /// Supplies the Kernel's current-claim read-only Jobs relay.
    #[must_use]
    pub fn with_jobs(mut self, jobs: Arc<dyn rsi_agent_turn_protocol::TurnJobs>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    fn handle_from_state(
        &self,
        state: HandleState,
        lease: Option<drafts::DraftLease>,
    ) -> Arc<LocalSessionHandle> {
        self.activity.opened(
            state
                .header()
                .expect("new handle has a Header")
                .session_id(),
        );
        Arc::new(LocalSessionHandle {
            origin: self.origin.clone(),
            workflow: self.workflow.clone(),
            resolver: self.resolver.clone(),
            coordinates: state
                .header()
                .expect("new handle has a Header")
                .coordinates()
                .clone(),
            terminals: self.terminals.clone(),
            references: self.references.clone(),
            resources: self.resources.clone(),
            resource_retention: self.resource_retention.clone(),
            goals: self.goals.clone(),
            continuations: self.continuations.clone(),
            jobs: self.jobs.clone(),
            jobs_retention: self.jobs_retention.clone(),
            preview_workers: self.preview_workers.clone(),
            export_workers: self.export_workers.clone(),
            projection_service: self.projection_service.clone(),
            projection_retention: self.projection_retention.clone(),
            projection_stopped: self.projection_stopped.clone(),
            metrics: self.metrics.clone(),
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
            context_budget: self.context_budget.clone(),
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
        self.check_origin()?;
        if let Some(handle) = self.drafts.get(session_id).await? {
            let handle = self.bind_handle(&handle)?;
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
        self.admit(header.coordinates().location())?;
        Ok(self.handle_from_state(HandleState::Attached(Arc::new(header)), None))
    }

    /// Stops draft admission and waits for service-owned preparation and sweeping.
    pub async fn stop(&self) -> Result<()> {
        self.projection_stopped.cancel();
        let terminals = if let Some(terminals) = &self.terminals {
            terminals.stop().await
        } else {
            Ok(())
        };
        let (commands, ()) = tokio::join!(self.draft_commands.stop(), self.drafts.stop());
        terminals.and(commands)
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
        let _admission = self.admit(workspace.coordinates.location())?;
        let agent_preset_id = match request.agent_preset_id {
            Some(id) => id,
            None => self
                .composition
                .default_preset_id()
                .await
                .map_err(|error| SessionError::Backend(error.to_string()))?,
        };
        workflow::require_preset(self.workflow.as_deref(), &agent_preset_id)?;
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
        let header = SessionHeader::new(
            session_id,
            now_ms()?,
            workspace.coordinates,
            agent_preset_id,
            settings,
        )
        .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let draft = AgentSessionDraft::new(header, Arc::clone(&self.composition))
            .await
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        Ok(self.handle_from_state(HandleState::Fresh(Box::new(draft)), Some(lease)))
    }
}

impl rsi_session_protocol::SessionDraftControl for LocalSessionService {
    fn release_draft(&self, session: &SessionId) -> rsi_session_protocol::DraftRelease {
        self.drafts.release(session)
    }
}

#[async_trait]
impl SessionService for LocalSessionService {
    async fn read_header(&self, session_id: &SessionId) -> Result<SessionHeader> {
        self.check_origin()?;
        let header = self
            .store
            .header(session_id)
            .await
            .map_err(map_store_error)?;
        let _admission = self.admit(header.coordinates().location())?;
        Ok(header)
    }
    async fn activity(&self) -> Result<rsi_session_protocol::SessionActivityPage> {
        self.collect_activity().await
    }
    async fn create(&self, request: CreateSession) -> Result<Arc<dyn SessionHandle>> {
        self.check_origin()?;
        self.drafts.accepting()?;
        let device = match &self.origin {
            rsi_api_protocol::CallOrigin::Local => None,
            rsi_api_protocol::CallOrigin::Device(device) => Some(device.id.clone()),
        };
        let handle = self.drafts.create(self.clone(), request, device).await?;
        self.bind_handle(&handle)
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
        self.visible_recent(after, limit).await
    }
}

#[async_trait]
impl rsi_session_protocol::SessionIngress for LocalSessionService {
    fn scoped(&self, origin: rsi_api_protocol::CallOrigin) -> Arc<dyn SessionService> {
        Arc::new(Self {
            origin,
            ..self.clone()
        })
    }
    async fn create_from(
        &self,
        request: CreateSession,
        origin: rsi_api_protocol::CallOrigin,
    ) -> Result<Arc<dyn SessionHandle>> {
        Self {
            origin,
            ..self.clone()
        }
        .create(request)
        .await
    }
}

enum HandleState {
    Fresh(Box<AgentSessionDraft>),
    Attached(Arc<SessionHeader>),
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
    origin: rsi_api_protocol::CallOrigin,
    workflow: Option<Arc<dyn rsi_session_protocol::WorkflowReadinessSource>>,
    resolver: Option<Arc<dyn rsi_execution::ExecutionResolver>>,
    coordinates: rsi_workspace_protocol::ExecutionCoordinates,
    terminals: Option<Arc<terminal::Terminals>>,
    references: Option<Arc<rsi_agent_references::References>>,
    resources: Option<Arc<dyn rsi_agent_turn_protocol::SessionResources>>,
    resource_retention: rsi_session_protocol::ResourceRetention,
    jobs: Option<Arc<dyn rsi_agent_turn_protocol::TurnJobs>>,
    jobs_retention: rsi_session_protocol::JobsRetention,
    preview_workers: Arc<tokio::sync::Semaphore>,
    export_workers: Arc<tokio::sync::Semaphore>,
    goals: Option<Arc<dyn rsi_goal::GoalController>>,
    continuations: Option<Arc<dyn rsi_agent_turn_protocol::SessionContinuations>>,
    projection_service: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
    projection_retention: rsi_session_protocol::ProjectionRetention,
    projection_stopped: tokio_util::sync::CancellationToken,
    metrics: Arc<metrics::MetricsCache>,
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
    context_budget: rsi_agent_context::ContextBudget,
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

    async fn header_snapshot(&self) -> Result<Arc<SessionHeader>> {
        let state = self.state.lock().await;
        match &*state {
            HandleState::Attached(header) => Ok(Arc::clone(header)),
            _ => state.header().cloned().map(Arc::new),
        }
    }

    fn begin_activity(&self) -> Result<Option<drafts::Activity>> {
        begin_draft_activity(&self.published, || {
            self.lease
                .as_ref()
                .ok_or_else(|| SessionError::NotFound("draft lease".into()))?
                .begin()
        })
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
            *state = HandleState::Attached(Arc::new(header));
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

    async fn prepare_workspace(
        &self,
        header: &SessionHeader,
    ) -> Result<Option<rsi_execution::ExecutionLease>> {
        let execution = self.execution_lease()?;
        self.validate_workspace(header, execution.as_ref()).await?;
        Ok(execution)
    }

    async fn validate_workspace(
        &self,
        header: &SessionHeader,
        execution: Option<&rsi_execution::ExecutionLease>,
    ) -> Result<()> {
        if let Some(execution) = execution {
            let coordinates = execution
                .canonicalize(header.canonical_cwd())
                .await
                .map_err(|_| SessionError::Api(rsi_api_protocol::ApiError::Unavailable))?;
            if &coordinates != header.coordinates() {
                return Err(SessionError::Invalid(
                    "durable Session workspace changed on its execution target".into(),
                ));
            }
            return Ok(());
        }
        access::require_native(header.coordinates().location())?;
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

    async fn prepare_input_content(
        &self,
        blocks: Vec<SessionInput>,
        header: &SessionHeader,
    ) -> Result<(
        Vec<AgentMessageContent>,
        Option<rsi_execution::ExecutionLease>,
    )> {
        validate_session_input(&blocks)?;
        let execution = self.prepare_workspace(header).await?;
        let mut content = Vec::with_capacity(blocks.len());
        for block in blocks {
            content.push(match block {
                SessionInput::Text { text } => AgentMessageContent::Text { text },
                SessionInput::Image { media } => {
                    self.media
                        .read(&media)
                        .await
                        .map_err(|error| map_media_error(&error))?;
                    AgentMessageContent::Image { media }
                }
                SessionInput::Reference { reference } => {
                    self.reference_owner()?
                        .verify(
                            header.clone(),
                            reference.clone(),
                            self.projection_stopped.clone(),
                        )
                        .await
                        .map_err(map_reference_error)?;
                    AgentMessageContent::Reference { reference }
                }
            });
        }
        Ok((content, execution))
    }

    async fn prepare_message(
        &self,
        request: SubmitInput,
        header: &SessionHeader,
    ) -> Result<(AgentMessage, Option<rsi_execution::ExecutionLease>)> {
        let (content, execution) = self.prepare_input_content(request.content, header).await?;
        let message = AgentMessage {
            message_id: request.message_id,
            source: AgentMessageSource::Human,
            content,
            options: MessageOptions {
                reasoning_effort: request.reasoning_effort,
                model: request.model,
                sandbox: request.sandbox,
            },
        };
        message
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok((message, execution))
    }
}

#[async_trait]
impl SessionHandle for LocalSessionHandle {
    async fn terminal(
        &self,
        request: rsi_session_protocol::terminal::Request,
    ) -> Result<rsi_session_protocol::terminal::Reply> {
        let _admission = self.admit()?;
        self.terminal_request(request).await
    }
    async fn read_recorded_reference(
        &self,
        request: rsi_agent_session_protocol::ReferenceReadRequest,
    ) -> Result<rsi_agent_session_protocol::ReferenceTextPage> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let header = self.header_snapshot().await?;
        self.reference_owner()?
            .read_recorded((*header).clone(), request, self.projection_stopped.clone())
            .await
            .map_err(map_reference_error)
    }
    async fn capture_reference(
        &self,
        source: SessionId,
    ) -> Result<rsi_agent_session_protocol::FrozenReference> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let header = self.header_snapshot().await?;
        self.reference_owner()?
            .capture(source, (*header).clone(), self.projection_stopped.clone())
            .await
            .map_err(map_reference_error)
    }
    async fn preview_reference(
        &self,
        reference: rsi_agent_session_protocol::FrozenReference,
        offset: usize,
        maximum: usize,
    ) -> Result<rsi_agent_session_protocol::ReferenceTextPage> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let header = self.header_snapshot().await?;
        self.reference_owner()?
            .preview(
                (*header).clone(),
                reference,
                offset,
                maximum,
                self.projection_stopped.clone(),
            )
            .await
            .map_err(map_reference_error)
    }
    async fn read_resource(
        &self,
        request: rsi_agent_session_protocol::SessionResourceRequest,
    ) -> Result<rsi_session_protocol::ResourceSnapshot> {
        let _admission = self.admit()?;
        self.resource_read(request).await
    }
    async fn peek_job(
        &self,
        request: rsi_agent_turn_protocol::JobPreviewRequest,
    ) -> Result<rsi_agent_turn_protocol::JobPreviewPage> {
        let _admission = self.admit()?;
        request.validate().map_err(map_turn_error)?;
        let _activity = self.begin_activity()?;
        let _permit = self
            .preview_workers
            .try_acquire()
            .map_err(|_| SessionError::Api(rsi_api_protocol::ApiError::Capacity))?;
        let service = self
            .jobs
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("live job output".into()))?;
        let header = self
            .header_snapshot()
            .await?
            .fingerprint()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let cancellation = self.projection_stopped.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let page = tokio::select! {
            () = cancellation.cancelled() => return Err(SessionError::ShuttingDown),
            result = service.peek_job(self.session_id(), &header, request.clone(), cancellation.clone()) => result,
        }.map_err(|error| match error {
            TurnError::StaleClaim => SessionError::NotFound("live job output".into()),
            error => map_turn_error(error),
        })?;
        Ok(page)
    }
    async fn evidence(
        &self,
        request: rsi_session_protocol::EvidenceRead,
    ) -> Result<rsi_session_protocol::EvidencePage> {
        let _admission = self.admit()?;
        request.validate()?;
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        tokio::select! {
            biased;
            () = self.projection_stopped.cancelled() => Err(SessionError::ShuttingDown),
            result = evidence::read(self.store.as_ref(), &self.session_id, &request) => result,
        }
    }
    async fn read_jobs(
        &self,
        request: rsi_agent_turn_protocol::TurnJobsRequest,
    ) -> Result<rsi_session_protocol::JobsSnapshot> {
        let _admission = self.admit()?;
        request.validate().map_err(map_turn_error)?;
        let service = self
            .jobs
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("current-Turn Jobs".into()))?;
        let reservation = self.jobs_retention.reserve_capture()?;
        let header = self
            .header_snapshot()
            .await?
            .fingerprint()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let cancellation = self.projection_stopped.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let page = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            service.read_jobs(self.session_id(), &header, request.clone(), cancellation),
        )
        .await
        .map_err(|_| SessionError::Backend("Jobs status read deadline elapsed".into()))?
        .map_err(|error| match error {
            TurnError::StaleClaim => SessionError::NotFound("current-Turn Jobs source".into()),
            error => map_turn_error(error),
        })?;
        reservation.retain(page)
    }
    async fn control_goal(
        &self,
        request: rsi_goal::GoalControl,
    ) -> Result<rsi_goal::GoalControlReceipt> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        let request_id = request.request_id.clone();
        self.goals
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("Goal controller".into()))?
            .control(Arc::new(self.clone()), request)
            .await
            .map_err(|error| match error {
                rsi_goal::GoalError::Conflict => SessionError::CommandConflict { request_id },
                other => goal::session_error(other),
            })
    }
    async fn goal_status(&self) -> Result<rsi_goal::GoalLiveState> {
        let _admission = self.admit()?;
        self.goals
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("Goal controller".into()))?
            .status(self.session_id())
            .map_err(goal::session_error)
    }
    async fn observe_goal(&self) -> Result<rsi_session_protocol::GoalStream> {
        use futures_util::StreamExt;
        let _admission = self.admit()?;
        let stream = self
            .goals
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("Goal controller".into()))?
            .observe(self.session_id())
            .map_err(goal::session_error)?;
        Ok(self.guard_stream(stream.map(|item| item.map_err(goal::session_error))))
    }
    async fn observe_projections(&self) -> Result<rsi_session_protocol::ProjectionStream> {
        let _admission = self.admit()?;
        Ok(self.guard_stream(self.projection_stream().await?))
    }
    async fn draft_snapshot(&self) -> Result<rsi_session_protocol::SessionDraftView> {
        let _admission = self.admit()?;
        self.read_draft_snapshot().await
    }
    async fn select_preset(
        &self,
        request: rsi_session_protocol::SelectDraftPreset,
    ) -> Result<rsi_session_protocol::SessionDraftView> {
        let _admission = self.admit()?;
        self.draft_commands
            .select_preset(self.clone(), request)
            .await
    }
    async fn commands(&self) -> Result<rsi_agent_session_protocol::SessionCommandsView> {
        let _admission = self.admit()?;
        self.list_commands().await
    }
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> Result<rsi_agent_session_protocol::SessionCommandReceipt> {
        let _admission = self.admit()?;
        self.dispatch_command(invocation).await
    }
    async fn command_status(
        &self,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>> {
        let _admission = self.admit()?;
        self.lookup_command(request_id).await
    }
    async fn read_message(
        &self,
        message_id: &MessageId,
        accepted_control_seq: u64,
    ) -> Result<AgentMessage> {
        let _admission = self.admit()?;
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
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        self.header_snapshot().await.map(|header| (*header).clone())
    }

    async fn submit(&self, request: SubmitInput) -> Result<MessageReceipt> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        validate_session_input(&request.content)?;
        let delivery = request.delivery;
        if delivery == rsi_agent_session_protocol::MessageDelivery::NextStep {
            return Err(SessionError::Invalid(
                "human Session input requires NextTurn or Steer intent".into(),
            ));
        }
        let header = self.header_snapshot().await?;
        if request.reasoning_effort.is_some() && request.model.is_none() {
            return Err(SessionError::Invalid(
                "effort override requires an explicit model".into(),
            ));
        }
        let selected = if let Some(model) = &request.model {
            rsi_agent_session_protocol::ModelSelection {
                model: model.clone(),
                reasoning_effort: request.reasoning_effort.clone(),
            }
        } else {
            self.current_model_selection(&header).await?
        };
        self.validate_model_selection(&selected)?;
        let mut state = self.state.lock().await;
        if let HandleState::Attached(attached_header) = &*state {
            workflow::require_preset(self.workflow.as_deref(), attached_header.agent_preset_id())?;
            drop(state);
            let session = self
                .turns
                .prepare_resume(self.session_id())
                .await
                .map(SubmitSession::Resume)
                .map_err(map_turn_error)?;
            let (message, execution) = self.prepare_message(request, &header).await?;
            return self
                .turns
                .submit_message(SubmitAgentMessage {
                    session: Self::bind_submission(session, execution)?,
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
        let (message, execution) = self.prepare_message(request, &header).await?;
        let result = self
            .turns
            .submit_message(SubmitAgentMessage {
                session: Self::bind_submission(session, execution)?,
                message,
                delivery,
            })
            .await;
        self.reconcile_fresh_submission(&mut state, result.is_ok())
            .await;
        result.map_err(map_turn_error)
    }

    async fn message_status(&self, message_id: &MessageId) -> Result<MessageReceipt> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.turns
            .message_status(self.session_id(), message_id)
            .await
            .map_err(map_turn_error)
    }

    async fn mutate_queue(
        &self,
        mut request: rsi_agent_session_protocol::QueueMutationRequest,
    ) -> Result<rsi_agent_session_protocol::QueueMutationReceipt> {
        use rsi_agent_session_protocol::QueueMutation;
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        request
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.reconcile_fresh_read().await?;
        // A committed retry is resolved before reconnecting or reading media.
        let committed = self
            .turns
            .queue_mutation_status(self.session_id(), &request.operation_id)
            .await
            .map_err(map_turn_error)?
            .is_some();
        let mut execution = None;
        if !committed {
            match &mut request.mutation {
                QueueMutation::Replace { content, .. } => {
                    let header = self.header_snapshot().await?;
                    let input = std::mem::take(content)
                        .into_iter()
                        .map(|block| match block {
                            AgentMessageContent::Text { text } => SessionInput::Text { text },
                            AgentMessageContent::Image { media } => SessionInput::Image { media },
                            AgentMessageContent::Reference { reference } => {
                                SessionInput::Reference { reference }
                            }
                        })
                        .collect();
                    let prepared = self.prepare_input_content(input, &header).await?;
                    *content = prepared.0;
                    execution = prepared.1;
                }
                QueueMutation::ConvertToSteer { .. } => {
                    execution = self
                        .prepare_workspace(self.header_snapshot().await?.as_ref())
                        .await?;
                }
                QueueMutation::Withdraw => {}
            }
        }
        self.turns
            .mutate_queue(self.session_id(), request, execution)
            .await
            .map_err(map_turn_error)
    }
    async fn queue_mutation_status(
        &self,
        operation: &rsi_agent_session_protocol::QueueOperationId,
    ) -> Result<Option<rsi_agent_session_protocol::QueueMutationReceipt>> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.turns
            .queue_mutation_status(self.session_id(), operation)
            .await
            .map_err(map_turn_error)
    }
    async fn generate_image(&self, request: SubmitDirectImage) -> Result<TurnReceipt> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.image
            .describe(&request.model)
            .map_err(|error| map_ai_error(&error))?;
        let mut state = self.state.lock().await;
        if let HandleState::Attached(attached_header) = &*state {
            workflow::require_preset(self.workflow.as_deref(), attached_header.agent_preset_id())?;
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
                    session: Self::bind_submission(session, self.execution_lease()?)?,
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
                session: Self::bind_submission(session, self.execution_lease()?)?,
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
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.turns
            .cancel_target(self.session_id(), target, reason)
            .await
            .map_err(map_turn_error)
    }

    async fn export(
        &self,
        options: rsi_session_protocol::export::ExportOptions,
    ) -> Result<rsi_session_protocol::export::ExportStream> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        options.validate()?;
        let published = self.reconcile_fresh_read().await?;
        let header = self.header_snapshot().await?;
        let permit = self
            .export_workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        let mut source = if published {
            rsi_session_export::export(
                self.store.clone(),
                (*header).clone(),
                options,
                self.projection_stopped.clone(),
                self.context_budget.clone(),
            )
            .await?
        } else {
            rsi_session_export::empty(
                (*header).clone(),
                options,
                self.projection_stopped.clone(),
                self.context_budget.clone(),
            )?
        };
        Ok(self.guard_stream(async_stream::try_stream! {
            let _permit = permit;
            while let Some(item) = futures_util::StreamExt::next(&mut source).await { yield item?; }
        }))
    }

    async fn history_before(
        &self,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> Result<SessionHistoryPage> {
        let _admission = self.admit()?;
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
        use futures_util::StreamExt as _;
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        let source = self
            .turns
            .observe_session(self.session_id(), cursor)
            .await
            .map_err(map_turn_error)?;
        Ok(self.guard_stream(source.map(|item| item.map_err(map_turn_error))))
    }

    async fn metrics(&self) -> Result<rsi_session_protocol::MetricsRead> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        let durable = self.reconcile_fresh_read().await?;
        let header = self.header_snapshot().await?;
        let current_model = self.current_model_read(&header).await?;
        if !durable {
            return Ok(rsi_session_protocol::MetricsRead {
                current_model,
                session_id: self.session_id.clone(),
                watermark: 0,
                complete: true,
                summary: rsi_conversation::SessionMetrics::default(),
            });
        }
        tokio::select! {
            biased;
            () = self.projection_stopped.cancelled() => Err(SessionError::ShuttingDown),
            result = self.metrics.read(self.store.as_ref(), &self.session_id) => {
                let mut progress = result?;
                if let Some(context) = &progress.summary.last_context {
                    let matches = match &current_model.availability {
                        rsi_session_protocol::ModelAvailability::Available {description} => &context.description == description,
                        rsi_session_protocol::ModelAvailability::Unavailable {..} => false,
                    };
                    if !matches {progress.summary.last_context = None;}
                }
                Ok(rsi_session_protocol::MetricsRead {current_model,session_id:self.session_id.clone(),watermark:progress.watermark,complete:progress.complete,summary:progress.summary})
            },
        }
    }

    async fn tree_metrics(&self, refresh: bool) -> Result<rsi_session_protocol::TreeMetricsRead> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        if !self.reconcile_fresh_read().await? {
            return Ok(rsi_session_protocol::TreeMetricsRead {
                session_id: self.session_id.clone(),
                membership_control_seq: 0,
                membership_complete: true,
                complete: true,
                members: vec![rsi_session_protocol::TreeMetricsMember {
                    session_id: self.session_id.clone(),
                    watermark: Some(0),
                    through_seq: 0,
                    complete: true,
                }],
                totals: rsi_conversation::UsageTotals::default(),
            });
        }
        tokio::select! {biased;
            ()=self.projection_stopped.cancelled()=>Err(SessionError::ShuttingDown),
            result=self.metrics.tree(self.store.as_ref(),&self.session_id,refresh)=>result,
        }
    }

    async fn workflow_readiness(&self) -> Result<rsi_session_protocol::WorkflowReadiness> {
        self.read_workflow_readiness().await
    }
    async fn list_workflows(
        &self,
        request: rsi_session_protocol::WorkflowList,
    ) -> Result<rsi_session_protocol::WorkflowPage> {
        self.workflow_list(request).await
    }
    async fn read_workflow(
        &self,
        request: rsi_session_protocol::WorkflowRead,
    ) -> Result<rsi_session_protocol::WorkflowDetail> {
        self.workflow_detail(request).await
    }
    async fn cancel_workflow(
        &self,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> Result<rsi_agent_turn_protocol::ProgramCancelReceipt> {
        self.workflow_cancel(run).await
    }
    async fn inspect(&self) -> Result<rsi_agent_store_protocol::StoreSessionInspection> {
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        self.store
            .inspect_session(self.session_id())
            .await
            .map_err(map_store_error)
    }

    async fn pending_questions(&self) -> Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        let _admission = self.admit()?;
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
        let _admission = self.admit()?;
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
        let _admission = self.admit()?;
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
        let _admission = self.admit()?;
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
        let _admission = self.admit()?;
        let _activity = self.begin_activity()?;
        let header = self.header_snapshot().await?;
        let stream = interactions::observe(
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
        .await?;
        Ok(self.guard_stream(stream))
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
        TurnError::Capacity | TurnError::ObserverCapacity | TurnError::ProjectionCapacity => {
            SessionError::Capacity
        }
        TurnError::ExecutionUnavailable => {
            SessionError::Api(rsi_api_protocol::ApiError::Unavailable)
        }
        TurnError::ExecutionOutcomeUnknown => {
            SessionError::Api(rsi_api_protocol::ApiError::OutcomeUnknown)
        }
        TurnError::ShuttingDown => SessionError::ShuttingDown,
        TurnError::QueueOperationConflict => SessionError::QueueOperationConflict,
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

fn begin_draft_activity<T>(
    published: &std::sync::atomic::AtomicBool,
    begin: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    if published.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(None);
    }
    match begin() {
        Err(SessionError::NotFound(_)) if published.load(std::sync::atomic::Ordering::Acquire) => {
            Ok(None)
        }
        result => result.map(Some),
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

#[cfg(test)]
mod publication_admission_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn publication_between_check_and_lease_admission_preserves_durable_activity() {
        let published = AtomicBool::new(false);
        // finish_fresh publishes before retiring its lease. Force that exact
        // interleaving after the first read, without timing or a retry loop.
        let result = begin_draft_activity::<()>(&published, || {
            published.store(true, Ordering::Release);
            Err(SessionError::NotFound("retired draft lease".into()))
        });
        assert!(matches!(result, Ok(None)), "{result:?}");
    }
    #[test]
    fn expiry_and_shutdown_are_not_successful_publication() {
        let published = AtomicBool::new(false);
        assert!(matches!(
            begin_draft_activity::<()>(&published, || Err(SessionError::NotFound(
                "expired".into()
            ))),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            begin_draft_activity::<()>(&published, || {
                published.store(true, Ordering::Release);
                Err(SessionError::ShuttingDown)
            }),
            Err(SessionError::ShuttingDown)
        ));
    }
}

mod evidence;
mod metrics;
mod model_selection;
