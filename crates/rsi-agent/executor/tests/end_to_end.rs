use async_trait::async_trait;
use futures_util::stream;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentCompositionError, AgentCompositionPin,
    PreparedFreshSession,
};
use rsi_agent_context::{ContextFold, ContextLimits};
use rsi_agent_executor::ExecutorFactory;
use rsi_agent_kernel::KernelFactory;
use rsi_agent_session_protocol::{
    AgentPresetId, BudgetDimension, FrozenAgentSettings, SessionFactBody, SessionHeader, SessionId,
    TurnBudget, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::{SessionStore, StoredContextCheckpoint, WriteContextCheckpoint};
use rsi_agent_testkit::{MemoryStore, MemoryStoreFactory};
use rsi_agent_turn_protocol::{
    ContextCheckpoint, FinalizationResult, PublishAttempt, SubmitImage, SubmitSession, SubmitTurn,
    SubmittedTurn, TurnCompletionBlocker, TurnExecutionContract, TurnFinalizationContext,
    TurnFinalizationContract, TurnFinalizationError, TurnFinalizationReport, TurnFinalizer,
    TurnServiceContract,
};
use rsi_ai_protocol::{
    AiCapability, AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase,
    FinishReason, ImageCall, ImageCallContract, ImageEvent, ImageRequest, ImageStream,
    ImageToolResultCapability, LanguageCall, LanguageCallContract, LanguageEvent, LanguageProfile,
    LanguageRequest, LanguageStream, ModelRef, PreparedCallSnapshot, PreparedImageCall,
    PreparedLanguageCall, RetryPolicy, ToolCallKind, ToolDialect,
};
use rsi_approval_protocol::{
    Approval, ApprovalContract, ApprovalDecision, ApprovalOutcome, ApprovalRequest,
};
use rsi_jobs_local::JobsLocalFactory;
use rsi_media_protocol::{Media, MediaContract, MediaError, MediaId, MediaRef, StoredMedia};
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, PluginFactory, PreparedActivation, ResolvedFactory,
    Runtime, UpdateMode,
};
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessRequest, Sandbox, SandboxBackend, SandboxContract,
    SandboxFileSystem, SandboxMode, SandboxNetwork, SandboxScratch,
};
use rsi_tools::ToolsFactory;
use rsi_tools_protocol::{
    Result as ToolResultType, RetainedToolResult, ToolCatalogProviderContract, ToolCatalogStage,
    ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrar, ToolRegistration,
    ToolResult, ToolRuntime,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

fn client_turn_id() -> TurnId {
    static NEXT_CLIENT_TURN: AtomicUsize = AtomicUsize::new(1);
    TurnId::new(format!(
        "caller-turn-{}",
        NEXT_CLIENT_TURN.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap()
}

#[derive(Debug)]
struct AllowApproval;

#[async_trait]
impl Approval for AllowApproval {
    async fn ask(
        &self,
        _request: ApprovalRequest,
        _cancellation: CancellationToken,
    ) -> rsi_approval_protocol::Result<ApprovalOutcome> {
        Ok(ApprovalOutcome {
            decision: ApprovalDecision::AllowOnce,
            answerer: "test".into(),
            reason: None,
        })
    }
}

#[derive(Debug)]
struct TestSandbox;

#[async_trait]
impl Sandbox for TestSandbox {
    async fn confine(&self, request: ProcessRequest) -> rsi_sandbox::Result<ConfinedProcess> {
        let (backend, filesystem, scratch) = match request.mode {
            SandboxMode::ReadOnly => (
                SandboxBackend::Bubblewrap {
                    sha256: "0".repeat(64),
                },
                SandboxFileSystem::ReadOnly,
                SandboxScratch::PrivateTmp,
            ),
            SandboxMode::WorkspaceWrite => (
                SandboxBackend::Bubblewrap {
                    sha256: "0".repeat(64),
                },
                SandboxFileSystem::WorkspaceWrite,
                SandboxScratch::PrivateTmp,
            ),
            SandboxMode::DangerFullAccess => (
                SandboxBackend::Unconfined,
                SandboxFileSystem::Unconfined,
                SandboxScratch::Host,
            ),
        };
        Ok(ConfinedProcess {
            program: request.program,
            arguments: request.arguments.into_iter().map(Into::into).collect(),
            cwd: request.cwd,
            stamp: EnforcementStamp {
                requested: request.mode,
                backend,
                workspace: request.workspace,
                filesystem,
                scratch,
                network: SandboxNetwork::Host,
            },
        })
    }
}

#[derive(Debug)]
struct SecurityFixtureFactory;

#[async_trait]
impl PluginFactory for SecurityFixtureFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let approval = plan
            .context()
            .provide_local::<ApprovalContract>(Arc::new(AllowApproval))?;
        let sandbox = plan
            .context()
            .provide_local::<SandboxContract>(Arc::new(TestSandbox))?;
        plan.defer(
            "withdraw security fixtures",
            Box::new(move || {
                Box::pin(async move {
                    drop(sandbox);
                    drop(approval);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct PreparedScript {
    snapshot: PreparedCallSnapshot,
    outcome: StartOutcome,
    store: Arc<MemoryStore>,
    starts: Arc<AtomicUsize>,
}

#[derive(Debug)]
enum StartOutcome {
    Stream(Vec<LanguageEvent>),
    GatedStream {
        events: Vec<LanguageEvent>,
        waiting_after_first: Arc<Notify>,
        release: Arc<Notify>,
    },
    Error(AiError),
}

#[async_trait]
impl PreparedLanguageCall for PreparedScript {
    fn snapshot(&self) -> &PreparedCallSnapshot {
        &self.snapshot
    }

    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<LanguageStream, AiError> {
        assert_latest_is(&self.store, |body| {
            matches!(body, SessionFactBody::ModelStarted { .. })
        })
        .await;
        self.starts.fetch_add(1, Ordering::AcqRel);
        match self.outcome {
            StartOutcome::Stream(script) => Ok(Box::pin(stream::iter(script.into_iter().map(Ok)))),
            StartOutcome::GatedStream {
                events,
                waiting_after_first,
                release,
            } => Ok(Box::pin(stream::unfold(
                (events.into_iter(), 0_usize),
                move |(mut events, index)| {
                    let waiting_after_first = Arc::clone(&waiting_after_first);
                    let release = Arc::clone(&release);
                    let cancellation = cancellation.clone();
                    async move {
                        if index == 1 {
                            waiting_after_first.notify_one();
                            tokio::select! {
                                () = release.notified() => {}
                                () = cancellation.cancelled() => {
                                    let error = AiError::new(
                                        ErrorKind::Cancelled,
                                        ErrorPhase::Stream,
                                        DispatchStatus::Dispatched,
                                        "fixture cancelled",
                                    )
                                    .unwrap();
                                    return Some((Err(error), (events, index + 1)));
                                }
                            }
                        }
                        events.next().map(|event| (Ok(event), (events, index + 1)))
                    }
                },
            ))),
            StartOutcome::Error(error) => Err(error),
        }
    }
}

#[derive(Debug)]
struct LanguageFixture {
    outcomes: Mutex<VecDeque<StartOutcome>>,
    requests: Mutex<Vec<LanguageRequest>>,
    starts: Arc<AtomicUsize>,
    store: Arc<MemoryStore>,
    retry_policy: RetryPolicy,
}

#[derive(Debug)]
struct PendingLanguage {
    entered: Arc<Notify>,
}

#[async_trait]
impl LanguageCall for PendingLanguage {
    fn describe(&self, _model: &ModelRef) -> Result<LanguageProfile, AiError> {
        Ok(LanguageProfile::new(
            100_000,
            1_000,
            10_000,
            ToolDialect::Responses,
            true,
            ImageToolResultCapability::No,
            vec![],
        )
        .unwrap())
    }

    async fn prepare(
        &self,
        _model: ModelRef,
        _request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[derive(Clone, Debug)]
struct PendingLanguageFactory {
    fixture: Arc<PendingLanguage>,
}

#[async_trait]
impl PluginFactory for PendingLanguageFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let language: Arc<dyn LanguageCall> = self.fixture.clone();
        let supply = plan
            .context()
            .provide_local::<LanguageCallContract>(language)?;
        plan.defer(
            "withdraw pending test Language",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct PreparedImageScript {
    snapshot: PreparedCallSnapshot,
    events: Vec<std::result::Result<ImageEvent, AiError>>,
    store: Arc<MemoryStore>,
}

#[async_trait]
impl PreparedImageCall for PreparedImageScript {
    fn snapshot(&self) -> &PreparedCallSnapshot {
        &self.snapshot
    }

    async fn start(
        self: Box<Self>,
        _cancellation: CancellationToken,
    ) -> std::result::Result<ImageStream, AiError> {
        assert_latest_is(&self.store, |body| {
            matches!(body, SessionFactBody::ImageStarted { .. })
        })
        .await;
        Ok(Box::pin(stream::iter(self.events)))
    }
}

#[derive(Debug)]
struct ImageFixture {
    events: Mutex<VecDeque<Vec<std::result::Result<ImageEvent, AiError>>>>,
    store: Arc<MemoryStore>,
}

#[async_trait]
impl ImageCall for ImageFixture {
    fn describe(&self, _model: &ModelRef) -> std::result::Result<(), AiError> {
        Ok(())
    }

    async fn prepare(
        &self,
        model: ModelRef,
        _request: ImageRequest,
    ) -> std::result::Result<Box<dyn PreparedImageCall>, AiError> {
        Ok(Box::new(PreparedImageScript {
            snapshot: PreparedCallSnapshot {
                call_id: "image-call-1".into(),
                deployment_id: model.deployment().into(),
                provider_family: "test".into(),
                capability: AiCapability::Image,
                model: model.model().into(),
                protocol: "test".into(),
                transport: "memory".into(),
                endpoint_fingerprint: "fixture".into(),
                config_generation: 1,
                credential_source: None,
                retry_policy: RetryPolicy::default(),
                request_sha256: "b".repeat(64),
            },
            events: self
                .events
                .lock()
                .unwrap()
                .pop_front()
                .expect("image script"),
            store: self.store.clone(),
        }))
    }
}

#[derive(Debug)]
struct MediaFixture {
    imports: AtomicUsize,
    store: Arc<MemoryStore>,
}

#[async_trait]
impl Media for MediaFixture {
    async fn import_image(&self, source: Arc<[u8]>) -> rsi_media_protocol::Result<MediaRef> {
        let index = self.imports.fetch_add(1, Ordering::AcqRel);
        if index > 0 {
            assert_latest_is(&self.store, |body| {
                matches!(body, SessionFactBody::ImageOutput { index: 0, .. })
            })
            .await;
        }
        let digest = char::from(b'c' + u8::try_from(index).unwrap())
            .to_string()
            .repeat(64);
        Ok(MediaRef {
            id: MediaId::new(digest).unwrap(),
            mime: "image/png".into(),
            bytes: u64::try_from(source.len()).unwrap(),
            width: 1,
            height: 1,
        })
    }

    async fn read(&self, reference: &MediaRef) -> rsi_media_protocol::Result<StoredMedia> {
        Err(MediaError::NotFound(reference.id.clone()))
    }
}

#[derive(Clone, Debug)]
struct ImageMediaFixtureFactory {
    image: Arc<ImageFixture>,
    media: Arc<MediaFixture>,
}

#[async_trait]
impl PluginFactory for ImageMediaFixtureFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let image: Arc<dyn ImageCall> = self.image.clone();
        let media: Arc<dyn Media> = self.media.clone();
        let image_supply = plan.context().provide_local::<ImageCallContract>(image)?;
        let media_supply = plan.context().provide_local::<MediaContract>(media)?;
        plan.defer(
            "withdraw test Image and Media",
            Box::new(move || {
                Box::pin(async move {
                    drop(media_supply);
                    drop(image_supply);
                    Ok(())
                })
            }),
        )
    }
}

#[async_trait]
impl LanguageCall for LanguageFixture {
    fn describe(&self, _model: &ModelRef) -> Result<LanguageProfile, AiError> {
        Ok(LanguageProfile::new(
            100_000,
            1_000,
            10_000,
            ToolDialect::Responses,
            true,
            ImageToolResultCapability::No,
            vec![],
        )
        .unwrap())
    }

    async fn prepare(
        &self,
        model: ModelRef,
        request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        self.requests.lock().unwrap().push(request);
        let call = self.requests.lock().unwrap().len();
        Ok(Box::new(PreparedScript {
            snapshot: PreparedCallSnapshot {
                call_id: format!("model-call-{call}"),
                deployment_id: model.deployment().into(),
                provider_family: "test".into(),
                capability: AiCapability::Language,
                model: model.model().into(),
                protocol: "test".into(),
                transport: "memory".into(),
                endpoint_fingerprint: "fixture".into(),
                config_generation: 1,
                credential_source: None,
                retry_policy: self.retry_policy.clone(),
                request_sha256: "a".repeat(64),
            },
            outcome: self.outcomes.lock().unwrap().pop_front().expect("outcome"),
            store: self.store.clone(),
            starts: self.starts.clone(),
        }))
    }
}

#[derive(Clone, Debug)]
struct LanguageFixtureFactory {
    fixture: Arc<LanguageFixture>,
}

#[async_trait]
impl PluginFactory for LanguageFixtureFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let language: Arc<dyn LanguageCall> = self.fixture.clone();
        let supply = plan
            .context()
            .provide_local::<LanguageCallContract>(language)?;
        plan.defer(
            "withdraw test Language",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct EchoTool {
    store: Arc<MemoryStore>,
    calls: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct NonCooperativeTool {
    entered: Arc<Notify>,
    release: CancellationToken,
}

#[derive(Debug)]
struct FailingTool {
    store: Arc<MemoryStore>,
}

#[derive(Debug)]
struct FailingFinalizer;

#[derive(Debug)]
struct CompletionBlockerFinalizer;

#[async_trait]
impl TurnFinalizer for FailingFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport> {
        Err(TurnFinalizationError::Failed {
            code: "test.finalization".into(),
            message: "fixture finalization failure".into(),
        })
    }
}

#[async_trait]
impl TurnFinalizer for CompletionBlockerFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport> {
        Ok(TurnFinalizationReport::blocked(TurnCompletionBlocker::new(
            "jobs.unreported",
            "background output was not collected",
        )?))
    }
}

#[async_trait]
impl ToolExecutor for FailingTool {
    async fn execute(
        &self,
        _arguments: Value,
        _execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        assert_latest_is(&self.store, |body| {
            matches!(body, SessionFactBody::ToolStarted { .. })
        })
        .await;
        Err(ToolError::Execution("fixture failure".into()))
    }
}

#[async_trait]
impl ToolExecutor for EchoTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        assert_latest_is(&self.store, |body| {
            matches!(body, SessionFactBody::ToolStarted { .. })
        })
        .await;
        let _confined = execution.confine("/bin/echo".into(), Vec::new()).await?;
        self.calls.fetch_add(1, Ordering::AcqRel);
        ToolResult::new(arguments, vec![], false)
    }
}

#[async_trait]
impl ToolExecutor for NonCooperativeTool {
    async fn execute(
        &self,
        arguments: Value,
        _execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        self.entered.notify_one();
        self.release.cancelled().await;
        ToolResult::new(arguments, vec![], false)
    }
}

async fn assert_latest_is(store: &MemoryStore, predicate: impl Fn(&SessionFactBody) -> bool) {
    let session = store
        .list_sessions(None, 1)
        .await
        .unwrap()
        .sessions
        .into_iter()
        .next()
        .unwrap();
    let page = store.read_facts(&session, 0, 64).await.unwrap();
    assert!(predicate(page.facts.last().unwrap().body()));
}

fn tool_script() -> Vec<LanguageEvent> {
    vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::ToolCall {
                id: "tool-call-1".into(),
                name: "echo".into(),
                kind: ToolCallKind::Function,
            },
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::ToolArguments(r#"{"value":42}"#.into()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::ToolCalls,
            replay: None,
        },
    ]
}

fn answer_script() -> Vec<LanguageEvent> {
    vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("done".into()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        },
    ]
}

fn header() -> SessionHeader {
    header_with_budget(TurnBudget::default())
}

fn header_with_budget(turn_budget: TurnBudget) -> SessionHeader {
    SessionHeader::new(
        SessionId::new("session-e2e").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new_with_budget(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
            turn_budget,
        )
        .unwrap(),
    )
    .unwrap()
}

#[derive(Debug)]
struct GenerationOwner(Arc<AtomicUsize>);

impl Drop for GenerationOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct CompositionFixture {
    stage: Mutex<Option<Box<dyn ToolCatalogStage>>>,
    registrar: Arc<dyn ToolRegistrar>,
    pin: Mutex<Option<AgentCompositionPin>>,
    owner_drops: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentComposition for CompositionFixture {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        let mut current = self.pin.lock().unwrap();
        if let Some(pin) = current.as_ref() {
            if pin.preset_id() == preset_id {
                return Ok(pin.clone());
            }
            return Err(AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: "test fixture owns one preset generation".into(),
            });
        }
        let stage = self.stage.lock().unwrap().take().ok_or_else(|| {
            AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: "test Tool catalog was already consumed".into(),
            }
        })?;
        let tools = stage
            .seal()
            .map_err(|error| AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: error.to_string(),
            })?;
        let pin = AgentCompositionPin::new(
            preset_id.clone(),
            "b".repeat(64),
            tools,
            Arc::new(GenerationOwner(Arc::clone(&self.owner_drops))),
        )?;
        *current = Some(pin.clone());
        Ok(pin)
    }
}

#[derive(Clone, Debug)]
struct CompositionFixtureFactory {
    installed: Arc<Mutex<Option<Arc<CompositionFixture>>>>,
}

#[async_trait]
impl PluginFactory for CompositionFixtureFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone())
            .requiring_local::<ToolCatalogProviderContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let stage = plan
            .local::<ToolCatalogProviderContract>()?
            .begin_stage()
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        let registrar = stage.registrar();
        let fixture = Arc::new(CompositionFixture {
            stage: Mutex::new(Some(stage)),
            registrar,
            pin: Mutex::new(None),
            owner_drops: Arc::new(AtomicUsize::new(0)),
        });
        *self.installed.lock().unwrap() = Some(Arc::clone(&fixture));
        let service: Arc<dyn AgentComposition> = fixture;
        let supply = plan
            .context()
            .provide_local::<AgentCompositionContract>(service)?;
        let installed = Arc::clone(&self.installed);
        plan.defer(
            "withdraw test Agent composition",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    installed.lock().unwrap().take();
                    Ok(())
                })
            }),
        )
    }
}

