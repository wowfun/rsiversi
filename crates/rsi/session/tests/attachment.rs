use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionError, AgentCompositionPin,
};
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, SessionId,
    TurnId,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore};
use rsi_agent_testkit::MemoryStore;
use rsi_agent_turn_protocol::{
    CancelResult, PreparedResumeSession, Result as TurnResult, SubmitImage, SubmitTurn,
    SubmittedTurn, TurnObservation, TurnService,
};
use rsi_ai_protocol::{
    AiError, ImageCall, ImageRequest, ImageToolResultCapability, LanguageCall, LanguageProfile,
    LanguageRequest, ModelRef, PreparedImageCall, PreparedLanguageCall, ToolDialect,
};
use rsi_approval_protocol::ApprovalDecision;
use rsi_sandbox::SandboxMode;
use rsi_session::{
    AgentSettingsSource, CreateSession, LocalSessionApplication, NoApprovalControl,
    SessionApplication, SessionApplicationError, SubmitDirectImage, SubmitText,
};
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError, ToolResultIdentity,
    ToolRuntime,
};
use rsi_workspace::{WorkspaceId, WorkspaceRecord, WorkspaceRegistry, WorkspaceStatus};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct UnavailableTurns;

#[async_trait]
impl TurnService for UnavailableTurns {
    async fn prepare_resume(&self, _session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        panic!("durable attachment must not prepare execution")
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
    async fn list(&self) -> Vec<WorkspaceRecord> {
        panic!("not used")
    }

    async fn get_or_create(&self, _path: &Path) -> rsi_workspace::Result<WorkspaceRecord> {
        panic!("durable attachment must not register a workspace")
    }

    async fn status(&self, _id: &WorkspaceId) -> rsi_workspace::Result<WorkspaceStatus> {
        panic!("not used")
    }

    async fn delete_registration(&self, _id: &WorkspaceId) -> rsi_workspace::Result<bool> {
        panic!("not used")
    }
}

#[derive(Debug, Default)]
struct RejectingWorkspace {
    registrations: AtomicUsize,
}

#[async_trait]
impl WorkspaceRegistry for RejectingWorkspace {
    async fn list(&self) -> Vec<WorkspaceRecord> {
        Vec::new()
    }

    async fn get_or_create(&self, _path: &Path) -> rsi_workspace::Result<WorkspaceRecord> {
        self.registrations.fetch_add(1, Ordering::AcqRel);
        Err(rsi_workspace::WorkspaceError::Storage(
            "unexpected workspace mutation".into(),
        ))
    }

    async fn status(&self, _id: &WorkspaceId) -> rsi_workspace::Result<WorkspaceStatus> {
        panic!("not used")
    }

    async fn delete_registration(&self, _id: &WorkspaceId) -> rsi_workspace::Result<bool> {
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
    fn current(&self) -> FrozenAgentSettings {
        panic!("durable attachment must not read current settings")
    }
}

#[derive(Debug)]
struct ImageSettings;

impl AgentSettingsSource for ImageSettings {
    fn current(&self) -> FrozenAgentSettings {
        FrozenAgentSettings::new(
            "settings",
            "system",
            ModelRef::new("removed-provider", "removed-model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap()
    }
}

#[derive(Debug)]
struct TextSettings;

impl AgentSettingsSource for TextSettings {
    fn current(&self) -> FrozenAgentSettings {
        test_settings()
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
async fn fresh_preset_failure_precedes_workspace_registration() {
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let workspace = Arc::new(RejectingWorkspace::default());
    let application = LocalSessionApplication::new(
        Arc::new(UnavailableTurns),
        store,
        Arc::new(FailingComposition),
        workspace.clone(),
        Arc::new(TextSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(NoApprovalControl),
    );

    let result = application
        .create(CreateSession {
            cwd: std::env::current_dir().unwrap(),
            session_id: Some(SessionId::new("session-failing-fresh-preset").unwrap()),
            agent_preset_id: Some(AgentPresetId::new("missing-preset").unwrap()),
        })
        .await;

    assert!(matches!(result, Err(SessionApplicationError::Backend(_))));
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
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: vec![
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
            ],
        })
        .await
        .unwrap();
    let store: Arc<dyn SessionStore> = store;
    let workspace = Arc::new(RejectingWorkspace::default());
    let application = LocalSessionApplication::new(
        Arc::new(RejectingResumeTurns),
        store,
        Arc::new(UnavailableComposition),
        workspace.clone(),
        Arc::new(UnavailableSettings),
        Arc::new(AvailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(NoApprovalControl),
    );
    let handle = application.attach(&session_id).await.unwrap();

    let result = handle
        .submit_text(SubmitText {
            turn_id: TurnId::new("turn-must-not-start").unwrap(),
            text: "must not start".into(),
            model: None,
            sandbox: None,
        })
        .await;

    assert!(matches!(result, Err(SessionApplicationError::Backend(_))));
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
            facts: vec![accepted.clone()],
        })
        .await
        .unwrap();
    let store_service: Arc<dyn SessionStore> = store;
    let application = LocalSessionApplication::new(
        Arc::new(UnavailableTurns),
        store_service,
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(NoApprovalControl),
    );

    let handle = application.attach(&session_id).await.unwrap();
    assert_eq!(handle.header().await.unwrap(), header);
    assert_eq!(
        handle.history_before(None, 8).await.unwrap().facts,
        [accepted]
    );
    assert!(handle.pending_approvals().await.unwrap().is_empty());
    assert!(
        handle
            .answer_approval("missing", ApprovalDecision::Deny)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn image_only_draft_defers_language_and_workspace_until_the_selected_operation_needs_them() {
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let workspace = Arc::new(RejectingWorkspace::default());
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let application = LocalSessionApplication::new(
        Arc::new(ImageTurns),
        store,
        Arc::new(AvailableComposition),
        workspace.clone(),
        Arc::new(ImageSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(AvailableImage),
        Arc::new(NoApprovalControl),
    );
    let handle = application
        .create(CreateSession {
            cwd,
            session_id: Some(SessionId::new("session-image-only").unwrap()),
            agent_preset_id: None,
        })
        .await
        .expect("draft creation must not resolve a Language route or mutate Workspace");
    assert_eq!(workspace.registrations.load(Ordering::Acquire), 0);

    let receipt = handle
        .submit_image(SubmitDirectImage {
            turn_id: TurnId::new("turn-image-only").unwrap(),
            model: ModelRef::new("image-provider", "image-model").unwrap(),
            request: ImageRequest::new("draw a square", 1).unwrap(),
        })
        .await
        .expect("direct Image submission must not require a Language route");
    assert_eq!(receipt.accepted_seq, 1);
    assert_eq!(workspace.registrations.load(Ordering::Acquire), 0);
}
