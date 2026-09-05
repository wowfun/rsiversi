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
    AgentPresetId, BudgetDimension, FrozenAgentSettings, SessionFact, SessionFactBody,
    SessionHeader, SessionId, TurnBudget, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::{SessionStore, StoredContextCheckpoint, WriteContextCheckpoint};
use rsi_agent_testkit::{MemoryStore, MemoryStoreFactory};
use rsi_agent_turn_protocol::{
    ClaimFactPage, ContextCheckpoint, ExecutorLease, FinalizationResult, PublishAttempt,
    SubmitImage, SubmitSession, SubmitTurn, SubmittedTurn, TurnClaim, TurnClaimIssuer,
    TurnCompletionBlocker, TurnError, TurnExecution, TurnExecutionContract, TurnFinalization,
    TurnFinalizationContext, TurnFinalizationContract, TurnFinalizationError,
    TurnFinalizationReport, TurnFinalizer, TurnFinalizerLease, TurnServiceContract,
};
use rsi_agent_workspace_context::{
    WorkspaceContext, WorkspaceContextContract, WorkspaceContextError, WorkspaceContextSnapshot,
};
use rsi_ai_protocol::{
    AiCapability, AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase,
    FinishReason, ImageCall, ImageCallContract, ImageEvent, ImageRequest, ImageStream,
    ImageToolResultCapability, LanguageCall, LanguageCallContract, LanguageEvent, LanguageProfile,
    LanguageRequest, LanguageStream, MessageContent, MessageRole, ModelRef, PreparedCallSnapshot,
    PreparedImageCall, PreparedLanguageCall, RetryPolicy, ToolCallKind, ToolDialect,
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
    ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolLaneParkingAuthority,
    ToolRegistrar, ToolRegistration, ToolResult, ToolRuntime, ToolScheduling,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use tokio::sync::{Barrier, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

fn client_turn_id() -> TurnId {
    static NEXT_CLIENT_TURN: AtomicUsize = AtomicUsize::new(1);
    TurnId::new(format!(
        "caller-turn-{}",
        NEXT_CLIENT_TURN.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap()
}

async fn activate_fixture(
    runtime: &Runtime,
    plugin: &str,
    revision: &str,
    factory: Arc<dyn PluginFactory>,
) -> FiberHandle {
    activate_configured_fixture(runtime, plugin, revision, factory, Value::Null).await
}

async fn activate_configured_fixture(
    runtime: &Runtime,
    plugin: &str,
    revision: &str,
    factory: Arc<dyn PluginFactory>,
    config: Value,
) -> FiberHandle {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(plugin, revision, UpdateMode::Replayable, factory),
            config,
        )
        .await
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

#[derive(Debug)]
struct EmptyWorkspaceContextFixture;

#[async_trait]
impl WorkspaceContext for EmptyWorkspaceContextFixture {
    async fn snapshot(
        &self,
        _header: &SessionHeader,
        _messages: &[&rsi_agent_session_protocol::AgentMessage],
    ) -> std::result::Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        Ok(WorkspaceContextSnapshot {
            complete: false,
            instructions_sha256: String::new(),
            instructions: None,
            skill_catalog_sha256: String::new(),
            skill_catalog: None,
            invocations: Vec::new(),
        })
    }
}

#[derive(Debug)]
struct WorkspaceContextFixtureFactory;

#[async_trait]
impl PluginFactory for WorkspaceContextFixtureFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service: Arc<dyn WorkspaceContext> = Arc::new(EmptyWorkspaceContextFixture);
        let supply = plan
            .context()
            .provide_local::<WorkspaceContextContract>(service)?;
        plan.defer(
            "withdraw test workspace context",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

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
struct FailingClaimFixture {
    claims: AtomicUsize,
    claim: Mutex<Option<TurnClaim>>,
    sibling_started: Arc<Notify>,
    sibling_cancelled: Arc<Notify>,
    sibling_release: Arc<(Mutex<bool>, Condvar)>,
    lease_dropped: Arc<AtomicBool>,
}

#[async_trait]
impl TurnExecution for FailingClaimFixture {
    fn register(&self, _executor_id: String) -> rsi_agent_turn_protocol::Result<ExecutorLease> {
        let lease_dropped = Arc::clone(&self.lease_dropped);
        Ok(ExecutorLease::new(move || {
            lease_dropped.store(true, Ordering::Release);
        }))
    }

    async fn claim(
        &self,
        _executor_id: &str,
        cancellation: CancellationToken,
    ) -> rsi_agent_turn_protocol::Result<Option<TurnClaim>> {
        let lane = self.claims.fetch_add(1, Ordering::AcqRel);
        assert!(
            lane < 2,
            "the failing pool must stop after its first claim error"
        );
        if lane == 1 {
            self.sibling_started.notified().await;
            return Err(TurnError::StaleClaim);
        }
        let sibling_cancelled = Arc::clone(&self.sibling_cancelled);
        tokio::spawn(async move {
            cancellation.cancelled().await;
            sibling_cancelled.notify_one();
        });
        self.sibling_started.notify_one();
        Ok(self.claim.lock().unwrap().take())
    }

    fn composition(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<AgentCompositionPin> {
        self.sibling_started.notify_one();
        let (released, changed) = &*self.sibling_release;
        let released = released.lock().unwrap();
        let _released = changed.wait_while(released, |released| !*released).unwrap();
        Err(TurnError::StaleClaim)
    }

    async fn read_fork_facts(
        &self,
        _claim: &TurnClaim,
        _after_parent_seq: u64,
        _limit: usize,
    ) -> rsi_agent_turn_protocol::Result<Option<rsi_agent_turn_protocol::ForkFactPage>> {
        unreachable!("the failing claim fixture never reads fork Facts")
    }

    async fn enter_pending_step_messages(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<usize> {
        unreachable!("the failing claim fixture never enters messages")
    }

    async fn refresh_workspace_context(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<usize> {
        unreachable!("the failing claim fixture never refreshes workspace context")
    }

    async fn close_current_step(
        &self,
        _claim: &TurnClaim,
        _outcome: &TurnOutcome,
    ) -> rsi_agent_turn_protocol::Result<()> {
        unreachable!("the failing claim fixture never closes a Step")
    }

    async fn finish_activation_turn(
        &self,
        _claim: &TurnClaim,
        _outcome: &TurnOutcome,
    ) -> rsi_agent_turn_protocol::Result<Option<Arc<SessionFact>>> {
        unreachable!("the failing claim fixture never settles an activation")
    }

    async fn read_facts(
        &self,
        _claim: &TurnClaim,
        _after_seq: u64,
        _limit: usize,
    ) -> rsi_agent_turn_protocol::Result<ClaimFactPage> {
        unreachable!("the failing claim fixture never yields a claim")
    }

    async fn publish(
        &self,
        _claim: &TurnClaim,
        _bodies: Vec<SessionFactBody>,
    ) -> rsi_agent_turn_protocol::Result<PublishAttempt> {
        Err(TurnError::StaleClaim)
    }

    async fn flush(
        &self,
        _claim: &TurnClaim,
        _through_seq: u64,
    ) -> rsi_agent_turn_protocol::Result<u64> {
        unreachable!("the failing claim fixture never yields a claim")
    }

    fn cancellation(
        &self,
        _claim: &TurnClaim,
    ) -> rsi_agent_turn_protocol::Result<CancellationToken> {
        unreachable!("the failing claim fixture never yields a claim")
    }

    fn release(&self, _claim: &TurnClaim) -> rsi_agent_turn_protocol::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl TurnFinalization for FailingClaimFixture {
    fn register(
        &self,
        _name: String,
        _finalizer: Arc<dyn TurnFinalizer>,
    ) -> FinalizationResult<TurnFinalizerLease> {
        unreachable!("the failing claim fixture never finalizes a turn")
    }

    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport> {
        Ok(TurnFinalizationReport::complete())
    }
}

#[derive(Clone, Debug)]
struct FailingClaimFixtureFactory {
    fixture: Arc<FailingClaimFixture>,
}

#[async_trait]
impl PluginFactory for FailingClaimFixtureFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let turns: Arc<dyn TurnExecution> = self.fixture.clone();
        let finalization: Arc<dyn TurnFinalization> = self.fixture.clone();
        let turns_supply = plan
            .context()
            .provide_local::<TurnExecutionContract>(turns)?;
        let finalization_supply = plan
            .context()
            .provide_local::<TurnFinalizationContract>(finalization)?;
        plan.defer(
            "withdraw failing Turn fixtures",
            Box::new(move || {
                Box::pin(async move {
                    drop(finalization_supply);
                    drop(turns_supply);
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
    expected_turn_text: String,
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

fn gated_answer(waiting_after_first: &Arc<Notify>, release: &Arc<Notify>) -> StartOutcome {
    StartOutcome::GatedStream {
        events: answer_script(),
        waiting_after_first: Arc::clone(waiting_after_first),
        release: Arc::clone(release),
    }
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
        assert_turn_session_latest_is(&self.store, &self.expected_turn_text, |body| {
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
        let expected_turn_text = request
            .messages()
            .iter()
            .rev()
            .find(|message| message.role() == MessageRole::User)
            .and_then(|message| {
                message.content().iter().find_map(|content| match content {
                    MessageContent::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .expect("fixture request has a user text message");
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
            expected_turn_text,
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
struct ParallelTool {
    rendezvous: Arc<Barrier>,
    release_first: Arc<Notify>,
    observed_lane_parking_authority: Arc<AtomicBool>,
}

#[derive(Debug)]
struct UnevenParallelResultTool;

#[derive(Debug)]
struct ParkingTool {
    parked: Arc<Notify>,
    release: Arc<Notify>,
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
impl ToolExecutor for ParallelTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        self.observed_lane_parking_authority.fetch_or(
            execution.extension::<ToolLaneParkingAuthority>().is_some(),
            Ordering::AcqRel,
        );
        self.rendezvous.wait().await;
        if arguments.get("position") == Some(&Value::String("first".into())) {
            self.release_first.notified().await;
        } else {
            self.release_first.notify_one();
        }
        ToolResult::new(arguments, vec![], false)
    }
}

#[async_trait]
impl ToolExecutor for UnevenParallelResultTool {
    async fn execute(
        &self,
        arguments: Value,
        _execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        let value = if arguments.get("position") == Some(&Value::String("first".into())) {
            json!({"position":"first","payload":"x".repeat(32 * 1024)})
        } else {
            arguments
        };
        ToolResult::new(value, vec![], false)
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

#[async_trait]
impl ToolExecutor for ParkingTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        let parking = execution
            .extension::<ToolLaneParkingAuthority>()
            .ok_or_else(|| {
                ToolError::Execution("executor supplied no lane-parking authority".into())
            })?;
        let parked = parking.park().await?;
        self.parked.notify_one();
        self.release.notified().await;
        parked.resume(execution.cancellation.clone()).await?;
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

async fn assert_turn_session_latest_is(
    store: &MemoryStore,
    expected_turn_text: &str,
    predicate: impl Fn(&SessionFactBody) -> bool,
) {
    let sessions = store.list_sessions(None, 64).await.unwrap().sessions;
    for session in sessions {
        let page = store.read_facts(&session, 0, 64).await.unwrap();
        let matches_turn = page.facts.iter().any(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::TurnAccepted { text, .. } if text == expected_turn_text
            )
        });
        if matches_turn {
            assert!(page.facts.last().is_some_and(|fact| predicate(fact.body())));
            return;
        }
    }
    panic!("no Session contains the expected accepted turn");
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

fn tool_calls_script(calls: &[(&str, &str, &str)]) -> Vec<LanguageEvent> {
    let mut events = Vec::with_capacity(calls.len().saturating_mul(3).saturating_add(1));
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        events.push(LanguageEvent::ContentStarted {
            index,
            content: ContentStart::ToolCall {
                id: (*id).into(),
                name: (*name).into(),
                kind: ToolCallKind::Function,
            },
        });
        events.push(LanguageEvent::ContentDelta {
            index,
            delta: ContentDelta::ToolArguments((*arguments).into()),
        });
        events.push(LanguageEvent::ContentFinished { index });
    }
    events.push(LanguageEvent::Finished {
        reason: FinishReason::ToolCalls,
        replay: None,
    });
    events
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
    header_for_session("session-e2e", turn_budget)
}

fn header_for_session(session: &str, turn_budget: TurnBudget) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session).unwrap(),
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

async fn wait_for_outcome(
    turns: &Arc<dyn rsi_agent_turn_protocol::TurnService>,
    submitted: &SubmittedTurn,
) -> TurnOutcome {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
    .expect("executor did not publish a terminal outcome")
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
    panic_on_commit: Mutex<Option<Arc<Notify>>>,
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
        let tools: Arc<dyn ToolRuntime> = match self.panic_on_commit.lock().unwrap().take() {
            Some(panicked) => Arc::new(pool::PanicCommitTools {
                inner: tools,
                panicked,
            }),
            None => tools,
        };
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
            panic_on_commit: Mutex::new(None),
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
    _test_admission: OwnedSemaphorePermit,
    runtime: Runtime,
    store: Arc<MemoryStore>,
    image: Arc<ImageFixture>,
    media: Arc<MediaFixture>,
    store_fiber: FiberHandle,
    kernel_fiber: FiberHandle,
    tools_fiber: FiberHandle,
    composition_fiber: FiberHandle,
    workspace_context_fiber: FiberHandle,
    composition: Arc<CompositionFixture>,
    tool_registrar: Arc<dyn ToolRegistrar>,
    jobs_fiber: FiberHandle,
    security_fiber: FiberHandle,
    image_media_fiber: FiberHandle,
}

impl BaseStack {
    #[allow(clippy::too_many_lines)] // The fixture keeps the complete dependency-order activation visible.
    async fn activate() -> Self {
        static TEST_STACK_ADMISSION: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let test_admission =
            Arc::clone(TEST_STACK_ADMISSION.get_or_init(|| Arc::new(Semaphore::new(8))))
                .acquire_owned()
                .await
                .expect("executor end-to-end test admission remains open");
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
        let workspace_context_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "test.workspace-context",
                    "workspace-context",
                    UpdateMode::Replayable,
                    Arc::new(WorkspaceContextFixtureFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
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
            _test_admission: test_admission,
            runtime,
            store,
            image,
            media,
            store_fiber,
            kernel_fiber,
            tools_fiber,
            composition_fiber,
            workspace_context_fiber,
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

    async fn submit_fresh(
        &self,
        turns: &Arc<dyn rsi_agent_turn_protocol::TurnService>,
        session: &str,
        text: &str,
    ) -> SubmittedTurn {
        turns
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: self
                    .fresh(header_for_session(session, TurnBudget::default()))
                    .await,
                text: text.into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap()
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
        self.dispose_services().await;
    }

    async fn dispose_services(self) {
        assert!(self.image_media_fiber.dispose().await.is_clean());
        assert!(self.security_fiber.dispose().await.is_clean());
        assert!(self.jobs_fiber.dispose().await.is_clean());
        assert!(self.kernel_fiber.dispose().await.is_clean());
        assert!(self.workspace_context_fiber.dispose().await.is_clean());
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

#[path = "end_to_end/budgets_and_recovery.rs"]
mod budgets_and_recovery;
#[path = "end_to_end/image_and_shutdown.rs"]
mod image_and_shutdown;
#[path = "end_to_end/pool.rs"]
mod pool;
#[path = "end_to_end/tool_execution.rs"]
mod tool_execution;
