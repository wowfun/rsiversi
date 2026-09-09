use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionError, AgentCompositionPin,
};
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS,
    MAXIMUM_TURN_TEXT_BYTES, MessageId, SessionFact, SessionFactBody, SessionHeader, SessionId,
    TurnId, WorkspaceTrust,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore};
use rsi_agent_testkit::MemoryStore;
use rsi_agent_turn_protocol::{
    CancelResult, MessageReceipt, MessageState, PreparedResumeSession, Result as TurnResult,
    ResumeAdmissionIssuer, SubmitImage, SubmitMessage, SubmitSession, SubmitTurn, SubmittedTurn,
    TurnError, TurnObservation, TurnService,
};
use rsi_ai_protocol::{
    AiError, ImageCall, ImageRequest, ImageToolResultCapability, LanguageCall, LanguageProfile,
    LanguageRequest, ModelRef, PreparedImageCall, PreparedLanguageCall, ToolDialect,
};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest, ApprovalSubject};
use rsi_media_protocol::{Media, MediaError, MediaRef, StoredMedia};
use rsi_sandbox::SandboxMode;
use rsi_session::LocalSessionService;
use rsi_session_protocol::{
    AgentSettingsSource, CreateSession, NoApprovalControl, SessionApprovalControl, SessionError,
    SessionHandle, SessionInput, SessionService, SubmitDirectImage, SubmitInput,
    validate_session_input,
};
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError, ToolResultIdentity,
    ToolRuntime,
};
use rsi_workspace_protocol::{
    WorkspaceCursor, WorkspaceId, WorkspacePage, WorkspaceRecord, WorkspaceRegistry,
    WorkspaceStatus,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

#[path = "attachment/drafts.rs"]
mod drafts;

#[path = "attachment/commands.rs"]
mod commands;

#[derive(Debug, Default)]
struct UnavailableTurns {
    tree: Option<Vec<SessionId>>,
    live_tree: Option<Arc<LiveTree>>,
}

#[async_trait]
impl TurnService for UnavailableTurns {
    async fn prepare_resume(&self, _session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        panic!("durable attachment must not prepare execution")
    }

    fn watch_tree_membership(
        &self,
        _root: &SessionId,
    ) -> TurnResult<rsi_agent_turn_protocol::TreeMembershipChanges> {
        Ok(self.live_tree.as_ref().map_or_else(
            || {
                Box::pin(futures_util::stream::pending())
                    as rsi_agent_turn_protocol::TreeMembershipChanges
            },
            |tree| {
                Box::pin(futures_util::stream::unfold(
                    tree.changes.subscribe(),
                    |mut receiver| async move {
                        receiver.changed().await.ok()?;
                        Some(((), receiver))
                    },
                ))
            },
        ))
    }
    async fn tree_sessions(&self, session_id: &SessionId) -> TurnResult<Vec<SessionId>> {
        if let Some(tree) = &self.live_tree {
            tree.reads.fetch_add(1, Ordering::SeqCst);
            return tree
                .members
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()));
        }
        Ok(self
            .tree
            .clone()
            .unwrap_or_else(|| vec![session_id.clone()]))
    }

    async fn submit(&self, _request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn submit_image(&self, _request: SubmitImage) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn cancel(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        panic!("not used")
    }

    async fn observe(
        &self,
        _session_id: &SessionId,
        _after_seq: u64,
    ) -> TurnResult<TurnObservation> {
        panic!("not used")
    }

    async fn outcome(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        panic!("not used")
    }

    async fn session_header(&self, _session_id: &SessionId) -> TurnResult<SessionHeader> {
        panic!("durable attachment must read the Store directly")
    }
}

#[derive(Debug)]
struct UnavailableComposition;

#[async_trait]
impl AgentComposition for UnavailableComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        panic!("not used")
    }

    async fn pin(
        &self,
        _preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        panic!("durable attachment must not pin a preset")
    }
}

#[derive(Debug)]
struct EmptyTools;

#[async_trait]
impl ToolRuntime for EmptyTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn prepare(
        &self,
        _invocation_id: &str,
        _call: ToolCall,
    ) -> Result<Box<dyn PreparedToolCall>, ToolError> {
        Err(ToolError::Sealed)
    }

    fn query(&self, _identity: &ToolResultIdentity) -> Result<RetainedToolResult, ToolError> {
        Err(ToolError::Sealed)
    }

    async fn wait(
        &self,
        _identity: &ToolResultIdentity,
        _cancellation: CancellationToken,
    ) -> Result<RetainedToolResult, ToolError> {
        Err(ToolError::Sealed)
    }

    fn commit(&self, _identity: &ToolResultIdentity) -> Result<(), ToolError> {
        Err(ToolError::Sealed)
    }
}

#[derive(Debug)]
struct AvailableComposition;

#[async_trait]
impl AgentComposition for AvailableComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("image-preset").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        AgentCompositionPin::new(
            preset_id.clone(),
            "a".repeat(64),
            Arc::new(EmptyTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            rsi_agent_composition_protocol::DomainCatalog::default(),
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(()),
        )
    }
}

#[derive(Debug)]
struct FailingComposition;

#[async_trait]
impl AgentComposition for FailingComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("missing-preset").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        Err(AgentCompositionError::Unavailable {
            preset_id: preset_id.clone(),
            reason: "test preset source is unavailable".into(),
        })
    }
}

#[derive(Debug)]
struct UnavailableWorkspace;

#[async_trait]
impl WorkspaceRegistry for UnavailableWorkspace {
    async fn get(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        panic!("durable attachment must not look up a workspace")
    }
    async fn list(
        &self,
        _after: Option<WorkspaceCursor>,
        _limit: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        panic!("not used")
    }

    async fn get_or_create(&self, _path: &Path) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        panic!("durable attachment must not register a workspace")
    }

    async fn status(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        panic!("not used")
    }

    async fn delete_registration(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        panic!("not used")
    }
}

#[derive(Debug, Default)]
struct RejectingWorkspace {
    registrations: AtomicUsize,
    gate: Option<Arc<Semaphore>>,
}

#[async_trait]
impl WorkspaceRegistry for RejectingWorkspace {
    async fn get(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        AvailableWorkspace::at(&std::env::current_dir().unwrap())
            .get(id)
            .await
    }
    async fn list(
        &self,
        _after: Option<WorkspaceCursor>,
        _limit: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        Ok(WorkspacePage {
            records: Vec::new(),
            next: None,
        })
    }