struct BaseStack {
    runtime: Runtime,
    store: Arc<MemoryStore>,
    image: Arc<ImageFixture>,
    media: Arc<MediaFixture>,
    store_fiber: FiberHandle,
    kernel_fiber: FiberHandle,
    tools_fiber: FiberHandle,
    composition_fiber: FiberHandle,
    composition: Arc<CompositionFixture>,
    tool_registrar: Arc<dyn ToolRegistrar>,
    jobs_fiber: FiberHandle,
    security_fiber: FiberHandle,
    image_media_fiber: FiberHandle,
}

impl BaseStack {
    #[allow(clippy::too_many_lines)] // The fixture keeps the complete dependency-order activation visible.
    async fn activate() -> Self {
        let runtime = Runtime::default();
        let store = Arc::new(MemoryStore::new());
        let store_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "rsi.agent.store.memory",
                    "store",
                    UpdateMode::Replayable,
                    Arc::new(MemoryStoreFactory::new(store.clone())),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let tools_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "rsi.tools",
                    "tools",
                    UpdateMode::Replayable,
                    Arc::new(ToolsFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let installed = Arc::new(Mutex::new(None));
        let composition_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "test.agent.composition",
                    "composition",
                    UpdateMode::Replayable,
                    Arc::new(CompositionFixtureFactory {
                        installed: Arc::clone(&installed),
                    }),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let composition = installed
            .lock()
            .unwrap()
            .clone()
            .expect("composition fixture activated");
        let tool_registrar = Arc::clone(&composition.registrar);
        let kernel_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "rsi.agent.kernel",
                    "kernel",
                    UpdateMode::Replayable,
                    Arc::new(KernelFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let jobs_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "rsi.jobs.local",
                    "jobs",
                    UpdateMode::Replayable,
                    Arc::new(JobsLocalFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let security_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "test.security",
                    "security",
                    UpdateMode::Replayable,
                    Arc::new(SecurityFixtureFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let image = Arc::new(ImageFixture {
            events: Mutex::new(VecDeque::new()),
            store: store.clone(),
        });
        let media = Arc::new(MediaFixture {
            imports: AtomicUsize::new(0),
            store: store.clone(),
        });
        let image_media_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "test.image-media",
                    "image-media",
                    UpdateMode::Replayable,
                    Arc::new(ImageMediaFixtureFactory {
                        image: image.clone(),
                        media: media.clone(),
                    }),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        Self {
            runtime,
            store,
            image,
            media,
            store_fiber,
            kernel_fiber,
            tools_fiber,
            composition_fiber,
            composition,
            tool_registrar,
            jobs_fiber,
            security_fiber,
            image_media_fiber,
        }
    }

    async fn activate_language(&self, key: &str, fixture: Arc<LanguageFixture>) -> FiberHandle {
        self.runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    key,
                    "language",
                    UpdateMode::Replayable,
                    Arc::new(LanguageFixtureFactory { fixture }),
                ),
                Value::Null,
            )
            .await
            .unwrap()
    }

    async fn activate_executor(&self, executor_id: &str) -> FiberHandle {
        self.activate_executor_with_config(json!({"executor_id": executor_id}))
            .await
    }

    async fn activate_executor_with_config(&self, config: Value) -> FiberHandle {
        self.runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "rsi.agent.executor",
                    "executor",
                    UpdateMode::Replayable,
                    Arc::new(ExecutorFactory),
                ),
                config,
            )
            .await
            .unwrap()
    }