    async fn get_or_create(&self, _path: &Path) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        self.registrations.fetch_add(1, Ordering::AcqRel);
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        Err(rsi_workspace_protocol::WorkspaceError::Storage(
            "unexpected workspace mutation".into(),
        ))
    }

    async fn status(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        panic!("not used")
    }

    async fn delete_registration(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct UnavailableLanguage;

#[async_trait]
impl LanguageCall for UnavailableLanguage {
    fn describe(&self, _model: &ModelRef) -> Result<LanguageProfile, AiError> {
        panic!("durable attachment must not resolve a Language route")
    }

    async fn prepare(
        &self,
        _model: ModelRef,
        _request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct AvailableLanguage;

#[async_trait]
impl LanguageCall for AvailableLanguage {
    fn describe(&self, _model: &ModelRef) -> Result<LanguageProfile, AiError> {
        Ok(LanguageProfile::new(
            128_000,
            4_096,
            16_384,
            ToolDialect::Responses,
            false,
            ImageToolResultCapability::No,
            Vec::new(),
        )
        .unwrap())
    }

    async fn prepare(
        &self,
        _model: ModelRef,
        _request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct CompetingPublicationTurns {
    store: Arc<MemoryStore>,
    resume_issuer: ResumeAdmissionIssuer,
    composition: AgentCompositionPin,
    submissions: AtomicUsize,
    change_created_at: bool,
}

#[derive(Debug)]
struct CompetingImagePublicationTurns {
    store: Arc<MemoryStore>,
    competing_header: SessionHeader,
    submissions: AtomicUsize,
}

#[derive(Debug)]
struct ConcurrentResumeTurns {
    header: SessionHeader,
    resume_issuer: ResumeAdmissionIssuer,
    composition: AgentCompositionPin,
    entered: AtomicUsize,
    entered_notify: Notify,
    release: Semaphore,
}

#[async_trait]
impl TurnService for ConcurrentResumeTurns {
    async fn prepare_resume(&self, session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        assert_eq!(session_id, self.header.session_id());
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.entered_notify.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("test release semaphore remains open")
            .forget();
        self.resume_issuer
            .issue(self.header.clone(), self.composition.clone())
    }

    async fn submit_message(&self, request: SubmitMessage) -> TurnResult<MessageReceipt> {
        assert!(matches!(&request.session, SubmitSession::Resume(_)));
        Ok(MessageReceipt {
            session_id: request.session.session_id().clone(),
            message_id: request.message.message_id,
            accepted_control_seq: 1,
            observed_fact_seq: 1,
            state: MessageState::Pending,
        })
    }

    async fn submit(&self, _request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn submit_image(&self, _request: SubmitImage) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn cancel(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        panic!("not used")
    }

    async fn observe(
        &self,
        _session_id: &SessionId,
        _after_seq: u64,
    ) -> TurnResult<TurnObservation> {
        panic!("not used")
    }

    async fn outcome(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        panic!("not used")
    }

    async fn session_header(&self, _session_id: &SessionId) -> TurnResult<SessionHeader> {
        panic!("not used")
    }
}

#[async_trait]
impl TurnService for CompetingPublicationTurns {
    async fn prepare_resume(&self, session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        let header = self
            .store
            .header(session_id)
            .await
            .map_err(|error| rsi_agent_turn_protocol::TurnError::Store(error.to_string()))?;
        self.resume_issuer.issue(header, self.composition.clone())
    }

    async fn submit_message(&self, request: SubmitMessage) -> TurnResult<MessageReceipt> {
        let attempt = self.submissions.fetch_add(1, Ordering::AcqRel);
        if attempt == 0 {
            tokio::task::yield_now().await;
            assert!(matches!(&request.session, SubmitSession::Fresh(_)));
            let original = request.session.header();
            let header = if self.change_created_at {
                SessionHeader::new(
                    original.session_id().clone(),
                    original.created_at_ms() + 1,
                    original.canonical_cwd(),
                    original.agent_preset_id().clone(),
                    original.settings().clone(),
                )
                .unwrap()
                .with_workspace_trust(original.workspace_trust())
                .unwrap()
            } else {
                original.clone()
            };
            let turn_id = TurnId::new("turn-competing-publication").unwrap();
            self.store
                .append(AppendBatch {
                    session_id: header.session_id().clone(),
                    expected_seq: 0,
                    header: Some(header),
                    facts: (vec![
                        SessionFact::new(
                            1,
                            1,
                            SessionFactBody::TurnAccepted {
                                turn_id,
                                text: "published by the competing handle".into(),
                                model: None,
                                sandbox: SandboxMode::WorkspaceWrite,
                                require_approval: false,
                            },
                        )
                        .unwrap(),
                    ])
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                })
                .await
                .map_err(|error| rsi_agent_turn_protocol::TurnError::Store(error.to_string()))?;
            return Err(rsi_agent_turn_protocol::TurnError::Invalid(
                "competing handle published first".into(),
            ));
        }
        assert_eq!(
            matches!(&request.session, SubmitSession::Fresh(_)),
            self.change_created_at
        );
        Ok(MessageReceipt {
            session_id: request.session.session_id().clone(),
            message_id: request.message.message_id,
            accepted_control_seq: 1,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        })
    }

    async fn submit(&self, _request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn submit_image(&self, _request: SubmitImage) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn cancel(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        panic!("not used")
    }

    async fn observe(
        &self,
        _session_id: &SessionId,
        _after_seq: u64,
    ) -> TurnResult<TurnObservation> {
        panic!("not used")
    }

    async fn outcome(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        panic!("not used")
    }

    async fn session_header(&self, _session_id: &SessionId) -> TurnResult<SessionHeader> {
        panic!("not used")
    }
}

#[async_trait]
impl TurnService for CompetingImagePublicationTurns {
    async fn prepare_resume(&self, _session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        panic!("a conflicting durable Header must not attach the fresh Image handle")
    }

    async fn submit(&self, _request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn submit_image(&self, request: SubmitImage) -> TurnResult<SubmittedTurn> {
        let attempt = self.submissions.fetch_add(1, Ordering::AcqRel);
        assert!(matches!(&request.session, SubmitSession::Fresh(_)));
        if attempt == 0 {
            tokio::task::yield_now().await;
            self.store
                .append(AppendBatch {
                    session_id: self.competing_header.session_id().clone(),
                    expected_seq: 0,
                    header: Some(self.competing_header.clone()),
                    facts: (vec![
                        SessionFact::new(
                            1,
                            1,
                            SessionFactBody::ImageRequested {
                                turn_id: request.turn_id.clone(),
                                model: request.model,
                                request: request.request,
                            },
                        )
                        .unwrap(),
                    ])
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                })
                .await
                .unwrap();
            return Err(TurnError::SubmissionConflict {
                session: request.session.session_id().to_string(),
                turn: request.turn_id.to_string(),
            });
        }
        Ok(SubmittedTurn {
            session_id: request.session.session_id().clone(),
            turn_id: request.turn_id,
            accepted_seq: 1,
        })
    }

    async fn cancel(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        panic!("not used")
    }

    async fn observe(
        &self,
        _session_id: &SessionId,
        _after_seq: u64,
    ) -> TurnResult<TurnObservation> {
        panic!("not used")
    }

    async fn outcome(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        panic!("not used")
    }

    async fn session_header(&self, _session_id: &SessionId) -> TurnResult<SessionHeader> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct AvailableWorkspace {
    path: std::path::PathBuf,
    available: std::sync::atomic::AtomicBool,
    reads: AtomicUsize,
}

impl AvailableWorkspace {
    fn at(path: &Path) -> Self {
        Self {
            path: path.canonicalize().unwrap(),
            available: std::sync::atomic::AtomicBool::new(true),
            reads: AtomicUsize::new(0),
        }
    }
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse("a".repeat(64)).unwrap()
}

#[async_trait]
impl WorkspaceRegistry for AvailableWorkspace {
    async fn get(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if !self.available.load(Ordering::SeqCst) || id != &workspace_id() {
            return Err(rsi_workspace_protocol::WorkspaceError::Unknown(id.clone()));
        }
        Ok(WorkspaceRecord {
            id: id.clone(),
            path: self.path.clone(),
        })
    }
    async fn list(
        &self,
        _after: Option<WorkspaceCursor>,
        _limit: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        Ok(WorkspacePage {
            records: Vec::new(),
            next: None,
        })
    }

    async fn get_or_create(&self, path: &Path) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        Ok(WorkspaceRecord {
            id: WorkspaceId::parse("a".repeat(64)).unwrap(),
            path: path.to_path_buf(),
        })
    }

    async fn status(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        Ok(WorkspaceStatus::Ok)
    }

    async fn delete_registration(&self, _id: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        Ok(false)
    }
}

#[derive(Debug)]
struct UnavailableImage;

#[async_trait]
impl ImageCall for UnavailableImage {
    fn describe(&self, _model: &ModelRef) -> Result<(), AiError> {
        panic!("not used")
    }

    async fn prepare(
        &self,
        _model: ModelRef,
        _request: ImageRequest,
    ) -> Result<Box<dyn PreparedImageCall>, AiError> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct UnavailableMedia;

#[async_trait]
impl Media for UnavailableMedia {
    async fn import_image(&self, _source: bytes::Bytes) -> rsi_media_protocol::Result<MediaRef> {
        panic!("not used")
    }

    async fn read(&self, reference: &MediaRef) -> rsi_media_protocol::Result<StoredMedia> {
        Err(MediaError::NotFound(reference.id.clone()))
    }
}

#[derive(Debug)]
struct AvailableImage;

#[async_trait]
impl ImageCall for AvailableImage {
    fn describe(&self, _model: &ModelRef) -> Result<(), AiError> {
        Ok(())
    }

    async fn prepare(
        &self,
        _model: ModelRef,
        _request: ImageRequest,
    ) -> Result<Box<dyn PreparedImageCall>, AiError> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct UnavailableSettings;

impl AgentSettingsSource for UnavailableSettings {
    fn current(&self) -> rsi_session_protocol::Result<FrozenAgentSettings> {
        panic!("durable attachment must not read current settings")
    }
}

#[derive(Debug, Default)]
struct TreeApprovals {
    changes: Option<tokio::sync::watch::Sender<u64>>,
    reads: AtomicUsize,
    pending: Mutex<BTreeMap<SessionId, Vec<ApprovalRequest>>>,
    answered: Mutex<Vec<(SessionId, String, ApprovalDecision)>>,
}

#[async_trait]
impl SessionApprovalControl for TreeApprovals {
    async fn pending_for_sessions(
        &self,
        sessions: &[SessionId],
    ) -> rsi_session_protocol::Result<Vec<ApprovalRequest>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .pending
            .lock()
            .await
            .iter()
            .filter(|(id, _)| sessions.contains(id))
            .flat_map(|(_, requests)| requests.clone())
            .collect())
    }
    fn watch_pending(
        &self,
        _sessions: &[SessionId],
    ) -> rsi_session_protocol::Result<rsi_user_questions_protocol::PendingChanges> {
        Ok(self.changes.as_ref().map_or_else(
            || {
                Box::pin(futures_util::stream::pending())
                    as rsi_user_questions_protocol::PendingChanges
            },
            |changes| {
                Box::pin(futures_util::stream::unfold(
                    changes.subscribe(),
                    |mut receiver| async move {
                        receiver.changed().await.ok()?;
                        Some(((), receiver))
                    },
                ))
            },
        ))
    }
    async fn pending(
        &self,
        session_id: &SessionId,
    ) -> rsi_session_protocol::Result<Vec<ApprovalRequest>> {
        Ok(self
            .pending
            .lock()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn answer(
        &self,
        session_id: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> rsi_session_protocol::Result<bool> {
        let mut pending = self.pending.lock().await;
        let Some(requests) = pending.get_mut(session_id) else {
            return Ok(false);
        };
        let Some(index) = requests
            .iter()
            .position(|request| request.id == approval_id)
        else {
            return Ok(false);
        };
        requests.remove(index);
        drop(pending);
        self.answered
            .lock()
            .await
            .push((session_id.clone(), approval_id.into(), decision));
        Ok(true)
    }
}

#[derive(Debug)]
struct ImageSettings;

impl AgentSettingsSource for ImageSettings {
    fn current(&self) -> rsi_session_protocol::Result<FrozenAgentSettings> {
        Ok(FrozenAgentSettings::new(
            "settings",
            "system",
            ModelRef::new("removed-provider", "removed-model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap())
    }
}

#[derive(Debug)]
struct TextSettings;

impl AgentSettingsSource for TextSettings {
    fn current(&self) -> rsi_session_protocol::Result<FrozenAgentSettings> {
        Ok(test_settings())
    }
}

fn test_settings() -> FrozenAgentSettings {
    FrozenAgentSettings::new(
        "settings",
        "system",
        ModelRef::new("fixture", "fixture-model").unwrap(),
        SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap()
}

#[tokio::test]
async fn new_drafts_read_current_defaults_while_existing_headers_remain_frozen() {
    #[derive(Debug)]
    struct MutableSettings(std::sync::Mutex<rsi_session_protocol::Result<FrozenAgentSettings>>);
    impl AgentSettingsSource for MutableSettings {
        fn current(&self) -> rsi_session_protocol::Result<FrozenAgentSettings> {
            self.0.lock().unwrap().clone()
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let initial = test_settings();
    let defaults = Arc::new(MutableSettings(std::sync::Mutex::new(Ok(initial.clone()))));
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        Arc::new(MemoryStore::new()),
        Arc::new(AvailableComposition),
        Arc::new(AvailableWorkspace::at(directory.path())),
        defaults.clone(),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let request = |id| CreateSession {
        workspace_id: workspace_id(),
        session_id: SessionId::new(id).unwrap(),
        agent_preset_id: Some(AgentPresetId::new("image-preset").unwrap()),
        workspace_trust: WorkspaceTrust::Untrusted,
    };
    let first = service.create(request("defaults-first")).await.unwrap();
    let frozen = first.header().await.unwrap();
    let replacement = FrozenAgentSettings::new(
        "replacement",
        "changed prompt",
        ModelRef::new("fixture", "changed-model").unwrap(),
        SandboxMode::ReadOnly,
        true,
    )
    .unwrap();
    *defaults.0.lock().unwrap() = Ok(replacement.clone());
    let second = service.create(request("defaults-second")).await.unwrap();
    assert_eq!(first.header().await.unwrap(), frozen);
    assert_eq!(second.header().await.unwrap().settings(), &replacement);
    *defaults.0.lock().unwrap() = Err(SessionError::Backend("defaults unavailable".into()));
    assert!(
        service
            .create(request("defaults-unavailable"))
            .await
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &first,
        &service.create(request("defaults-first")).await.unwrap()
    ));
    assert_eq!(first.header().await.unwrap(), frozen);
}

#[tokio::test]
async fn repeated_create_shares_one_live_draft_and_conflicts_on_changed_input() {
    let directory = tempfile::tempdir().unwrap();
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        Arc::new(MemoryStore::new()),
        Arc::new(AvailableComposition),
        Arc::new(AvailableWorkspace::at(directory.path())),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let request = CreateSession {
        workspace_id: workspace_id(),
        session_id: SessionId::new("same-draft").unwrap(),
        agent_preset_id: None,
        workspace_trust: WorkspaceTrust::Untrusted,
    };
    let first = service.create(request.clone()).await.unwrap();
    let second = service.create(request.clone()).await.unwrap();
    assert!(
        Arc::ptr_eq(&first, &second),
        "one registry shares one preparation and pin"
    );
    let mut conflict = request;
    conflict.workspace_trust = WorkspaceTrust::Trusted;
    assert!(matches!(
        service.create(conflict).await,
        Err(SessionError::DraftConflict { .. })
    ));
}

async fn submit_text(
    handle: Arc<dyn SessionHandle>,
    message_id: &'static str,
    text: &'static str,
) -> rsi_session_protocol::Result<MessageReceipt> {
    handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new(message_id).unwrap(),
            content: vec![SessionInput::Text { text: text.into() }],
            model: None,
            sandbox: None,
        })
        .await
}

#[test]
fn session_input_validation_is_complete_before_any_backend_operation() {
    assert!(validate_session_input(&[]).is_err());
    assert!(
        validate_session_input(&vec![
            SessionInput::Text { text: "x".into() };
            MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS + 1
        ])
        .is_err()
    );
    assert!(
        validate_session_input(&[
            SessionInput::Text {
                text: "x".repeat(MAXIMUM_TURN_TEXT_BYTES / 2 + 1),
            },
            SessionInput::Text {
                text: "y".repeat(MAXIMUM_TURN_TEXT_BYTES / 2 + 1),
            },
        ])
        .is_err()
    );
    for text in [
        String::new(),
        "contains\0nul".into(),
        "contains\u{7f}delete".into(),
    ] {
        assert!(validate_session_input(&[SessionInput::Text { text }]).is_err());
    }
    assert!(
        validate_session_input(&[SessionInput::Image {
            media: MediaRef {
                id: rsi_media_protocol::MediaId::new("a".repeat(64)).unwrap(),
                mime: "image/png".into(),
                bytes: 0,
                width: 1,
                height: 1
            },
        }])
        .is_err()
    );
    validate_session_input(&[
        SessionInput::Text {
            text: "inspect".into(),
        },
        SessionInput::Image {
            media: MediaRef {
                id: rsi_media_protocol::MediaId::new("a".repeat(64)).unwrap(),
                mime: "image/png".into(),
                bytes: 3,
                width: 1,
                height: 1,
            },
        },
    ])
    .unwrap();
}

#[derive(Debug)]
struct ImageTurns;

#[async_trait]
impl TurnService for ImageTurns {
    async fn prepare_resume(&self, _session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        panic!("not used")
    }

    async fn submit(&self, _request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn submit_image(&self, request: SubmitImage) -> TurnResult<SubmittedTurn> {
        Ok(SubmittedTurn {
            session_id: request.session.session_id().clone(),
            turn_id: request.turn_id,
            accepted_seq: 1,
        })
    }

    async fn cancel(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        panic!("not used")
    }

    async fn observe(
        &self,
        _session_id: &SessionId,
        _after_seq: u64,
    ) -> TurnResult<TurnObservation> {
        panic!("not used")
    }

    async fn outcome(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        panic!("not used")
    }

    async fn session_header(&self, _session_id: &SessionId) -> TurnResult<SessionHeader> {
        panic!("not used")
    }
}

#[derive(Debug)]
struct RejectingResumeTurns;

#[async_trait]
impl TurnService for RejectingResumeTurns {
    async fn prepare_resume(&self, _session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        Err(rsi_agent_turn_protocol::TurnError::Composition(
            "test preset source is unavailable".into(),
        ))
    }

    async fn submit(&self, _request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        panic!("resume preparation must fail before submit")
    }

    async fn submit_image(&self, _request: SubmitImage) -> TurnResult<SubmittedTurn> {
        panic!("not used")
    }

    async fn cancel(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
        _reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        panic!("not used")
    }

    async fn observe(
        &self,
        _session_id: &SessionId,
        _after_seq: u64,
    ) -> TurnResult<TurnObservation> {
        panic!("not used")
    }

    async fn outcome(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> TurnResult<Option<rsi_agent_session_protocol::TurnOutcome>> {
        panic!("not used")
    }

    async fn session_header(&self, _session_id: &SessionId) -> TurnResult<SessionHeader> {
        panic!("not used")
    }
}

#[tokio::test]
async fn matching_competing_publication_attaches_a_fresh_handle_for_its_next_submit() {
    assert_competing_message_publication(false, false).await;
}

#[tokio::test]
async fn a_different_creation_time_does_not_attach_a_fresh_message_handle() {
    assert_competing_message_publication(true, false).await;
}

#[tokio::test]
async fn concurrent_submissions_reject_a_draft_expired_by_the_first_publication() {
    assert_competing_message_publication(true, true).await;
}

async fn assert_competing_message_publication(change_created_at: bool, concurrent: bool) {
    let store = Arc::new(MemoryStore::new());
    let preset_id = AgentPresetId::new("image-preset").unwrap();
    let composition = AvailableComposition.pin(&preset_id).await.unwrap();
    let turns = Arc::new(CompetingPublicationTurns {
        store: store.clone(),
        resume_issuer: ResumeAdmissionIssuer::new(),
        composition,
        submissions: AtomicUsize::new(0),
        change_created_at,
    });
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        turns.clone(),
        store,
        Arc::new(AvailableComposition),
        Arc::new(AvailableWorkspace::at(&std::env::current_dir().unwrap())),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let handle = application
        .create(CreateSession {
            workspace_id: workspace_id(),
            session_id: SessionId::new("session-competing-publication").unwrap(),
            agent_preset_id: Some(preset_id),
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    if concurrent {
        let (first, second) = tokio::join!(
            submit_text(handle.clone(), "first", "first"),
            submit_text(handle.clone(), "second", "second"),
        );
        assert!(first.is_err());
        assert!(matches!(second, Err(SessionError::NotFound(_))));
        assert_eq!(turns.submissions.load(Ordering::Acquire), 1);
        return;
    }
    assert!(
        handle
            .submit(SubmitInput {
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                message_id: MessageId::new("message-lost-publication-race").unwrap(),
                content: vec![SessionInput::Text {
                    text: "first attempt".into(),
                }],
                model: None,
                sandbox: None,
            })
            .await
            .is_err()
    );
    if change_created_at {
        assert!(matches!(
            submit_text(handle, "must-not-resume-other-header", "retry").await,
            Err(SessionError::NotFound(_))
        ));
        assert_eq!(turns.submissions.load(Ordering::Acquire), 1);
        return;
    }
    let receipt = handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-after-publication-race").unwrap(),
            content: vec![SessionInput::Text {
                text: "retry through the attached path".into(),
            }],
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(
        receipt.message_id.as_str(),
        "message-after-publication-race"
    );
    assert_eq!(turns.submissions.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn conflicting_image_publication_keeps_the_fresh_handle_detached() {
    assert_competing_image_publication(false).await;
}

#[tokio::test]
async fn concurrent_images_reject_a_draft_expired_by_the_first_publication() {
    assert_competing_image_publication(true).await;
}

async fn assert_competing_image_publication(concurrent: bool) {
    let store = Arc::new(MemoryStore::new());
    let session_id = SessionId::new("session-conflicting-image-publication").unwrap();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let turns = Arc::new(CompetingImagePublicationTurns {
        store: store.clone(),
        competing_header: SessionHeader::new(
            session_id.clone(),
            1,
            cwd.to_str().unwrap(),
            AgentPresetId::new("different-preset").unwrap(),
            test_settings(),
        )
        .unwrap(),
        submissions: AtomicUsize::new(0),
    });
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        turns.clone(),
        store,
        Arc::new(AvailableComposition),
        Arc::new(AvailableWorkspace::at(&std::env::current_dir().unwrap())),
        Arc::new(ImageSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(AvailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let handle = application
        .create(CreateSession {
            workspace_id: workspace_id(),
            session_id,
            agent_preset_id: Some(AgentPresetId::new("image-preset").unwrap()),
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    if concurrent {
        let input = |id| SubmitDirectImage {
            turn_id: TurnId::new(id).unwrap(),
            model: ModelRef::new("image-provider", "image-model").unwrap(),
            request: ImageRequest::new("concurrent attempt", 1).unwrap(),
        };
        let (first, second) = tokio::join!(
            handle.generate_image(input("first")),
            handle.generate_image(input("second")),
        );
        assert!(first.is_err());
        assert!(matches!(second, Err(SessionError::NotFound(_))));
        assert_eq!(turns.submissions.load(Ordering::Acquire), 1);
        return;
    }
    assert!(
        handle
            .generate_image(SubmitDirectImage {
                turn_id: TurnId::new("turn-conflicting-image-publication").unwrap(),
                model: ModelRef::new("image-provider", "image-model").unwrap(),
                request: ImageRequest::new("first attempt", 1).unwrap(),
            })
            .await
            .is_err()
    );
    let retried = handle
        .generate_image(SubmitDirectImage {
            turn_id: TurnId::new("turn-image-after-conflict").unwrap(),
            model: ModelRef::new("image-provider", "image-model").unwrap(),
            request: ImageRequest::new("second attempt", 1).unwrap(),
        })
        .await;
    assert!(matches!(retried, Err(SessionError::NotFound(_))));
    assert_eq!(turns.submissions.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn attached_handle_does_not_serialize_independent_resume_preparation() {
    let store = Arc::new(MemoryStore::new());
    let session_id = SessionId::new("session-concurrent-resume").unwrap();
    let header = SessionHeader::new(
        session_id.clone(),
        1,
        std::env::current_dir()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap(),
        AgentPresetId::new("image-preset").unwrap(),
        test_settings(),
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: (vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("turn-existing-concurrent-resume").unwrap(),
                        text: "existing".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
        })
        .await
        .unwrap();
    let composition = AvailableComposition
        .pin(header.agent_preset_id())
        .await
        .unwrap();
    let turns = Arc::new(ConcurrentResumeTurns {
        header,
        resume_issuer: ResumeAdmissionIssuer::new(),
        composition,
        entered: AtomicUsize::new(0),
        entered_notify: Notify::new(),
        release: Semaphore::new(0),
    });
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        turns.clone(),
        store,
        Arc::new(UnavailableComposition),
        Arc::new(AvailableWorkspace::at(&std::env::current_dir().unwrap())),
        Arc::new(UnavailableSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let handle = application.attach(&session_id).await.unwrap();
    let first = tokio::spawn(submit_text(
        Arc::clone(&handle),
        "message-concurrent-resume-1",
        "first",
    ));
    let second = tokio::spawn(submit_text(
        Arc::clone(&handle),
        "message-concurrent-resume-2",
        "second",
    ));

    let overlapped = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if turns.entered.load(Ordering::Acquire) == 2 {
                break;
            }
            turns.entered_notify.notified().await;
        }
    })
    .await;
    turns.release.add_permits(2);
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert!(
        overlapped.is_ok(),
        "attached submissions serialized prepare_resume behind the handle state lock"
    );
}

#[tokio::test]
async fn fresh_preset_failure_precedes_workspace_registration() {
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let workspace = Arc::new(RejectingWorkspace::default());
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        store,
        Arc::new(FailingComposition),
        workspace.clone(),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );

    let result = application
        .create(CreateSession {
            workspace_id: workspace_id(),
            session_id: SessionId::new("session-failing-fresh-preset").unwrap(),
            agent_preset_id: Some(AgentPresetId::new("missing-preset").unwrap()),
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await;

    assert!(matches!(result, Err(SessionError::Backend(_))));
    assert_eq!(workspace.registrations.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn cold_resume_preset_failure_precedes_workspace_registration() {
    let store = Arc::new(MemoryStore::new());
    let session_id = SessionId::new("session-failing-resume-preset").unwrap();
    let turn_id = TurnId::new("turn-existing-resume-preset").unwrap();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let header = SessionHeader::new(
        session_id.clone(),
        1,
        cwd.to_str().unwrap(),
        AgentPresetId::new("missing-preset").unwrap(),
        test_settings(),
    )
    .unwrap();
    rsi_agent_testkit::append_history_fixture(
        store.as_ref(),
        AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: (vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn_id.clone(),
                        text: "existing".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::TurnTerminal {
                        turn_id,
                        outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
        },
    )
    .await
    .unwrap();
    let store: Arc<dyn SessionStore> = store;
    let workspace = Arc::new(RejectingWorkspace::default());
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(RejectingResumeTurns),
        store,
        Arc::new(UnavailableComposition),
        workspace.clone(),
        Arc::new(UnavailableSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let handle = application.attach(&session_id).await.unwrap();

    let result = handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-must-not-start").unwrap(),
            content: vec![SessionInput::Text {
                text: "must not start".into(),
            }],
            model: None,
            sandbox: None,
        })
        .await;

    assert!(matches!(result, Err(SessionError::Backend(_))));
    assert_eq!(workspace.registrations.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn attach_and_history_need_only_the_durable_store() {
    let store = Arc::new(MemoryStore::new());
    let session_id = SessionId::new("session-store-only-attach").unwrap();
    let turn_id = TurnId::new("turn-existing").unwrap();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let header = SessionHeader::new(
        session_id.clone(),
        1,
        cwd.to_str().unwrap(),
        AgentPresetId::new("removed-preset").unwrap(),
        FrozenAgentSettings::new(
            "settings",
            "system",
            ModelRef::new("removed-provider", "removed-model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let accepted = SessionFact::new(
        1,
        1,
        SessionFactBody::TurnAccepted {
            turn_id,
            text: "durable".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: (vec![accepted.clone()])
                .into_iter()
                .map(Into::into)
                .collect(),
        })
        .await
        .unwrap();
    let store_service: Arc<dyn SessionStore> = store;
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        store_service,
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );

    let read = rsi_session_protocol::SessionReads::acquire(
        &application,
        &rsi_session_protocol::SessionTarget {
            session_id: session_id.clone(),
            header_key: header.fingerprint().unwrap(),
        },
    )
    .await
    .unwrap();
    assert_eq!(read.header(), &header);
    drop(read);
    let handle = application.attach(&session_id).await.unwrap();
    assert_eq!(handle.header().await.unwrap(), header);
    assert_eq!(
        handle.history_before(None, 8).await.unwrap().facts,
        [accepted]
    );
    assert!(handle.pending_approvals().await.unwrap().is_empty());
    assert!(
        !handle
            .answer_approval(&session_id, "missing", ApprovalDecision::Deny)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One tree fixture proves duplicate tuples and rejection before dispatch.
async fn root_session_lists_and_answers_a_descendant_approval_by_exact_subject() {
    let store = Arc::new(MemoryStore::new());
    let root = SessionId::new("session-approval-root").unwrap();
    let child = SessionId::new("session-approval-child").unwrap();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let header = SessionHeader::new(
        root.clone(),
        1,
        cwd.to_str().unwrap(),
        AgentPresetId::new("removed-preset").unwrap(),
        test_settings(),
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: root.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: (vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("turn-approval-root").unwrap(),
                        text: "existing".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
        })
        .await
        .unwrap();
    let request = ApprovalRequest {
        review: None,
        subject: ApprovalSubject::new(child.as_str(), "turn-child", "effect-child").unwrap(),
        id: "approval-child".into(),
        action: "write child file".into(),
        reason: "child mutation".into(),
    };
    let approvals = Arc::new(TreeApprovals::default());
    approvals
        .pending
        .lock()
        .await
        .insert(child.clone(), vec![request.clone()]);
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns {
            tree: Some(vec![root.clone(), child.clone()]),
            ..Default::default()
        }),
        store,
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        approvals.clone(),
    );
    let handle = application.attach(&root).await.unwrap();

    assert_eq!(
        handle.pending_approvals().await.unwrap().as_slice(),
        std::slice::from_ref(&request)
    );
    let mut same_id_at_root = request;
    same_id_at_root.subject =
        ApprovalSubject::new(root.as_str(), "turn-root", "effect-root").unwrap();
    approvals
        .pending
        .lock()
        .await
        .insert(root.clone(), vec![same_id_at_root]);
    assert!(
        matches!(handle.answer_approval(&SessionId::new("unrelated").unwrap(), "approval-child", ApprovalDecision::Deny).await,
        Err(SessionError::Invalid(message)) if message.contains("outside"))
    );
    assert!(approvals.answered.lock().await.is_empty());
    assert!(
        handle
            .answer_approval(&child, "approval-child", ApprovalDecision::AllowOnce)
            .await
            .unwrap()
    );
    assert_eq!(
        approvals.answered.lock().await.as_slice(),
        &[(
            child.clone(),
            "approval-child".into(),
            ApprovalDecision::AllowOnce,
        )]
    );
    assert_eq!(handle.pending_approvals().await.unwrap().len(), 1);
    assert!(
        handle
            .answer_approval(&root, "approval-child", ApprovalDecision::Deny)
            .await
            .unwrap()
    );
    assert!(handle.pending_approvals().await.unwrap().is_empty());
    assert!(
        !handle
            .answer_approval(&child, "approval-child", ApprovalDecision::Deny)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn image_only_draft_defers_language_and_workspace_until_the_selected_operation_needs_them() {
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let workspace = Arc::new(RejectingWorkspace::default());
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(ImageTurns),
        store,
        Arc::new(AvailableComposition),
        workspace.clone(),
        Arc::new(ImageSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(AvailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let handle = application
        .create(CreateSession {
            workspace_id: workspace_id(),
            session_id: SessionId::new("session-image-only").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .expect("draft creation must not resolve a Language route or mutate Workspace");
    assert_eq!(workspace.registrations.load(Ordering::Acquire), 0);

    let receipt = handle
        .generate_image(SubmitDirectImage {
            turn_id: TurnId::new("turn-image-only").unwrap(),
            model: ModelRef::new("image-provider", "image-model").unwrap(),
            request: ImageRequest::new("draw a square", 1).unwrap(),
        })
        .await
        .expect("direct Image submission must not require a Language route");
    assert_eq!(receipt.accepted_seq, 1);
    assert_eq!(workspace.registrations.load(Ordering::Acquire), 0);
}

#[derive(Debug)]
struct FailedQuestions(rsi_user_questions_protocol::QuestionError);

#[async_trait]
impl rsi_user_questions_protocol::UserQuestions for FailedQuestions {
    async fn pending_for_sessions(
        &self,
        _sessions: &[String],
    ) -> rsi_user_questions_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>>
    {
        self.pending("session").await
    }
    fn watch_pending(
        &self,
        _sessions: &[String],
    ) -> rsi_user_questions_protocol::Result<rsi_user_questions_protocol::PendingChanges> {
        Err(self.0.clone())
    }

    async fn ask(
        &self,
        _: rsi_user_questions_protocol::QuestionRequest,
        _: CancellationToken,
    ) -> rsi_user_questions_protocol::Result<rsi_user_questions_protocol::QuestionAnswer> {
        Err(self.0.clone())
    }
    async fn pending(
        &self,
        _: &str,
    ) -> rsi_user_questions_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>>
    {
        Err(self.0.clone())
    }
    async fn answer(
        &self,
        _: &str,
        _: &str,
        _: rsi_user_questions_protocol::QuestionAnswer,
    ) -> rsi_user_questions_protocol::Result<bool> {
        Err(self.0.clone())
    }
}

#[tokio::test]
async fn question_operations_preserve_shutdown_and_capacity_errors() {
    use rsi_user_questions_protocol::{QuestionAnswer, QuestionError};
    for (error, expected) in [
        (QuestionError::Cancelled, SessionError::ShuttingDown),
        (QuestionError::Capacity, SessionError::Capacity),
    ] {
        let application = LocalSessionService::new(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            Arc::new(UnavailableCommands),
            Arc::new(UnavailableProjections),
            Arc::new(ImageTurns),
            Arc::new(MemoryStore::new()),
            Arc::new(AvailableComposition),
            Arc::new(AvailableWorkspace::at(&std::env::current_dir().unwrap())),
            Arc::new(ImageSettings),
            Arc::new(UnavailableLanguage),
            Arc::new(AvailableImage),
            Arc::new(UnavailableMedia),
            Arc::new(NoApprovalControl),
        )
        .with_questions(Some(Arc::new(FailedQuestions(error))));
        let handle = application
            .create(CreateSession {
                workspace_id: workspace_id(),
                session_id: SessionId::new("question-errors").unwrap(),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .unwrap();
        assert_eq!(handle.pending_questions().await, Err(expected.clone()));
        assert_eq!(
            handle
                .answer_question(
                    "q",
                    QuestionAnswer {
                        answers: vec!["answer".into()]
                    }
                )
                .await,
            Err(expected)
        );
    }
}

#[derive(Debug)]
struct LiveTree {
    members: std::sync::Mutex<Option<Vec<SessionId>>>,
    changes: tokio::sync::watch::Sender<u64>,
    reads: AtomicUsize,
}
impl LiveTree {
    fn new() -> Self {
        Self {
            members: std::sync::Mutex::new(None),
            changes: tokio::sync::watch::channel(0).0,
            reads: AtomicUsize::new(0),
        }
    }
    fn publish(&self, members: Vec<SessionId>) {
        *self.members.lock().unwrap() = Some(members);
        self.changes.send_modify(|revision| *revision += 1);
    }
}
#[derive(Debug)]
struct PinTracker(Arc<AtomicUsize>);
#[derive(Debug)]
struct TrackedLease(Arc<AtomicUsize>);
impl Drop for TrackedLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl AgentComposition for PinTracker {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("tracked").unwrap())
    }
    async fn pin(
        &self,
        preset: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        AgentCompositionPin::new(
            preset.clone(),
            "a".repeat(64),
            Arc::new(EmptyTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            rsi_agent_composition_protocol::DomainCatalog::default(),
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(TrackedLease(self.0.clone())),
        )
    }
}

#[tokio::test(start_paused = true)]
async fn fresh_interactions_release_composition_pin_and_follow_tree_publication_without_polling() {
    use futures_util::StreamExt as _;
    let tree = Arc::new(LiveTree::new());
    let leases = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(TreeApprovals {
        changes: Some(tokio::sync::watch::channel(0).0),
        ..Default::default()
    });
    let application = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns {
            live_tree: Some(tree.clone()),
            ..Default::default()
        }),
        Arc::new(MemoryStore::new()),
        Arc::new(PinTracker(leases.clone())),
        Arc::new(AvailableWorkspace::at(&std::env::current_dir().unwrap())),
        Arc::new(TextSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        approvals.clone(),
    );
    let root = SessionId::new("interaction-root").unwrap();
    let child = SessionId::new("interaction-child").unwrap();
    let handle = application
        .create(CreateSession {
            workspace_id: workspace_id(),
            session_id: root.clone(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    assert_eq!(leases.load(Ordering::SeqCst), 1);
    let mut stream = handle.observe_interactions().await.unwrap();
    drop(handle);
    assert_eq!(
        leases.load(Ordering::SeqCst),
        1,
        "the service retains its draft lease"
    );
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        leases.load(Ordering::SeqCst),
        0,
        "live stream retained the Fresh composition pin"
    );
    assert!(stream.next().await.unwrap().unwrap().approvals().is_empty());
    assert_eq!(tree.reads.load(Ordering::SeqCst), 1);
    assert_eq!(approvals.reads.load(Ordering::SeqCst), 0);
    {
        let mut next = Box::pin(stream.next());
        assert!(futures_util::poll!(&mut next).is_pending());
        tokio::time::advance(std::time::Duration::from_mins(1)).await;
        assert!(futures_util::poll!(&mut next).is_pending());
    }
    assert_eq!(tree.reads.load(Ordering::SeqCst), 1);
    tree.publish(vec![root.clone()]);
    assert!(stream.next().await.unwrap().unwrap().approvals().is_empty());
    let request = ApprovalRequest {
        review: None,
        id: "child-request".into(),
        subject: ApprovalSubject::new(child.as_str(), "turn", "effect").unwrap(),
        action: "write".into(),
        reason: "test".into(),
    };
    approvals
        .pending
        .lock()
        .await
        .insert(child.clone(), vec![request.clone()]);
    tree.publish(vec![root, child]);
    assert_eq!(
        stream.next().await.unwrap().unwrap().approvals(),
        &[request]
    );
    let tree_reads = tree.reads.load(Ordering::SeqCst);
    approvals.pending.lock().await.clear();
    approvals
        .changes
        .as_ref()
        .unwrap()
        .send_modify(|revision| *revision += 1);
    assert!(stream.next().await.unwrap().unwrap().approvals().is_empty());
    assert_eq!(
        tree.reads.load(Ordering::SeqCst),
        tree_reads,
        "broker-only change reread tree metadata"
    );
}

#[tokio::test]
async fn registered_workspace_is_resolved_once_and_live_retries_ignore_later_removal() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Arc::new(AvailableWorkspace::at(directory.path()));
    let canonical = workspace.path.clone();
    directory.close().unwrap();
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        Arc::new(UnavailableProjections),
        Arc::new(UnavailableTurns::default()),
        Arc::new(MemoryStore::new()),
        Arc::new(AvailableComposition),
        workspace.clone(),
        Arc::new(TextSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(NoApprovalControl),
    );
    let request = CreateSession {
        workspace_id: workspace_id(),
        session_id: SessionId::new("registered-draft").unwrap(),
        agent_preset_id: None,
        workspace_trust: WorkspaceTrust::Untrusted,
    };
    let first = service.create(request.clone()).await.unwrap();
    assert_eq!(
        first.header().await.unwrap().canonical_cwd(),
        canonical.to_str().unwrap()
    );
    workspace.available.store(false, Ordering::SeqCst);
    assert!(Arc::ptr_eq(
        &first,
        &service.create(request.clone()).await.unwrap()
    ));
    let mut changed = request.clone();
    changed.workspace_id = WorkspaceId::parse("b".repeat(64)).unwrap();
    assert!(matches!(
        service.create(changed).await,
        Err(SessionError::DraftConflict { .. })
    ));
    assert_eq!(workspace.reads.load(Ordering::SeqCst), 1);
    let mut next = request;
    next.session_id = SessionId::new("new-unknown-workspace").unwrap();
    assert!(
        matches!(service.create(next).await, Err(SessionError::Invalid(message)) if message.contains("not registered"))
    );
    assert_eq!(workspace.reads.load(Ordering::SeqCst), 2);
    service.stop().await;
}

#[derive(Debug)]
struct UnavailableCommands;
#[derive(Debug)]
struct UnavailableProjections;
#[async_trait]
impl rsi_agent_turn_protocol::SessionProjections for UnavailableProjections {
    fn watch_projection_changes(
        &self,
        _: &SessionId,
    ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::SessionProjectionChanges> {
        panic!("unexpected projection subscription")
    }
    async fn projection_snapshot(
        &self,
        _: &SessionId,
    ) -> rsi_agent_turn_protocol::Result<rsi_agent_session_protocol::SessionProjectionSnapshot>
    {
        panic!("unexpected projection capture")
    }
}
#[async_trait]
impl rsi_agent_turn_protocol::SessionCommands for UnavailableCommands {
    async fn list(
        &self,
        _: rsi_agent_turn_protocol::PreparedResumeSession,
    ) -> rsi_agent_turn_protocol::Result<rsi_agent_session_protocol::SessionCommandsView> {
        panic!("unexpected command discovery")
    }
    async fn execute(
        &self,
        _: rsi_agent_turn_protocol::PreparedResumeSession,
        _: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::DomainMutationReceipt> {
        panic!("unexpected command execution")
    }
    async fn query(
        &self,
        _: &SessionId,
        _: &rsi_agent_session_protocol::DomainRequestId,
    ) -> rsi_agent_turn_protocol::Result<Option<rsi_agent_turn_protocol::DomainMutationReceipt>>
    {
        panic!("unexpected command query")
    }
}