    async fn submit_and_wait(&self, text: &str) -> (SubmittedTurn, TurnOutcome) {
        self.submit_and_wait_with_sandbox(text, None).await
    }

    async fn submit_and_wait_with_sandbox(
        &self,
        text: &str,
        sandbox: Option<SandboxMode>,
    ) -> (SubmittedTurn, TurnOutcome) {
        self.submit_and_wait_with_header(text, sandbox, header())
            .await
    }

    async fn submit_and_wait_with_header(
        &self,
        text: &str,
        sandbox: Option<SandboxMode>,
        header: SessionHeader,
    ) -> (SubmittedTurn, TurnOutcome) {
        let turns = self
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = turns
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: self.fresh(header).await,
                text: text.into(),
                model: None,
                sandbox,
            })
            .await
            .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break outcome;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        (submitted, outcome)
    }

    async fn fresh(&self, header: SessionHeader) -> SubmitSession {
        let pin = self
            .composition
            .pin(header.agent_preset_id())
            .await
            .unwrap();
        SubmitSession::Fresh(PreparedFreshSession::new(header, pin).unwrap())
    }

    fn tool_runtime(&self) -> Arc<dyn ToolRuntime> {
        self.composition
            .pin
            .lock()
            .unwrap()
            .as_ref()
            .expect("a fresh session sealed the Tool catalog")
            .tools()
    }

    async fn resume_and_wait(
        &self,
        text: &str,
        session_id: SessionId,
    ) -> (SubmittedTurn, TurnOutcome) {
        let turns = self
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = turns
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: SubmitSession::Resume(turns.prepare_resume(&session_id).await.unwrap()),
                text: text.into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break outcome;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        (submitted, outcome)
    }

    async fn dispose(self, language_fiber: FiberHandle, executor_fiber: FiberHandle) {
        assert!(executor_fiber.dispose().await.is_clean());
        assert!(language_fiber.dispose().await.is_clean());
        assert!(self.image_media_fiber.dispose().await.is_clean());
        assert!(self.security_fiber.dispose().await.is_clean());
        assert!(self.jobs_fiber.dispose().await.is_clean());
        assert!(self.kernel_fiber.dispose().await.is_clean());
        assert!(self.composition_fiber.dispose().await.is_clean());
        assert!(self.tools_fiber.dispose().await.is_clean());
        assert!(self.store_fiber.dispose().await.is_clean());
    }
}

#[derive(Debug)]
struct HangingFinalizer {
    entered: Arc<Notify>,
}

#[async_trait]
impl TurnFinalizer for HangingFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn elapsed_budget_bounds_a_provider_prepare_that_never_returns() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(PendingLanguage {
        entered: Arc::new(Notify::new()),
    });
    let language_fiber = stack
        .runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "test.language.pending-prepare",
                "language",
                UpdateMode::Replayable,
                Arc::new(PendingLanguageFactory { fixture }),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-elapsed-budget").await;
    let budget = TurnBudget::new(20, 64, 256, 65_536, 67_108_864).unwrap();

    let (_, outcome) = stack
        .submit_and_wait_with_header("never prepare", None, header_with_budget(budget))
        .await;

    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            consumed,
            limit: 20,
        } if consumed >= 20
    ));
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Keep admission, terminal eviction, retained settlement, and final generation release in one public scenario.
async fn elapsed_budget_retires_an_admitted_tool_after_it_settles() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.elapsed-tool", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-elapsed-tool").await;
    let budget = TurnBudget::new(50, 64, 256, 65_536, 67_108_864).unwrap();
    let fresh = stack.fresh(header_with_budget(budget)).await;
    let tool_runtime = stack.tool_runtime();

    let turn = tokio::spawn({
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        async move {
            let submitted = turns
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: fresh,
                    text: "delay the tool".into(),
                    model: None,
                    sandbox: None,
                })
                .await
                .unwrap();
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break (submitted, outcome);
                }
                tokio::task::yield_now().await;
            }
        }
    });
    entered.notified().await;
    drop(stack.composition.pin.lock().unwrap().take());
    let (submitted, outcome) = tokio::time::timeout(std::time::Duration::from_secs(2), turn)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            limit: 50,
            ..
        }
    ));
    assert_eq!(
        stack.composition.owner_drops.load(Ordering::Acquire),
        0,
        "a retained Tool must keep its exact generation alive after the resident session is terminal"
    );
    let identity = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .find_map(|fact| match fact.into_body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity),
            _ => None,
        })
        .expect("durable ToolStarted identity");

    release.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if tool_runtime.query(&identity).unwrap() == RetainedToolResult::Absent {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("elapsed terminal must retire the later-settled retained Tool identity");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the retained Tool's final pin must release after settlement");

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Keep executor replacement, durable recovery, terminal eviction, and generation ownership in one scenario.
async fn recovered_pending_tool_keeps_its_generation_pin_through_elapsed_retirement() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.recovered-tool", fixture)
        .await;
    let first_executor = stack.activate_executor("executor-before-recovery").await;
    let budget = TurnBudget::new(250, 64, 256, 65_536, 67_108_864).unwrap();
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header_with_budget(budget)).await,
            text: "recover the pending tool".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    entered.notified().await;
    assert!(first_executor.dispose().await.is_clean());

    let second_executor = stack.activate_executor("executor-after-recovery").await;
    let tool_runtime = stack.tool_runtime();
    drop(stack.composition.pin.lock().unwrap().take());
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the inherited elapsed deadline did not terminate recovery");
    assert!(matches!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            limit: 250,
            ..
        }
    ));
    assert_eq!(
        stack.composition.owner_drops.load(Ordering::Acquire),
        0,
        "terminal eviction released the recovered Tool's generation while it was pending"
    );

    let identity = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .find_map(|fact| match fact.into_body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity),
            _ => None,
        })
        .expect("durable recovered ToolStarted identity");
    release.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while tool_runtime.query(&identity).unwrap() != RetainedToolResult::Absent {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovered Tool identity was not retired after settlement");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovered Tool's final generation pin was not released after settlement");

    drop((tool_runtime, turns, tool_lease, tools));
    stack.dispose(language_fiber, second_executor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successfully_recovered_tool_releases_its_tracking_pin_after_commit() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.successfully-recovered-tool", fixture)
        .await;
    let first_executor = stack
        .activate_executor("executor-before-successful-recovery")
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "recover and finish the pending tool".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    entered.notified().await;
    assert!(first_executor.dispose().await.is_clean());

    let second_executor = stack
        .activate_executor("executor-after-successful-recovery")
        .await;
    drop(stack.composition.pin.lock().unwrap().take());
    release.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successfully recovered Tool did not complete the turn");
    assert_eq!(outcome, TurnOutcome::Completed);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a committed recovered Tool left its tracking generation pin resident");

    drop((turns, tool_lease, tools));
    stack.dispose(language_fiber, second_executor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_tool_retirement_does_not_block_the_next_claim() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "delayed", json!({"type":"object"})).unwrap(),
            timeout_ms: 60_000,
            executor: Arc::new(NonCooperativeTool {
                entered: Arc::clone(&entered),
                release: release.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.nonblocking-retirement", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id":"executor-nonblocking-retirement",
            "retained_tool_wait_ms":200
        }))
        .await;
    let budget = TurnBudget::new(50, 64, 256, 65_536, 67_108_864).unwrap();

    let first = tokio::spawn({
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let fresh = stack.fresh(header_with_budget(budget)).await;
        async move {
            let submitted = turns
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: fresh,
                    text: "delay the tool".into(),
                    model: None,
                    sandbox: None,
                })
                .await
                .unwrap();
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break (submitted, outcome);
                }
                tokio::task::yield_now().await;
            }
        }
    });
    entered.notified().await;
    let (submitted, first_outcome) = first.await.unwrap();
    assert!(matches!(
        first_outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::Elapsed,
            ..
        }
    ));

    let (_, second_outcome) = stack
        .resume_and_wait("the next claim must run", submitted.session_id)
        .await;
    assert_eq!(second_outcome, TurnOutcome::Completed);

    drop(stack.composition.pin.lock().unwrap().take());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while stack.composition.owner_drops.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the retirement deadline must release its exact generation pin");

    release.cancel();
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
async fn hanging_finalizer_becomes_a_durable_bounded_failure() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.hanging-finalizer", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let entered = Arc::new(Notify::new());
    let finalizer_lease = finalization
        .register(
            "hanging-test-finalizer".into(),
            Arc::new(HangingFinalizer {
                entered: Arc::clone(&entered),
            }),
        )
        .unwrap();
    let executor_fiber = stack
        .activate_executor_with_config(json!({
            "executor_id": "executor-finalization-timeout",
            "finalization_wait_ms": 10
        }))
        .await;

    let (_, outcome) = stack.submit_and_wait("finish with a stuck finalizer").await;
    entered.notified().await;
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "turn.finalization_timeout"
    ));

    drop(finalizer_lease);
    drop(finalization);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The three-turn public-seam interleaving is clearer as one linear timeline.
async fn checkpoint_after_a_later_acceptance_cannot_cross_the_claim_acceptance_fence() {
    let stack = BaseStack::activate().await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "second private".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let third = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "third private".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();

    let execution = stack
        .runtime
        .root()
        .lookup_local::<TurnExecutionContract>()
        .unwrap();
    let lease = execution.register("checkpoint-builder".into()).unwrap();
    let first_claim = execution
        .claim("checkpoint-builder", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id(), &first.turn_id);
    let terminal = match execution
        .publish(
            &first_claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
    {
        PublishAttempt::Published(facts) => facts,
        PublishAttempt::FlushRequired { .. } => panic!("terminal unexpectedly required a flush"),
    };
    execution
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    let mut fold =
        ContextFold::with_limits(first_claim.header().clone(), ContextLimits::default()).unwrap();
    loop {
        let after_seq = fold.through_seq();
        let page = execution
            .read_checkpoint_facts(
                &first_claim,
                after_seq,
                rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
            )
            .await
            .unwrap()
            .unwrap();
        if page.through_seq == after_seq {
            break;
        }
        fold.apply_page(&page.facts, page.through_seq).unwrap();
    }
    assert!(fold.through_seq() >= third.accepted_seq);
    assert!(
        execution
            .write_context_checkpoint(
                &first_claim,
                ContextCheckpoint {
                    header_fingerprint: first_claim.header().fingerprint().unwrap(),
                    through_seq: fold.through_seq(),
                    fact_prefix_sha256: fold.fact_prefix_sha256(),
                    bytes: fold.checkpoint_bytes().unwrap(),
                },
            )
            .await
            .unwrap()
    );
    drop(lease);

    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.checkpoint-claim-fence", fixture.clone())
        .await;
    let executor_fiber = stack
        .activate_executor("executor-checkpoint-claim-fence")
        .await;

    for submitted in [&second, &third] {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break outcome;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
    }
    {
        let requests = fixture.requests.lock().unwrap();
        let second_request = serde_json::to_string(&requests[0]).unwrap();
        assert!(second_request.contains("second private"));
        assert!(!second_request.contains("third private"));
    }
    drop(execution);
    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
async fn context_checkpoint_reads_only_suffix_and_corruption_falls_back_equivalently() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.context-checkpoint", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-context-checkpoint").await;

    let (first, first_outcome) = stack.submit_and_wait("first").await;
    assert_eq!(first_outcome, TurnOutcome::Completed);
    let first_checkpoint = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(checkpoint) = stack
                .store
                .read_context_checkpoint(&first.session_id)
                .await
                .unwrap()
            {
                break checkpoint;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stack.store.take_fact_read_cursors();

    let (_, second_outcome) = stack
        .resume_and_wait("second", first.session_id.clone())
        .await;
    assert_eq!(second_outcome, TurnOutcome::Completed);
    let suffix_cursors = stack.store.take_fact_read_cursors();
    assert_eq!(
        suffix_cursors.iter().filter(|cursor| **cursor == 0).count(),
        1,
        "only the provider fixture's durability assertion should scan from zero"
    );

    let second_checkpoint = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let checkpoint = stack
                .store
                .read_context_checkpoint(&first.session_id)
                .await
                .unwrap()
                .unwrap();
            if checkpoint.through_seq > first_checkpoint.through_seq {
                break checkpoint;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stack
        .store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: first.session_id.clone(),
            expected_durable_seq: second_checkpoint.through_seq,
            checkpoint: StoredContextCheckpoint {
                header_fingerprint: second_checkpoint.header_fingerprint,
                through_seq: second_checkpoint.through_seq,
                fact_prefix_sha256: second_checkpoint.fact_prefix_sha256,
                bytes: Arc::from(b"corrupt-context-checkpoint".as_slice()),
            },
        })
        .await
        .unwrap();
    stack.store.take_fact_read_cursors();

    let (_, third_outcome) = stack
        .resume_and_wait("third", first.session_id.clone())
        .await;
    assert_eq!(third_outcome, TurnOutcome::Completed);
    let fallback_cursors = stack.store.take_fact_read_cursors();
    assert!(
        fallback_cursors
            .iter()
            .filter(|cursor| **cursor == 0)
            .count()
            >= 2
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let third_request = serde_json::to_string(&requests[2]).unwrap();
        assert!(third_request.contains("first"));
        assert!(third_request.contains("second"));
        assert!(third_request.contains("third"));
    }
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_persists_intent_and_start_before_model_and_tool_io() {
    let stack = BaseStack::activate().await;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: tool_calls.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-1").await;
    let (submitted, outcome) = stack
        .submit_and_wait_with_sandbox("call echo", Some(SandboxMode::DangerFullAccess))
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    assert_eq!(tool_calls.load(Ordering::Acquire), 1);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let kinds = page
        .facts
        .iter()
        .map(|fact| fact_kind(fact.body()))
        .collect::<Vec<_>>();
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent {
            approval: Some(approval),
            ..
        } if approval.decision == ApprovalDecision::AllowOnce
    )));
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolResult { result, .. } if result.enforcement.len() == 1
    )));
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["model_intent", "model_started"])
    );
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["tool_intent", "tool_started"])
    );
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["tool_started", "tool_result"])
    );
    assert_eq!(kinds.last(), Some(&"terminal"));
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| { message.role() == rsi_ai_protocol::MessageRole::Tool })
        );
    }
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_attempt_budget_stops_a_model_tool_loop_with_durable_evidence() {
    let stack = BaseStack::activate().await;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&tool_calls),
            }),
        })
        .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::clone(&starts),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.budget", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-budget").await;
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();
    let (submitted, outcome) = stack
        .submit_and_wait_with_header("keep calling tools", None, header_with_budget(budget))
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::ProviderAttempts,
            consumed: 2,
            limit: 1,
        }
    );
    assert_eq!(starts.load(Ordering::Acquire), 1);
    assert_eq!(tool_calls.load(Ordering::Acquire), 1);
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let kinds = page
        .facts
        .iter()
        .map(|fact| fact_kind(fact.body()))
        .collect::<Vec<_>>();
    assert!(kinds.ends_with(&["budget_exhausted", "terminal"]));

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalizer_failure_wins_before_any_budget_marker_is_published() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.finalizer-budget", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let finalizer_lease = finalization
        .register("failing-test-finalizer".into(), Arc::new(FailingFinalizer))
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-finalizer-budget").await;
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("fail finalization", None, header_with_budget(budget))
        .await;

    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "test.finalization"
    ));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::BudgetExhausted { .. }))
    );

    drop(finalizer_lease);
    drop(finalization);
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_blocker_replaces_only_an_otherwise_successful_outcome() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.completion-blocker", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let finalizer_lease = finalization
        .register(
            "completion-blocker".into(),
            Arc::new(CompletionBlockerFinalizer),
        )
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-completion-blocker").await;

    let (_, outcome) = stack.submit_and_wait("finish successfully").await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            code: "jobs.unreported".into(),
            message: "background output was not collected".into(),
        }
    );

    drop(finalizer_lease);
    drop(finalization);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_budget_failure_retires_the_retained_identity_after_terminal_durability() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-result-budget", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-result-budget").await;
    let budget = TurnBudget::new(1_800_000, 64, 256, 8, 67_108_864).unwrap();

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("budget the result", None, header_with_budget(budget))
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::BudgetExceeded {
            dimension: BudgetDimension::GeneratedFacts,
            consumed: 9,
            limit: 8,
        }
    );
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("durable ToolStarted identity");
    assert_eq!(
        stack.tool_runtime().query(&identity).unwrap(),
        RetainedToolResult::Absent
    );

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

fn fact_kind(body: &SessionFactBody) -> &'static str {
    match body {
        SessionFactBody::TurnAccepted { .. } => "accepted",
        SessionFactBody::ImageRequested { .. } => "image_requested",
        SessionFactBody::ModelIntent { .. } => "model_intent",
        SessionFactBody::ModelStarted { .. } => "model_started",
        SessionFactBody::ModelEvent { .. } => "model_event",
        SessionFactBody::ImageIntent { .. } => "image_intent",
        SessionFactBody::ImageStarted { .. } => "image_started",
        SessionFactBody::ImageOutput { .. } => "image_output",
        SessionFactBody::ToolIntent { .. } => "tool_intent",
        SessionFactBody::ToolStarted { .. } => "tool_started",
        SessionFactBody::ToolResult { .. } => "tool_result",
        SessionFactBody::TurnTerminal { .. } => "terminal",
        SessionFactBody::CancelRequested { .. } => "cancel",
        SessionFactBody::BudgetExhausted { .. } => "budget_exhausted",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_tool_result_is_retired_after_the_terminal_fact_is_durable() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "fail", json!({"type":"object"})).unwrap(),
            timeout_ms: 2_000,
            executor: Arc::new(FailingTool {
                store: stack.store.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-failure", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-failure").await;
    let (submitted, outcome) = stack.submit_and_wait("fail the tool").await;
    assert!(matches!(outcome, TurnOutcome::Failed { .. }));

    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("durable ToolStarted identity");
    assert_eq!(
        stack.tool_runtime().query(&identity).unwrap(),
        RetainedToolResult::Absent,
        "terminal durability must release the process-local retained slot"
    );

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interleaved_same_session_submission_does_not_fail_the_streaming_turn() {
    let stack = BaseStack::activate().await;
    let waiting_after_first = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let waiting_in_second = Arc::new(Notify::new());
    let release_second = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::GatedStream {
                events: answer_script(),
                waiting_after_first: Arc::clone(&waiting_after_first),
                release: Arc::clone(&release),
            },
            StartOutcome::GatedStream {
                events: answer_script(),
                waiting_after_first: Arc::clone(&waiting_in_second),
                release: Arc::clone(&release_second),
            },
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.interleaved", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-interleaved").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        waiting_after_first.notified(),
    )
    .await
    .expect("executor did not publish the first streamed event");
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    release.notify_one();

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        waiting_in_second.notified(),
    )
    .await
    .expect("executor did not start the queued second turn");
    if let Some(checkpoint) = stack
        .store
        .read_context_checkpoint(&first.session_id)
        .await
        .unwrap()
    {
        assert!(
            checkpoint.through_seq >= second.accepted_seq,
            "a checkpoint racing a queued turn must include that accepted state"
        );
    }
    release_second.notify_one();

    for submitted in [&first, &second] {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(outcome) = turns
                    .outcome(&submitted.session_id, &submitted.turn_id)
                    .await
                    .unwrap()
                {
                    break outcome;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("interleaved turn did not terminate");
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

async fn run_retry_case(
    dispatch_status: DispatchStatus,
) -> (TurnOutcome, Vec<SessionFactBody>, usize) {
    let stack = BaseStack::activate().await;
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Error(
                AiError::new(
                    ErrorKind::RateLimited,
                    ErrorPhase::Connect,
                    dispatch_status,
                    "temporary refusal",
                )
                .unwrap(),
            ),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: starts.clone(),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language_fiber = stack
        .activate_language("test.language.retry", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-retry").await;
    let (submitted, outcome) = stack.submit_and_wait("retry safely").await;
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .map(|fact| fact.body().clone())
        .collect();
    let start_count = starts.load(Ordering::Acquire);

    stack.dispose(language_fiber, executor_fiber).await;
    (outcome, facts, start_count)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_only_a_policy_admitted_proven_undispatched_model_attempt() {
    let (outcome, facts, starts) = run_retry_case(DispatchStatus::NotDispatched).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(starts, 2);
    assert_eq!(
        facts
            .iter()
            .filter(|body| matches!(body, SessionFactBody::ModelIntent { .. }))
            .count(),
        2
    );
    assert!(facts.iter().any(|body| matches!(
        body,
        SessionFactBody::ModelEvent {
            event: LanguageEvent::Failed { error, .. },
            ..
        } if error.dispatch_status() == DispatchStatus::NotDispatched
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_retries_a_dispatch_uncertain_model_attempt() {
    let (outcome, facts, starts) = run_retry_case(DispatchStatus::Unknown).await;
    assert!(matches!(
        outcome,
        TurnOutcome::Interrupted {
            effect: Some(rsi_agent_session_protocol::EffectKind::Model),
            ..
        }
    ));
    assert_eq!(starts, 1);
    assert_eq!(
        facts
            .iter()
            .filter(|body| matches!(body, SessionFactBody::ModelIntent { .. }))
            .count(),
        1
    );
}

fn partial_image_script() -> Vec<std::result::Result<ImageEvent, AiError>> {
    vec![
        Ok(ImageEvent::OutputStarted {
            index: 0,
            mime_type: "image/png".into(),
        }),
        Ok(ImageEvent::OutputChunk {
            index: 0,
            sequence: 1,
            bytes: vec![1, 2, 3],
        }),
        Ok(ImageEvent::OutputFinished { index: 0 }),
        Ok(ImageEvent::OutputStarted {
            index: 1,
            mime_type: "image/png".into(),
        }),
        Ok(ImageEvent::OutputChunk {
            index: 1,
            sequence: 1,
            bytes: vec![4, 5, 6],
        }),
        Ok(ImageEvent::OutputFinished { index: 1 }),
        Err(AiError::new(
            ErrorKind::OutputValidation,
            ErrorPhase::Assemble,
            DispatchStatus::Dispatched,
            "third image failed validation",
        )
        .unwrap()),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_turn_flushes_each_media_ref_and_preserves_partial_failure() {
    let stack = BaseStack::activate().await;
    stack
        .image
        .events
        .lock()
        .unwrap()
        .push_back(partial_image_script());
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.unused", language)
        .await;
    let executor_fiber = stack.activate_executor("executor-image").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit_image(SubmitImage {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            model: ModelRef::new("deployment", "image-model").unwrap(),
            request: ImageRequest::new("draw three tiles", 3).unwrap(),
        })
        .await
        .unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let TurnOutcome::PartialFailed {
        media,
        code,
        message,
    } = outcome
    else {
        panic!("expected partial Image failure")
    };
    assert_eq!(media.len(), 2);
    assert_eq!(code, ErrorKind::OutputValidation.code());
    assert_eq!(message, "third image failed validation");
    assert_eq!(stack.media.imports.load(Ordering::Acquire), 2);

    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap();
    let outputs = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ImageOutput { index, media, .. } => Some((*index, media.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].0, 0);
    assert_eq!(outputs[1].0, 1);
    assert_eq!(outputs[0].1, media[0]);
    assert_eq!(outputs[1].1, media[1]);
    let encoded = serde_json::to_string(&page.facts).unwrap();
    assert!(!encoded.contains("[1,2,3]"));
    assert!(matches!(
        page.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::PartialFailed { media, .. },
            ..
        } if media.len() == 2
    ));

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_shutdown_releases_a_claimed_nonterminal_turn_without_reclaiming_it() {
    let stack = BaseStack::activate().await;
    let waiting_after_first = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::GatedStream {
            events: answer_script(),
            waiting_after_first: Arc::clone(&waiting_after_first),
            release: Arc::new(Notify::new()),
        }])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.shutdown", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-shutdown").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "remain nonterminal".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        waiting_after_first.notified(),
    )
    .await
    .expect("executor did not claim and start the turn");

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), executor_fiber.dispose())
        .await
        .expect("executor shutdown must not reclaim the stopped turn");
    assert!(report.is_clean(), "{report:?}");

    drop(turns);
    stack.dispose(language_fiber, executor_fiber).await;
}
