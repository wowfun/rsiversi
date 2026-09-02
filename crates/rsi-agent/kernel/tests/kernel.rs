use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentCompositionError, AgentCompositionPin,
    AgentSessionDraft, PreparedFreshSession,
};
use rsi_agent_kernel::{
    Clock, DEFAULT_MAXIMUM_ACTIVE_OBSERVERS, KernelFactory, KernelLimits, MAXIMUM_ACTIVE_SESSIONS,
    MAXIMUM_PENDING_FACT_BYTES, SessionKernel,
};
use rsi_agent_session_protocol::{
    AgentPresetId, BudgetDimension, EffectId, EffectKind, FrozenAgentSettings,
    MAXIMUM_AGENT_DIAGNOSTIC_BYTES, MAXIMUM_FACTS_PER_READ, MAXIMUM_SESSION_FACT_BYTES,
    MAXIMUM_TURN_GENERATED_FACT_BYTES, MAXIMUM_TURN_TEXT_BYTES, SessionFact, SessionFactBody,
    SessionHeader, SessionId, TurnBudget, TurnId, TurnOutcome, fact_prefix_sha256,
};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, CasObjectRef, SessionStore, StoreError, StoreFactPage,
    StoreOpenTurnPage, StoreSessionPage, StoreTurnBoundary, StoreTurnFactPage,
    StoredContextCheckpoint, WriteContextCheckpoint,
};
use rsi_agent_testkit::{MemoryStore, MemoryStoreFactory};
use rsi_agent_turn_protocol::{
    ContextCheckpoint, PublishAttempt, SubmitSession, SubmitTurn, TurnClaimIssuer, TurnError,
    TurnExecution, TurnFinalizationContext, TurnFinalizationError, TurnFinalizationReport,
    TurnFinalizer, TurnService, TurnUpdate,
};
use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, LanguageEvent, MAX_LANGUAGE_OUTPUT_BYTES, ModelRef,
    PreparedCallSnapshot, RetryPolicy,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError, ToolResultIdentity,
    ToolRuntime,
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

trait PublishedFacts {
    fn published(self) -> Vec<Arc<SessionFact>>;
}

impl PublishedFacts for PublishAttempt {
    fn published(self) -> Vec<Arc<SessionFact>> {
        match self {
            PublishAttempt::Published(facts) => facts,
            PublishAttempt::FlushRequired { .. } => panic!("expected published Facts"),
        }
    }
}

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        42
    }
}

#[derive(Debug)]
struct FactReadRaceStore {
    inner: Arc<MemoryStore>,
    block_header_reads: AtomicBool,
    blocked_header_session: Mutex<Option<SessionId>>,
    header_read_attempts: AtomicUsize,
    release_header_reads: Notify,
    pause_read: AtomicBool,
    read_attempts: AtomicUsize,
    read_captured: Notify,
    release_read: Notify,
    read_error: Mutex<Option<String>>,
    pause_open_turn_read: AtomicBool,
    open_turn_read_attempts: AtomicUsize,
    open_turn_read_captured: Notify,
    release_open_turn_read: Notify,
    block_turn_boundary_reads: AtomicBool,
    turn_boundary_read_attempts: AtomicUsize,
    turn_boundary_read_started: Notify,
    release_turn_boundary_read: Notify,
    append_attempts: AtomicUsize,
    pause_append_at: AtomicUsize,
    append_blocked: Notify,
    release_append: Notify,
    permanent_append_failure_for: Mutex<Option<SessionId>>,
    fail_checkpoint_write: AtomicBool,
}

impl FactReadRaceStore {
    fn new(inner: Arc<MemoryStore>) -> Self {
        Self {
            inner,
            block_header_reads: AtomicBool::new(false),
            blocked_header_session: Mutex::new(None),
            header_read_attempts: AtomicUsize::new(0),
            release_header_reads: Notify::new(),
            pause_read: AtomicBool::new(false),
            read_attempts: AtomicUsize::new(0),
            read_captured: Notify::new(),
            release_read: Notify::new(),
            read_error: Mutex::new(None),
            pause_open_turn_read: AtomicBool::new(false),
            open_turn_read_attempts: AtomicUsize::new(0),
            open_turn_read_captured: Notify::new(),
            release_open_turn_read: Notify::new(),
            block_turn_boundary_reads: AtomicBool::new(false),
            turn_boundary_read_attempts: AtomicUsize::new(0),
            turn_boundary_read_started: Notify::new(),
            release_turn_boundary_read: Notify::new(),
            append_attempts: AtomicUsize::new(0),
            pause_append_at: AtomicUsize::new(0),
            append_blocked: Notify::new(),
            release_append: Notify::new(),
            permanent_append_failure_for: Mutex::new(None),
            fail_checkpoint_write: AtomicBool::new(false),
        }
    }

    fn block_header_reads(&self) {
        self.block_header_reads.store(true, Ordering::Release);
    }

    fn block_header_reads_for(&self, session_id: SessionId) {
        *self
            .blocked_header_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);
        self.block_header_reads.store(true, Ordering::Release);
    }

    fn header_read_attempts(&self) -> usize {
        self.header_read_attempts.load(Ordering::Acquire)
    }

    fn reset_header_read_attempts(&self) {
        self.header_read_attempts.store(0, Ordering::Release);
    }

    fn release_blocked_header_reads(&self) {
        self.block_header_reads.store(false, Ordering::Release);
        self.blocked_header_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.release_header_reads.notify_waiters();
    }

    fn pause_next_read(&self) {
        self.pause_read.store(true, Ordering::Release);
    }

    fn reset_read_attempts(&self) {
        self.read_attempts.store(0, Ordering::Release);
    }

    fn read_attempts(&self) -> usize {
        self.read_attempts.load(Ordering::Acquire)
    }

    fn pause_next_open_turn_read(&self) {
        self.pause_open_turn_read.store(true, Ordering::Release);
    }

    fn reset_open_turn_read_attempts(&self) {
        self.open_turn_read_attempts.store(0, Ordering::Release);
    }

    fn open_turn_read_attempts(&self) -> usize {
        self.open_turn_read_attempts.load(Ordering::Acquire)
    }

    async fn wait_until_open_turn_read_is_captured(&self) {
        self.open_turn_read_captured.notified().await;
    }

    fn release_captured_open_turn_read(&self) {
        self.release_open_turn_read.notify_one();
    }

    async fn wait_until_read_is_captured(&self) {
        self.read_captured.notified().await;
    }

    fn release_captured_read(&self) {
        self.release_read.notify_one();
    }

    fn fail_next_read(&self, message: String) {
        *self
            .read_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
    }

    fn pause_second_following_append(&self) {
        let attempt = self.append_attempts.load(Ordering::Acquire) + 2;
        self.pause_append_at.store(attempt, Ordering::Release);
    }

    fn pause_next_append(&self) {
        let attempt = self.append_attempts.load(Ordering::Acquire) + 1;
        self.pause_append_at.store(attempt, Ordering::Release);
    }

    async fn wait_until_append_is_blocked(&self) {
        self.append_blocked.notified().await;
    }

    fn release_blocked_append(&self) {
        self.release_append.notify_one();
    }

    fn fail_appends_for(&self, session_id: SessionId) {
        *self
            .permanent_append_failure_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);
    }

    fn fail_next_checkpoint_write(&self) {
        self.fail_checkpoint_write.store(true, Ordering::Release);
    }

    fn block_turn_boundary_reads(&self) {
        self.block_turn_boundary_reads
            .store(true, Ordering::Release);
    }

    async fn wait_for_turn_boundary_attempts(&self, expected: usize) {
        while self.turn_boundary_read_attempts.load(Ordering::Acquire) < expected {
            self.turn_boundary_read_started.notified().await;
        }
    }

    fn turn_boundary_read_attempts(&self) -> usize {
        self.turn_boundary_read_attempts.load(Ordering::Acquire)
    }

    fn release_one_turn_boundary_read(&self) {
        self.release_turn_boundary_read.notify_one();
    }
}

#[async_trait]
impl SessionStore for FactReadRaceStore {
    async fn append(&self, batch: AppendBatch) -> rsi_agent_store_protocol::Result<AppendCommit> {
        let attempt = self.append_attempts.fetch_add(1, Ordering::AcqRel) + 1;
        if self.pause_append_at.load(Ordering::Acquire) == attempt {
            self.append_blocked.notify_one();
            self.release_append.notified().await;
        }
        if self
            .permanent_append_failure_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            == Some(&batch.session_id)
        {
            return Err(StoreError::Invalid(
                "injected permanent append failure".into(),
            ));
        }
        self.inner.append(batch).await
    }

    async fn header(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<SessionHeader> {
        self.header_read_attempts.fetch_add(1, Ordering::AcqRel);
        let released = self.release_header_reads.notified();
        let blocked_session = self
            .blocked_header_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if self.block_header_reads.load(Ordering::Acquire)
            && blocked_session
                .as_ref()
                .is_none_or(|blocked| blocked == session_id)
        {
            released.await;
        }
        self.inner.header(session_id).await
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreFactPage> {
        self.read_attempts.fetch_add(1, Ordering::AcqRel);
        if let Some(message) = self
            .read_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return Err(StoreError::Io(message));
        }
        let page = self.inner.read_facts(session_id, after_seq, limit).await?;
        if self.pause_read.swap(false, Ordering::AcqRel) {
            self.read_captured.notify_one();
            self.release_read.notified().await;
        }
        Ok(page)
    }

    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreBackwardFactPage> {
        self.inner
            .read_facts_before(session_id, exclusive_before_seq, limit)
            .await
    }

    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreTurnFactPage> {
        self.inner
            .read_turn_facts(session_id, turn_id, after_seq, limit)
            .await
    }

    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> rsi_agent_store_protocol::Result<StoreTurnBoundary> {
        self.turn_boundary_read_attempts
            .fetch_add(1, Ordering::AcqRel);
        self.turn_boundary_read_started.notify_waiters();
        if self.block_turn_boundary_reads.load(Ordering::Acquire) {
            self.release_turn_boundary_read.notified().await;
        }
        self.inner.read_turn_boundary(session_id, turn_id).await
    }

    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreOpenTurnPage> {
        self.open_turn_read_attempts.fetch_add(1, Ordering::AcqRel);
        let page = self
            .inner
            .list_open_turns(session_id, after_accepted_seq, limit)
            .await?;
        if self.pause_open_turn_read.swap(false, Ordering::AcqRel) {
            self.open_turn_read_captured.notify_one();
            self.release_open_turn_read.notified().await;
        }
        Ok(page)
    }

    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreSessionPage> {
        self.inner.list_sessions(after, limit).await
    }

    async fn list_recent_sessions(
        &self,
        after: Option<&rsi_agent_store_protocol::StoreRecentSessionCursor>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreRecentSessionPage> {
        self.inner.list_recent_sessions(after, limit).await
    }

    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreSessionPage> {
        self.inner.list_open_sessions(after, limit).await
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<Option<StoredContextCheckpoint>> {
        self.inner.read_context_checkpoint(session_id).await
    }

    async fn write_context_checkpoint(
        &self,
        write: WriteContextCheckpoint,
    ) -> rsi_agent_store_protocol::Result<()> {
        if self.fail_checkpoint_write.swap(false, Ordering::AcqRel) {
            return Err(StoreError::Io("injected checkpoint write failure".into()));
        }
        self.inner.write_context_checkpoint(write).await
    }

    async fn put_cas(&self, bytes: Arc<[u8]>) -> rsi_agent_store_protocol::Result<CasObjectRef> {
        self.inner.put_cas(bytes).await
    }

    async fn read_cas(&self, object: &CasObjectRef) -> rsi_agent_store_protocol::Result<Arc<[u8]>> {
        self.inner.read_cas(object).await
    }
}

#[derive(Debug)]
struct RecordingFinalizer {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
}

#[derive(Debug)]
struct CoordinatedFinalizer {
    entered: Arc<AtomicUsize>,
    entered_changed: Arc<Notify>,
    release: Arc<Notify>,
    fail: bool,
}

#[async_trait]
impl TurnFinalizer for CoordinatedFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.entered_changed.notify_waiters();
        self.release.notified().await;
        if self.fail {
            Err(TurnFinalizationError::Failed {
                code: "test.concurrent_failure".into(),
                message: "concurrent finalizer failed".into(),
            })
        } else {
            Ok(TurnFinalizationReport::complete())
        }
    }
}

#[derive(Debug)]
struct PanickingFinalizer;

#[async_trait]
impl TurnFinalizer for PanickingFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        panic!("test finalizer panic")
    }
}

#[async_trait]
impl TurnFinalizer for RecordingFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        self.calls.lock().unwrap().push(self.name);
        if self.fail {
            return Err(TurnFinalizationError::Failed {
                code: "test.failed".into(),
                message: "test finalizer failed".into(),
            });
        }
        Ok(TurnFinalizationReport::complete())
    }
}

fn client_turn_id() -> TurnId {
    static NEXT_CLIENT_TURN: AtomicUsize = AtomicUsize::new(1);
    TurnId::new(format!(
        "caller-turn-{}",
        NEXT_CLIENT_TURN.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap()
}

#[derive(Debug)]
struct EmptyTools;

#[derive(Debug)]
struct DropOwner(Arc<AtomicUsize>);

impl Drop for DropOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl ToolRuntime for EmptyTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn prepare(
        &self,
        _invocation_id: &str,
        _call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }

    fn query(
        &self,
        _identity: &ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }

    async fn wait(
        &self,
        _identity: &ToolResultIdentity,
        _cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }

    fn commit(&self, _identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }
}

#[derive(Debug)]
struct TestComposition;

fn test_pin(preset_id: &AgentPresetId) -> AgentCompositionPin {
    test_pin_with_digest(preset_id, 'a')
}

fn test_pin_with_digest(preset_id: &AgentPresetId, digit: char) -> AgentCompositionPin {
    AgentCompositionPin::new(
        preset_id.clone(),
        digit.to_string().repeat(64),
        Arc::new(EmptyTools),
        Arc::new(()),
    )
    .unwrap()
}

#[derive(Debug)]
struct MutableComposition {
    digest_digit: Mutex<char>,
    unavailable: AtomicBool,
    calls: AtomicUsize,
}

#[derive(Debug)]
struct DropTrackingComposition {
    calls: AtomicUsize,
    drops: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct UnboundedDiagnosticComposition;

#[async_trait]
impl AgentComposition for UnboundedDiagnosticComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        Err(AgentCompositionError::Unavailable {
            preset_id: preset_id.clone(),
            reason: "界".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES),
        })
    }
}

#[async_trait]
impl AgentComposition for DropTrackingComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        AgentCompositionPin::new(
            preset_id.clone(),
            "a".repeat(64),
            Arc::new(EmptyTools),
            Arc::new(DropOwner(Arc::clone(&self.drops))),
        )
    }
}

impl MutableComposition {
    fn new(digest_digit: char) -> Self {
        Self {
            digest_digit: Mutex::new(digest_digit),
            unavailable: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }

    fn select_digest(&self, digest_digit: char) {
        *self.digest_digit.lock().unwrap() = digest_digit;
    }

    fn set_unavailable(&self) {
        self.unavailable.store(true, Ordering::Release);
    }
}

#[async_trait]
impl AgentComposition for MutableComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.unavailable.load(Ordering::Acquire) {
            return Err(AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: "test preset source is unavailable".into(),
            });
        }
        Ok(test_pin_with_digest(
            preset_id,
            *self.digest_digit.lock().unwrap(),
        ))
    }
}

#[async_trait]
impl AgentComposition for TestComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        Ok(test_pin(preset_id))
    }
}

fn composition() -> Arc<dyn AgentComposition> {
    Arc::new(TestComposition)
}

#[derive(Clone, Debug)]
struct TestCompositionFactory;

#[async_trait]
impl PluginFactory for TestCompositionFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<AgentCompositionContract>(composition())?;
        plan.defer(
            "withdraw test Agent composition",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn fresh(header: SessionHeader) -> SubmitSession {
    let pin = test_pin(header.agent_preset_id());
    SubmitSession::Fresh(PreparedFreshSession::new(header, pin).unwrap())
}

async fn resume(kernel: &SessionKernel, session_id: SessionId) -> SubmitSession {
    SubmitSession::Resume(kernel.prepare_resume(&session_id).await.unwrap())
}

fn profile() -> FrozenAgentSettings {
    FrozenAgentSettings::new(
        "default",
        "system",
        ModelRef::new("deployment", "model").unwrap(),
        SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap()
}

fn header(session_id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session_id).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        profile(),
    )
    .unwrap()
}

fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "call-1".into(),
        deployment_id: "deployment".into(),
        provider_family: "test".into(),
        capability: AiCapability::Language,
        model: "model".into(),
        protocol: "test".into(),
        transport: "memory".into(),
        endpoint_fingerprint: "endpoint".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
    }
}

fn accepted_fact(seq: u64, turn_id: &TurnId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

fn model_intent_fact(seq: u64, turn_id: &TurnId, effect_id: &EffectId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelIntent {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
            snapshot: snapshot(),
        },
    )
    .unwrap()
}

fn model_started_fact(seq: u64, turn_id: &TurnId, effect_id: &EffectId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelStarted {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
        },
    )
    .unwrap()
}

fn model_finished_fact(seq: u64, turn_id: &TurnId, effect_id: &EffectId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelEvent {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
            event: LanguageEvent::Finished {
                reason: rsi_ai_protocol::FinishReason::Stop,
                replay: None,
            },
        },
    )
    .unwrap()
}

fn budget_fact(
    seq: u64,
    turn_id: &TurnId,
    dimension: BudgetDimension,
    consumed: u64,
    limit: u64,
) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::BudgetExhausted {
            turn_id: turn_id.clone(),
            dimension,
            consumed,
            limit,
        },
    )
    .unwrap()
}

async fn kernel(store: Arc<MemoryStore>) -> SessionKernel {
    let store: Arc<dyn SessionStore> = store;
    SessionKernel::recover_with_clock(store, composition(), Arc::new(FixedClock))
        .await
        .unwrap()
}

async fn append_terminal_history(store: &MemoryStore, session_id: &str, turns: usize) {
    let session_id = SessionId::new(session_id).unwrap();
    let mut facts = Vec::with_capacity(turns * 2);
    for index in 0..turns {
        let turn_id = TurnId::new(format!("turn-history-{index}")).unwrap();
        let accepted_seq = u64::try_from(index * 2 + 1).unwrap();
        facts.push(
            SessionFact::new(
                accepted_seq,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id: turn_id.clone(),
                    text: "done".into(),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        facts.push(
            SessionFact::new(
                accepted_seq + 1,
                1,
                SessionFactBody::TurnTerminal {
                    turn_id,
                    outcome: TurnOutcome::Completed,
                },
            )
            .unwrap(),
        );
    }
    let mut expected_seq = 0;
    for (batch_index, batch_facts) in facts.chunks(512).enumerate() {
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq,
                header: (batch_index == 0).then(|| header(session_id.as_str())),
                facts: batch_facts.to_vec(),
            })
            .await
            .unwrap();
        expected_seq = batch_facts.last().unwrap().seq();
    }
}

async fn submit(
    kernel: &SessionKernel,
    session_id: &str,
    text: &str,
) -> rsi_agent_turn_protocol::SubmittedTurn {
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header(session_id)),
            text: text.into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn fresh_submission_returns_only_after_its_acceptance_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-lazy", "hello").await;
    let session = submitted.session_id.clone();
    assert_eq!(
        store.header(&session).await.unwrap(),
        header("session-lazy")
    );

    let mut observation = kernel.observe(&session, 0).await.unwrap();
    assert!(matches!(
        observation.next().await.unwrap().unwrap(),
        TurnUpdate::Fact { durable_seq: 1, .. }
    ));
    assert_eq!(
        store.read_facts(&session, 0, 8).await.unwrap().durable_seq,
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn submission_without_a_running_write_behind_worker_fails_within_a_bound() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(61),
        kernel.submit(SubmitTurn {
            turn_id: TurnId::new("turn-no-worker").unwrap(),
            session: fresh(header("session-no-worker")),
            text: "must not wait forever".into(),
            model: None,
            sandbox: None,
        }),
    )
    .await
    .expect("the Kernel must bound a durability wait without its worker");
    assert!(
        matches!(result, Err(TurnError::Flush(ref message)) if message.contains("timed out")),
        "unexpected result: {result:?}"
    );

    let worker = kernel.start_write_behind();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn blocked_retry_does_not_serialize_an_independent_session_submission() {
    let memory = Arc::new(MemoryStore::new());
    let session_id = SessionId::new("session-blocked-retry").unwrap();
    let turn_id = TurnId::new("turn-blocked-retry").unwrap();
    memory
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header(session_id.as_str())),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn_id.clone(),
                        text: "retry body".into(),
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
                        turn_id: turn_id.clone(),
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    store.block_header_reads_for(session_id.clone());
    let blocked = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    turn_id,
                    session: fresh(header(session_id.as_str())),
                    text: "retry body".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store.header_read_attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry must reach the blocked Store read");

    let independent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        submit(&kernel, "session-independent", "independent"),
    )
    .await;
    store.release_blocked_header_reads();
    assert!(blocked.await.unwrap().is_ok());
    assert!(
        independent.is_ok(),
        "an unrelated Session was serialized behind blocked Store I/O"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn caller_turn_id_is_idempotent_live_and_after_restart_but_body_changes_conflict() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(Arc::clone(&store)).await;
    let worker = initial.start_write_behind();
    let turn_id = TurnId::new("caller-retry-turn").unwrap();
    let request = || SubmitTurn {
        turn_id: turn_id.clone(),
        session: fresh(header("session-idempotent-submit")),
        text: "same canonical body".into(),
        model: None,
        sandbox: None,
    };

    let first = initial.submit(request()).await.unwrap();
    assert_eq!(initial.submit(request()).await.unwrap(), first);
    assert!(matches!(
        initial
            .submit(SubmitTurn {
                text: "different body".into(),
                ..request()
            })
            .await,
        Err(TurnError::SubmissionConflict { session, turn })
            if session == "session-idempotent-submit" && turn == "caller-retry-turn"
    ));
    initial.shutdown(worker).await.unwrap();

    let restarted = kernel(Arc::clone(&store)).await;
    let restarted_worker = restarted.start_write_behind();
    assert_eq!(restarted.submit(request()).await.unwrap(), first);
    let stored = store
        .read_turn_boundary(&first.session_id, &turn_id)
        .await
        .unwrap();
    assert_eq!(stored.accepted_seq(), first.accepted_seq);
    restarted.shutdown(restarted_worker).await.unwrap();
}

#[tokio::test]
async fn indexed_turn_boundary_reads_share_the_process_store_read_admission() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-boundary-admission-a", 1).await;
    append_terminal_history(&memory, "session-boundary-admission-b", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_store_read_bytes: MAXIMUM_SESSION_FACT_BYTES,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    store.block_turn_boundary_reads();
    let turn = TurnId::new("turn-history-0").unwrap();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        let turn = turn.clone();
        async move {
            kernel
                .outcome(
                    &SessionId::new("session-boundary-admission-a").unwrap(),
                    &turn,
                )
                .await
        }
    });
    store.wait_for_turn_boundary_attempts(1).await;
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .outcome(
                    &SessionId::new("session-boundary-admission-b").unwrap(),
                    &TurnId::new("turn-history-0").unwrap(),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        store.turn_boundary_read_attempts(),
        1,
        "the second maximum-weight boundary read bypassed Store-read admission"
    );
    store.release_one_turn_boundary_read();
    store.wait_for_turn_boundary_attempts(2).await;
    store.release_one_turn_boundary_read();
    assert_eq!(first.await.unwrap().unwrap(), Some(TurnOutcome::Completed));
    assert_eq!(second.await.unwrap().unwrap(), Some(TurnOutcome::Completed));
}

#[tokio::test]
async fn caller_turn_id_retry_after_terminal_pruning_does_not_reexecute() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(Arc::clone(&store)).await;
    let worker = kernel.start_write_behind();
    let turn_id = TurnId::new("pruned-retry-turn").unwrap();
    let request = || SubmitTurn {
        turn_id: turn_id.clone(),
        session: fresh(header("session-pruned-retry")),
        text: "retried body".into(),
        model: None,
        sandbox: None,
    };

    let first = kernel.submit(request()).await.unwrap();
    // A second live turn keeps the session resident while the first turn's
    // durable terminal entry is pruned from the in-memory turn index.
    let keeper = kernel
        .submit(SubmitTurn {
            turn_id: TurnId::new("resident-keeper-turn").unwrap(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "keeper".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-pruned".into()).unwrap();
    let claim = kernel
        .claim("executor-pruned", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &first.turn_id);
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    // The terminal turn is pruned while the session stays resident; a retry
    // must resolve against the Store instead of re-accepting the turn.
    assert_eq!(kernel.submit(request()).await.unwrap(), first);

    // The only claimable turn is the keeper: the retry did not re-enqueue.
    let next = kernel
        .claim("executor-pruned", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.turn_id(), &keeper.turn_id);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn resident_session_keeps_its_pin_while_a_new_session_uses_the_new_generation() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();

    let first_header = header("session-generation-a");
    let first_pin = composition
        .pin(first_header.agent_preset_id())
        .await
        .unwrap();
    let first_tools = first_pin.tools();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(first_header, first_pin).unwrap(),
            ),
            text: "first A turn".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    composition.select_digest('b');
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, SessionId::new("session-generation-a").unwrap()).await,
            text: "resident still A".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second_header = header("session-generation-b");
    let second_pin = composition
        .pin(second_header.agent_preset_id())
        .await
        .unwrap();
    let second_tools = second_pin.tools();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(second_header, second_pin).unwrap(),
            ),
            text: "new session B".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();

    let _first_lease = kernel.register("executor-a".into()).unwrap();
    let _second_lease = kernel.register("executor-b".into()).unwrap();
    let first_claim = kernel
        .claim("executor-a", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.session_id().as_str(), "session-generation-a");
    let first_claim_pin = kernel.composition(&first_claim).unwrap();
    assert_eq!(first_claim_pin.source_digest(), "a".repeat(64));
    assert!(Arc::ptr_eq(&first_claim_pin.tools(), &first_tools));
    let second_claim = kernel
        .claim("executor-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_claim.session_id().as_str(), "session-generation-b");
    let second_claim_pin = kernel.composition(&second_claim).unwrap();
    assert_eq!(second_claim_pin.source_digest(), "b".repeat(64));
    assert!(Arc::ptr_eq(&second_claim_pin.tools(), &second_tools));
    assert!(!Arc::ptr_eq(
        &first_claim_pin.tools(),
        &second_claim_pin.tools()
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn shutdown_releases_resident_generation_pins_while_service_handles_escape() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let escaped_kernel = kernel.clone();
    let worker = kernel.start_write_behind();
    let session_header = header("session-shutdown-pin");
    let drops = Arc::new(AtomicUsize::new(0));
    let pin = AgentCompositionPin::new(
        session_header.agent_preset_id().clone(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(DropOwner(Arc::clone(&drops))),
    )
    .unwrap();

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(PreparedFreshSession::new(session_header, pin).unwrap()),
            text: "keep the resident generation pinned".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 0);

    kernel.shutdown(worker).await.unwrap();

    assert_eq!(
        drops.load(Ordering::Acquire),
        1,
        "shutdown must quiesce resident generation ownership even when a service handle escapes"
    );
    assert!(matches!(
        escaped_kernel
            .prepare_resume(&SessionId::new("session-shutdown-pin").unwrap())
            .await,
        Err(TurnError::ShuttingDown)
    ));
}

#[tokio::test]
async fn unavailable_cold_preset_fails_before_fact_log_materialization() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-unavailable-preset", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    composition.set_unavailable();
    store.reset_header_read_attempts();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();

    assert!(matches!(
        kernel
            .prepare_resume(&SessionId::new("session-unavailable-preset").unwrap())
            .await,
        Err(TurnError::Composition(_))
    ));
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    assert_eq!(store.header_read_attempts(), 1);
    assert_eq!(store.read_attempts(), 0);
    assert_eq!(store.open_turn_read_attempts(), 0);
    assert_eq!(
        memory
            .read_facts(&SessionId::new("session-unavailable-preset").unwrap(), 0, 8,)
            .await
            .unwrap()
            .facts
            .len(),
        2
    );
}

#[tokio::test]
async fn dropping_an_unsubmitted_cold_resume_releases_its_pin_without_hydration() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-dropped-resume-token", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let drops = Arc::new(AtomicUsize::new(0));
    let composition = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();

    let prepared = kernel
        .prepare_resume(&SessionId::new("session-dropped-resume-token").unwrap())
        .await
        .unwrap();
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    assert_eq!(drops.load(Ordering::Acquire), 0);
    assert_eq!(store.read_attempts(), 0);
    assert_eq!(store.open_turn_read_attempts(), 0);

    drop(prepared);
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn dropping_an_unsubmitted_fresh_draft_releases_its_pin_without_store_state() {
    let store = MemoryStore::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let composition: Arc<dyn AgentComposition> = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let session_header = header("session-dropped-fresh-draft");
    let session_id = session_header.session_id().clone();

    let draft = AgentSessionDraft::new(session_header, composition)
        .await
        .unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 0);

    drop(draft);

    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(matches!(
        store.header(&session_id).await,
        Err(StoreError::NotFound(missing)) if missing == session_id.to_string()
    ));
}

#[tokio::test]
async fn resume_token_from_another_kernel_is_rejected_and_releases_its_pin() {
    let source_store = Arc::new(MemoryStore::new());
    append_terminal_history(&source_store, "session-foreign-resume-token", 1).await;
    let drops = Arc::new(AtomicUsize::new(0));
    let composition = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let source_store_contract: Arc<dyn SessionStore> = source_store;
    let composition_contract: Arc<dyn AgentComposition> = composition;
    let source = SessionKernel::recover_with_clock(
        source_store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let prepared = source
        .prepare_resume(&SessionId::new("session-foreign-resume-token").unwrap())
        .await
        .unwrap();
    let target = kernel(Arc::new(MemoryStore::new())).await;

    let error = target
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(prepared),
            text: "must not cross Kernel authority".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Invalid(message) if message.contains("different Turn service")
    ));
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn resume_preparation_uses_the_resident_pin_when_the_source_is_unavailable() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_header = header("session-resident-damaged-source");
    let pin = composition
        .pin(session_header.agent_preset_id())
        .await
        .unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(PreparedFreshSession::new(session_header, pin).unwrap()),
            text: "resident A".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    composition.set_unavailable();

    let prepared = kernel
        .prepare_resume(&SessionId::new("session-resident-damaged-source").unwrap())
        .await
        .unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(prepared),
            text: "resident A remains available".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cold_resume_after_process_restart_pins_the_current_generation() {
    let store = Arc::new(MemoryStore::new());
    append_terminal_history(&store, "session-cold-generation-b", 1).await;
    let composition = Arc::new(MutableComposition::new('a'));
    composition.select_digest('b');
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition;
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(
                &kernel,
                SessionId::new("session-cold-generation-b").unwrap(),
            )
            .await,
            text: "cold session uses current B".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-cold-b".into()).unwrap();
    let claim = kernel
        .claim("executor-cold-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel.composition(&claim).unwrap().source_digest(),
        "b".repeat(64)
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn resume_after_idle_eviction_pins_the_current_generation() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store;
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_header = header("session-evicted-generation-b");
    let pin = composition
        .pin(session_header.agent_preset_id())
        .await
        .unwrap();
    let first = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Fresh(PreparedFreshSession::new(session_header, pin).unwrap()),
            text: "generation A".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor-eviction".into()).unwrap();
    let first_claim = kernel
        .claim("executor-eviction", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &first_claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    composition.select_digest('b');
    let prepared = kernel.prepare_resume(&first.session_id).await.unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(prepared),
            text: "generation B".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second_claim = kernel
        .claim("executor-eviction", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel.composition(&second_claim).unwrap().source_digest(),
        "b".repeat(64)
    );
    assert_eq!(composition.calls.load(Ordering::Acquire), 2);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cold_composition_failure_has_a_utf8_safe_bounded_diagnostic() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-unbounded-composition", 1).await;
    let store: Arc<dyn SessionStore> = memory;
    let composition: Arc<dyn AgentComposition> = Arc::new(UnboundedDiagnosticComposition);
    let kernel = SessionKernel::recover_with_clock(store, composition, Arc::new(FixedClock))
        .await
        .unwrap();

    let Err(TurnError::Composition(message)) = kernel
        .prepare_resume(&SessionId::new("session-unbounded-composition").unwrap())
        .await
    else {
        panic!("cold composition failure must preserve its typed error class");
    };
    assert!(message.len() <= MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
}

#[tokio::test]
async fn store_read_failure_has_a_utf8_safe_bounded_turn_diagnostic() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.fail_next_read(format!(
        "{}\0tail",
        "界".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)
    ));

    let Err(TurnError::Store(message)) = kernel
        .observe(&SessionId::new("session-store-diagnostic").unwrap(), 0)
        .await
    else {
        panic!("ordinary Store read failure must not be classified as a durability flush");
    };
    assert!(message.len() <= MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    assert!(!message.contains('\0'));
}

#[tokio::test(start_paused = true)]
async fn explicit_effect_flush_waits_through_transient_failure_without_reordering() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-retry", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-1").unwrap();
    let facts = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect,
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    store.fail_next_appends(1);
    let through = facts.last().unwrap().seq();
    let flush = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.flush(&claim, through).await }
    });
    tokio::task::yield_now().await;
    assert!(!flush.is_finished());
    assert_eq!(
        store
            .read_facts(&submitted.session_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        submitted.accepted_seq
    );
    tokio::time::advance(std::time::Duration::from_millis(199)).await;
    tokio::task::yield_now().await;
    assert!(!flush.is_finished());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(flush.await.unwrap().unwrap(), through);
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert_eq!(stored.facts.len(), 2);
    assert!(matches!(
        stored.facts[0].body(),
        SessionFactBody::TurnAccepted { .. }
    ));
    assert!(matches!(
        stored.facts[1].body(),
        SessionFactBody::ModelIntent { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn effect_start_requires_its_intent_to_be_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-effect-fence", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-effect-fence").unwrap();

    let error = kernel
        .publish(
            &claim,
            vec![
                SessionFactBody::ModelIntent {
                    turn_id: submitted.turn_id.clone(),
                    effect_id: effect.clone(),
                    snapshot: snapshot(),
                },
                SessionFactBody::ModelStarted {
                    turn_id: submitted.turn_id.clone(),
                    effect_id: effect.clone(),
                },
            ],
        )
        .await
        .expect_err("an effect start cannot share the undurable intent publication");
    assert!(matches!(error, TurnError::Invalid(_)));

    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let through = intent.last().unwrap().seq();
    kernel.flush(&claim, through).await.unwrap();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: submitted.turn_id,
                effect_id: effect,
            }],
        )
        .await
        .unwrap()
        .published();

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancellation_single_assigns_cancelled_even_if_executor_reports_completed() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    assert!(
        kernel
            .cancel(
                &submitted.session_id,
                &submitted.turn_id,
                Some("stop".into())
            )
            .await
            .unwrap()
            .accepted
    );
    assert!(cancellation.is_cancelled());
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Cancelled,
            ..
        }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn claim_horizon_hides_later_accepted_turns_but_admits_claimed_turn_facts() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-horizon", "FIRST_PRIVATE_PROMPT").await;
    let later = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "LATER_PRIVATE_PROMPT".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &first.turn_id);

    let initial = kernel.read_facts(&claim, 0, 8).await.unwrap();
    assert_eq!(initial.through_seq, later.accepted_seq);
    assert_eq!(initial.facts.len(), 1);
    assert!(matches!(
        initial.facts[0].body(),
        SessionFactBody::TurnAccepted { text, .. } if text == "FIRST_PRIVATE_PROMPT"
    ));

    let effect_id = EffectId::new("effect-horizon").unwrap();
    let published = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: first.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let incremental = kernel
        .read_facts(&claim, initial.through_seq, 8)
        .await
        .unwrap();
    assert_eq!(incremental.facts, published);
    assert!(matches!(
        incremental.facts[0].body(),
        SessionFactBody::ModelIntent { effect_id: current, .. } if current == &effect_id
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn checkpoint_maintenance_reads_the_exact_prefix_including_queued_turns() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-checkpoint-queue", "first").await;
    let queued = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "queued".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    let page = kernel
        .read_checkpoint_facts(&claim, 0, 8)
        .await
        .unwrap()
        .expect("a terminal claim with no speculative suffix is checkpointable");

    assert_eq!(page.through_seq, terminal.last().unwrap().seq());
    assert!(page.facts.iter().any(|fact| {
        matches!(
            fact.body(),
            SessionFactBody::TurnAccepted { turn_id, text, .. }
                if turn_id == &queued.turn_id && text == "queued"
        )
    }));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn checkpoint_maintenance_rejects_a_foreign_terminal_claim() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-checkpoint-claim-binding", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    let foreign = TurnClaimIssuer::new().issue(
        claim.executor_id().to_owned(),
        claim.claim_id(),
        claim.session_id().clone(),
        claim.turn_id().clone(),
        Arc::new(claim.header().clone()),
        claim.accepted_at_ms(),
        claim.accepted_seq(),
        claim.live_seq(),
    );
    assert!(matches!(
        kernel
            .read_checkpoint_facts(&foreign, 0, MAXIMUM_FACTS_PER_READ)
            .await,
        Err(TurnError::StaleClaim)
    ));

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn checkpoint_store_failure_remains_typed_at_the_execution_seam() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-checkpoint-write-failure", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    let through_seq = terminal.last().unwrap().seq();
    kernel.flush(&claim, through_seq).await.unwrap();
    store.fail_next_checkpoint_write();

    assert!(matches!(
        kernel
            .write_context_checkpoint(
                &claim,
                ContextCheckpoint {
                    header_fingerprint: claim.header().fingerprint().unwrap(),
                    through_seq,
                    fact_prefix_sha256: "0".repeat(64),
                    bytes: Arc::from(b"checkpoint".as_slice()),
                },
            )
            .await,
        Err(TurnError::Store(message)) if message.contains("injected checkpoint write failure")
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn tightened_store_read_budget_disables_checkpoint_maintenance_end_to_end() {
    let store = Arc::new(MemoryStore::new());
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_store_read_bytes: MAXIMUM_SESSION_FACT_BYTES,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-checkpoint-disabled", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    let through_seq = terminal.last().unwrap().seq();
    kernel.flush(&claim, through_seq).await.unwrap();

    assert!(
        kernel
            .read_checkpoint_facts(&claim, 0, MAXIMUM_FACTS_PER_READ)
            .await
            .unwrap()
            .is_none()
    );
    let durable = store
        .read_facts(claim.session_id(), 0, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap();
    assert!(
        !kernel
            .write_context_checkpoint(
                &claim,
                ContextCheckpoint {
                    header_fingerprint: claim.header().fingerprint().unwrap(),
                    through_seq,
                    fact_prefix_sha256: fact_prefix_sha256(&durable.facts).unwrap(),
                    bytes: Arc::from(b"disabled-checkpoint".as_slice()),
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .read_context_checkpoint(claim.session_id())
            .await
            .unwrap()
            .is_none()
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The deterministic interleaving keeps every race barrier visible.
async fn claim_fact_read_never_skips_a_prefix_committed_during_store_io() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel =
        SessionKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-read-race", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-read-race").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    let started = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, started.last().unwrap().seq())
        .await
        .unwrap();
    worker.abort();
    let _ = worker.await;

    let mut first_batch = Vec::with_capacity(MAXIMUM_FACTS_PER_READ);
    first_batch.push(SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect.clone(),
        event: LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
    });
    first_batch.extend(
        (1..MAXIMUM_FACTS_PER_READ).map(|_| SessionFactBody::ModelEvent {
            turn_id: submitted.turn_id.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("x".into()),
            },
        }),
    );
    kernel
        .publish(&claim, first_batch)
        .await
        .unwrap()
        .published();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelEvent {
                turn_id: submitted.turn_id,
                effect_id: effect,
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("tail".into()),
                },
            }],
        )
        .await
        .unwrap()
        .published();

    store.pause_next_read();
    let read = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.read_facts(&claim, 0, MAXIMUM_FACTS_PER_READ).await }
    });
    store.wait_until_read_is_captured().await;
    store.pause_second_following_append();
    let worker = kernel.start_write_behind();
    store.wait_until_append_is_blocked().await;
    store.release_captured_read();

    let page = read.await.unwrap().unwrap();
    assert_eq!(page.through_seq, 3);
    assert!(
        page.facts
            .windows(2)
            .all(|pair| pair[1].seq() == pair[0].seq() + 1),
        "a Store prefix committed during the read must be returned on a later page, not skipped: {:?}",
        page.facts.iter().map(|fact| fact.seq()).collect::<Vec<_>>()
    );
    let committed = kernel
        .read_facts(&claim, page.through_seq, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap();
    assert_eq!(committed.facts.first().map(|fact| fact.seq()), Some(4));
    assert_eq!(committed.facts.last().map(|fact| fact.seq()), Some(515));
    assert_eq!(committed.through_seq, 515);

    store.release_blocked_append();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn claim_fact_read_does_not_cross_the_live_horizon_captured_before_store_io() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel =
        SessionKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-captured-live-horizon", "first").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.flush(&claim, submitted.accepted_seq).await.unwrap();
    worker.abort();
    let _ = worker.await;

    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: EffectId::new("captured-live-horizon").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let captured_live_seq = intent.last().unwrap().seq();

    store.pause_next_read();
    let read = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.read_facts(&claim, 0, MAXIMUM_FACTS_PER_READ).await }
    });
    store.wait_until_read_is_captured().await;
    let worker = kernel.start_write_behind();
    let later = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: resume(&kernel, submitted.session_id).await,
                    text: "LATER_PRIVATE_PROMPT".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(
        !later.is_finished(),
        "a maximum-weight boundary read must wait behind the captured Store page"
    );
    store.release_captured_read();

    let page = read.await.unwrap().unwrap();
    let later = later.await.unwrap().unwrap();
    assert!(page.through_seq <= captured_live_seq);
    assert!(
        page.facts
            .iter()
            .all(|fact| fact.seq() <= captured_live_seq)
    );
    assert!(page.facts.iter().all(|fact| {
        !matches!(
            fact.body(),
            SessionFactBody::TurnAccepted { turn_id, .. } if turn_id == &later.turn_id
        )
    }));

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn executor_cannot_classify_cancellation_without_a_durable_request() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-unrequested-cancel", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Cancelled,
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    assert!(matches!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Failed { code, .. }) if code == "executor.unrequested_cancellation"
    ));
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Failed { code, .. },
            ..
        } if code == "executor.unrequested_cancellation"
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn terminal_outcome_and_fact_are_hidden_until_their_prefix_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let initial_worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-terminal-fence", "hello").await;
    initial_worker.abort();
    let _ = initial_worker.await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        None
    );
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), observation.next())
            .await
            .is_err(),
        "a speculative terminal Fact must not enter observation"
    );

    let worker = kernel.start_write_behind();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    let mut observed_terminal = false;
    while let Some(update) = observation.next().await {
        if matches!(
            update.unwrap(),
            TurnUpdate::Fact { fact, durable_seq }
                if fact.seq() == terminal[0].seq() && durable_seq >= fact.seq()
        ) {
            observed_terminal = true;
            break;
        }
    }
    assert!(observed_terminal);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancellation_does_not_fire_before_its_fact_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-durable", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    store.fail_next_appends(1);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move {
            kernel
                .cancel(&session_id, &turn_id, Some("stop".into()))
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!cancellation.is_cancelled());
    assert!(!cancelling.is_finished());

    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    assert!(cancelling.await.unwrap().unwrap().accepted);
    assert!(cancellation.is_cancelled());
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::CancelRequested { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn durable_cancellation_fires_even_after_the_requesting_future_detaches() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-detached", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    store.fail_next_appends(1);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });
    let update = observation.next().await.unwrap().unwrap();
    assert!(matches!(
        update,
        TurnUpdate::Fact { fact, .. }
            if matches!(fact.body(), SessionFactBody::CancelRequested { .. })
    ));
    cancelling.abort();
    let _ = cancelling.await;
    assert!(!cancellation.is_cancelled());

    tokio::time::advance(std::time::Duration::from_millis(400)).await;
    tokio::task::yield_now().await;
    assert!(
        cancellation.is_cancelled(),
        "durable commit, not request-future ownership, must fire the token"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn persistent_store_failure_eventually_latches_a_flush_error() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-persistent-io", "hello").await;
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    store.fail_next_appends(usize::MAX);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });

    for _ in 0..16 {
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        if cancelling.is_finished() {
            break;
        }
    }
    assert!(
        cancelling.is_finished(),
        "persistent I/O failure must not leave a public cancel future pending forever"
    );
    assert!(matches!(
        cancelling.await.unwrap(),
        Err(TurnError::Flush(_))
    ));
    assert!(matches!(
        observation.next().await,
        Some(Ok(TurnUpdate::Fact { fact, .. }))
            if matches!(fact.body(), SessionFactBody::CancelRequested { .. })
    ));
    let terminal = tokio::time::timeout(std::time::Duration::from_millis(1), observation.next())
        .await
        .expect("a latched flush error must terminate the attached observation");
    assert!(matches!(terminal, Some(Err(TurnError::Flush(_)))));
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: resume(&kernel, submitted.session_id.clone()).await,
                text: "must not wedge behind the permanent failure".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Flush(_))
    ));
    assert!(kernel.shutdown(worker).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn failed_cancellation_admission_can_be_retried_after_capacity_recovers() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-cancel-full", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("effect-fill").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: first.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: first.turn_id.clone(),
                effect_id: effect.clone(),
            }],
        )
        .await
        .unwrap()
        .published();
    let mut chunk = MAX_LANGUAGE_OUTPUT_BYTES;
    while chunk > 0 {
        let result = kernel
            .publish(
                &claim,
                vec![SessionFactBody::ModelEvent {
                    turn_id: first.turn_id.clone(),
                    effect_id: effect.clone(),
                    event: LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("x".repeat(chunk)),
                    },
                }],
            )
            .await;
        match result {
            Ok(_) => {}
            Err(TurnError::Flush(_) | TurnError::BudgetExceeded { .. }) => chunk /= 2,
            Err(error) => panic!("unexpected fill failure: {error}"),
        }
    }

    assert!(
        kernel
            .cancel(
                &first.session_id,
                &first.turn_id,
                Some("x".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)),
            )
            .await
            .is_err(),
        "the full speculative suffix must reject the cancellation Fact"
    );

    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    let retry = kernel
        .cancel(&first.session_id, &first.turn_id, None)
        .await
        .unwrap();
    assert!(
        retry.accepted,
        "failed admission must not consume cancellation"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_timeout_stops_the_worker_and_releases_its_store_owner() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-shutdown-failure", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    store.fail_next_appends(usize::MAX);
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id,
                effect_id: EffectId::new("shutdown-pending").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();

    let shutdown = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.shutdown(worker).await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert!(shutdown.await.unwrap().is_err());
    drop(kernel);
    assert_eq!(
        Arc::strong_count(&store),
        1,
        "failed shutdown must not leave the Store owned by a detached worker"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_snapshots_flush_waiters_before_terminal_sessions_can_be_evicted() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-shutdown-a", "first").await;
    let second = submit(&kernel, "session-shutdown-b", "second").await;
    let _lease = kernel.register("executor".into()).unwrap();

    for submitted in [&first, &second] {
        let claim = kernel
            .claim("executor", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.turn_id(), &submitted.turn_id);
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::TurnTerminal {
                    turn_id: submitted.turn_id.clone(),
                    outcome: TurnOutcome::Completed,
                }],
            )
            .await
            .unwrap()
            .published();
    }

    kernel
        .shutdown(worker)
        .await
        .expect("terminal eviction must not invalidate a later shutdown waiter");
    for submitted in [first, second] {
        let page = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
        assert_eq!(page.durable_seq, 2);
        assert!(matches!(
            page.facts.last().map(SessionFact::body),
            Some(SessionFactBody::TurnTerminal { .. })
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_fences_publish_before_its_final_flush_snapshot_can_be_extended() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-shutdown-publish", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect_id = EffectId::new("shutdown-publish").unwrap();
    store.pause_next_append();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();

    let shutdown = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.shutdown(worker).await }
    });
    store.wait_until_append_is_blocked().await;
    assert!(matches!(
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::ModelStarted {
                    turn_id: submitted.turn_id.clone(),
                    effect_id,
                }],
            )
            .await,
        Err(TurnError::ShuttingDown)
    ));

    store.release_blocked_append();
    shutdown.await.unwrap().unwrap();
    let page = memory
        .read_facts(&submitted.session_id, 0, 8)
        .await
        .unwrap();
    assert_eq!(page.durable_seq, 2);
}

#[tokio::test]
async fn shutdown_settles_joined_cold_hydration_without_installing_a_resident_pin() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-shutdown-hydration", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let drops = Arc::new(AtomicUsize::new(0));
    let composition = Arc::new(DropTrackingComposition {
        calls: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    store.pause_next_open_turn_read();

    let leader = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-shutdown-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "leader".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let follower = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-shutdown-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "follower".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    while composition.calls.load(Ordering::Acquire) != 1 {
        tokio::task::yield_now().await;
    }

    kernel.shutdown(worker).await.unwrap();

    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_millis(100), follower)
            .await
            .expect("shutdown must settle hydration followers before Store I/O returns")
            .unwrap(),
        Err(TurnError::ShuttingDown)
    );
    store.release_captured_open_turn_read();
    assert_eq!(leader.await.unwrap(), Err(TurnError::ShuttingDown));
    assert_eq!(
        drops.load(Ordering::Acquire),
        1,
        "the shared hydration pin may not become resident after shutdown"
    );
}

#[tokio::test]
async fn next_turn_is_not_claimable_until_the_previous_terminal_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-queue", "first").await;
    let second = kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(&kernel, first.session_id.clone()).await,
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _one = kernel.register("one".into()).unwrap();
    let _two = kernel.register("two".into()).unwrap();
    let first_claim = kernel
        .claim("one", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id(), &first.turn_id);
    let terminal = kernel
        .publish(
            &first_claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap()
        .published();
    let waiting = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.claim("two", CancellationToken::new()).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    kernel
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        waiting.await.unwrap().unwrap().unwrap().turn_id(),
        &second.turn_id
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn conflicting_retry_does_not_replace_the_original_turn_control_state() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let turn_id = TurnId::new("caller-stable-turn").unwrap();
    let first = kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header("session-conflicting-retry")),
            text: "original".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                turn_id: turn_id.clone(),
                session: fresh(header("session-conflicting-retry")),
                text: "changed".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::SubmissionConflict { .. })
    ));
    let _lease = kernel.register("executor-conflict".into()).unwrap();
    let claim = kernel
        .claim("executor-conflict", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.session_id(), &first.session_id);
    assert_eq!(claim.turn_id(), &turn_id);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the exact admission, flush, and retry timeline visible.
async fn process_capacity_flush_required_preserves_bodies_and_turn_control_state() {
    let turn_id = TurnId::new("turn-1").unwrap();
    let effect_id = EffectId::new("effect-capacity").unwrap();
    let accepted = SessionFact::new(
        1,
        42,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    let intent = SessionFactBody::ModelIntent {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
        snapshot: snapshot(),
    };
    let started = SessionFactBody::ModelStarted {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
    };
    let body = SessionFactBody::ModelEvent {
        turn_id: turn_id.clone(),
        effect_id: effect_id.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(1024)),
        },
    };
    let second_body = SessionFactBody::ModelEvent {
        turn_id: turn_id.clone(),
        effect_id,
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".repeat(1024)),
        },
    };
    let body_bytes = [
        intent.clone(),
        started.clone(),
        body.clone(),
        second_body.clone(),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, body)| {
        SessionFact::new(u64::try_from(index + 2).unwrap(), 42, body)
            .unwrap()
            .encoded_len()
    })
    .max()
    .unwrap();
    let limits = KernelLimits {
        maximum_process_pending_fact_bytes: accepted.encoded_len().max(body_bytes),
        ..KernelLimits::default()
    };
    let memory = Arc::new(MemoryStore::new());
    let store: Arc<dyn SessionStore> = memory.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store,
        composition(),
        Arc::new(FixedClock),
        limits,
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let _submitted = kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header("session-process-publish")),
            text: "hello".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let intent = kernel
        .publish(&claim, vec![intent])
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    let started = kernel
        .publish(&claim, vec![started])
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, started.last().unwrap().seq())
        .await
        .unwrap();
    memory.fail_next_appends(usize::MAX);
    let published = kernel
        .publish(&claim, vec![body])
        .await
        .unwrap()
        .published();
    let first_rejection = kernel.publish(&claim, vec![second_body.clone()]).await;
    assert!(
        matches!(
            &first_rejection,
            Ok(PublishAttempt::FlushRequired { unpublished }) if unpublished == &vec![second_body.clone()]
        ),
        "unexpected capacity result: {first_rejection:?}"
    );
    assert!(matches!(
        kernel.publish(&claim, vec![second_body.clone()]).await,
        Ok(PublishAttempt::FlushRequired { unpublished }) if unpublished == vec![second_body.clone()]
    ));
    memory.fail_next_appends(0);
    kernel
        .flush(&claim, published.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .publish(&claim, vec![second_body])
            .await
            .unwrap()
            .published()
            .len(),
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn publication_larger_than_an_empty_process_budget_is_invalid() {
    let turn_id = TurnId::new("turn-oversized-publication").unwrap();
    let intent = SessionFactBody::ModelIntent {
        turn_id: turn_id.clone(),
        effect_id: EffectId::new("effect-oversized-publication").unwrap(),
        snapshot: snapshot(),
    };
    let accepted_bytes = SessionFact::new(
        1,
        42,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
    .encoded_len();
    assert!(
        SessionFact::new(2, 42, intent.clone())
            .unwrap()
            .encoded_len()
            > accepted_bytes
    );
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: accepted_bytes,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: turn_id.clone(),
            session: fresh(header("session-oversized-publication")),
            text: "hello".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(
        kernel.publish(&claim, vec![intent]).await,
        Err(TurnError::Invalid(_))
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancel_reports_pending_capacity_separately_from_durable_flush_failure() {
    let memory = Arc::new(MemoryStore::new());
    let kernel = kernel(memory.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-pending-capacity", "hello").await;
    let _lease = kernel
        .register("executor-capacity-taxonomy".into())
        .unwrap();
    let claim = kernel
        .claim("executor-capacity-taxonomy", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect_id = EffectId::new("effect-capacity-taxonomy").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let intent_bytes = intent[0].encoded_len();
    kernel.flush(&claim, intent[0].seq()).await.unwrap();

    let started = SessionFactBody::ModelStarted {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect_id.clone(),
    };
    let first_delta = SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect_id.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(MAX_LANGUAGE_OUTPUT_BYTES)),
        },
    };
    let second_delta_base = SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect_id.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".into()),
        },
    };
    let started_bytes = SessionFact::new(3, 42, started.clone())
        .unwrap()
        .encoded_len();
    let first_delta_bytes = SessionFact::new(4, 42, first_delta.clone())
        .unwrap()
        .encoded_len();
    let second_base_bytes = SessionFact::new(5, 42, second_delta_base)
        .unwrap()
        .encoded_len();
    let pending_budget = usize::try_from(MAXIMUM_TURN_GENERATED_FACT_BYTES).unwrap() - intent_bytes;
    let second_text_bytes = pending_budget
        .checked_sub(started_bytes + first_delta_bytes + second_base_bytes - 1)
        .expect("maximum deltas leave room for the second event");
    assert!(second_text_bytes <= MAX_LANGUAGE_OUTPUT_BYTES);
    let second_delta = SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id,
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".repeat(second_text_bytes)),
        },
    };

    memory.fail_next_appends(usize::MAX);
    assert!(matches!(
        kernel
            .publish(&claim, vec![started, first_delta, second_delta])
            .await
            .unwrap(),
        PublishAttempt::Published(_)
    ));
    assert_eq!(MAXIMUM_PENDING_FACT_BYTES, 64 * 1024 * 1024);
    assert_eq!(
        kernel
            .cancel(
                &submitted.session_id,
                &submitted.turn_id,
                Some("c".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)),
            )
            .await,
        Err(TurnError::Capacity)
    );

    memory.fail_next_appends(0);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
#[allow(clippy::too_many_lines)] // One scenario keeps both Sessions and the blocked durable commit ordered.
async fn cross_session_process_pressure_waits_for_global_durable_progress() {
    let first_turn = TurnId::new("turn-process-pressure-a").unwrap();
    let second_turn = TurnId::new("turn-process-pressure-b").unwrap();
    let first_body = SessionFactBody::ModelIntent {
        turn_id: first_turn.clone(),
        effect_id: EffectId::new("effect-process-pressure-a").unwrap(),
        snapshot: snapshot(),
    };
    let second_body = SessionFactBody::ModelIntent {
        turn_id: second_turn.clone(),
        effect_id: EffectId::new("effect-process-pressure-b").unwrap(),
        snapshot: snapshot(),
    };
    let fact_bytes = [
        SessionFact::new(
            1,
            42,
            SessionFactBody::TurnAccepted {
                turn_id: first_turn.clone(),
                text: "first".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap()
        .encoded_len(),
        SessionFact::new(2, 42, first_body.clone())
            .unwrap()
            .encoded_len(),
        SessionFact::new(
            1,
            42,
            SessionFactBody::TurnAccepted {
                turn_id: second_turn.clone(),
                text: "second".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap()
        .encoded_len(),
        SessionFact::new(2, 42, second_body.clone())
            .unwrap()
            .encoded_len(),
    ]
    .into_iter()
    .max()
    .unwrap();
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: fact_bytes,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let first = kernel
        .submit(SubmitTurn {
            turn_id: first_turn,
            session: fresh(header("session-process-pressure-a")),
            text: "first".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let second = kernel
        .submit(SubmitTurn {
            turn_id: second_turn,
            session: fresh(header("session-process-pressure-b")),
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _first_lease = kernel.register("executor-pressure-a".into()).unwrap();
    let _second_lease = kernel.register("executor-pressure-b".into()).unwrap();
    let first_claim = kernel
        .claim("executor-pressure-a", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let second_claim = kernel
        .claim("executor-pressure-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id(), &first.turn_id);
    assert_eq!(second_claim.turn_id(), &second.turn_id);
    let first_fact = kernel
        .publish(&first_claim, vec![first_body])
        .await
        .unwrap()
        .published()
        .pop()
        .unwrap();
    store.pause_next_append();
    let first_flush = tokio::spawn({
        let kernel = kernel.clone();
        let first_claim = first_claim.clone();
        async move { kernel.flush(&first_claim, first_fact.seq()).await }
    });
    store.wait_until_append_is_blocked().await;
    let second_publish = tokio::spawn({
        let kernel = kernel.clone();
        let second_claim = second_claim.clone();
        async move { kernel.publish(&second_claim, vec![second_body]).await }
    });
    tokio::task::yield_now().await;
    assert!(
        !second_publish.is_finished(),
        "cross-Session pressure must wait for global durable progress"
    );

    store.release_blocked_append();
    first_flush.await.unwrap().unwrap();
    assert!(matches!(
        second_publish.await.unwrap().unwrap(),
        PublishAttempt::Published(_)
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
#[allow(clippy::too_many_lines)] // One scenario keeps both Sessions, pressure, and failure ordered.
async fn cross_session_process_pressure_observes_own_permanent_flush_failure() {
    let target_session = SessionId::new("session-process-failure-a").unwrap();
    let blocker_session = SessionId::new("session-process-failure-b").unwrap();
    let target_turn = TurnId::new("turn-process-failure-a").unwrap();
    let blocker_turn = TurnId::new("turn-process-failure-b").unwrap();
    let target_effect = EffectId::new("effect-process-failure-a").unwrap();
    let blocker_effect = EffectId::new("effect-process-failure-b").unwrap();
    let target_first = SessionFactBody::ModelEvent {
        turn_id: target_turn.clone(),
        effect_id: target_effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(4096)),
        },
    };
    let target_second = SessionFactBody::ModelEvent {
        turn_id: target_turn.clone(),
        effect_id: target_effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("y".repeat(4096)),
        },
    };
    let blocker_first = SessionFactBody::ModelEvent {
        turn_id: blocker_turn.clone(),
        effect_id: blocker_effect.clone(),
        event: LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("z".repeat(4096)),
        },
    };
    let process_bytes = SessionFact::new(4, 42, target_first.clone())
        .unwrap()
        .encoded_len()
        + SessionFact::new(4, 42, blocker_first.clone())
            .unwrap()
            .encoded_len();
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store.clone() as Arc<dyn SessionStore>,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: process_bytes,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: target_turn.clone(),
            session: fresh(header(target_session.as_str())),
            text: "target".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    kernel
        .submit(SubmitTurn {
            turn_id: blocker_turn.clone(),
            session: fresh(header(blocker_session.as_str())),
            text: "blocker".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _target_lease = kernel.register("executor-failure-a".into()).unwrap();
    let _blocker_lease = kernel.register("executor-failure-b".into()).unwrap();
    let target_claim = kernel
        .claim("executor-failure-a", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let blocker_claim = kernel
        .claim("executor-failure-b", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    for (claim, turn_id, effect_id) in [
        (&target_claim, &target_turn, &target_effect),
        (&blocker_claim, &blocker_turn, &blocker_effect),
    ] {
        let intent = kernel
            .publish(
                claim,
                vec![SessionFactBody::ModelIntent {
                    turn_id: turn_id.clone(),
                    effect_id: effect_id.clone(),
                    snapshot: snapshot(),
                }],
            )
            .await
            .unwrap()
            .published();
        kernel.flush(claim, intent[0].seq()).await.unwrap();
        let started = kernel
            .publish(
                claim,
                vec![SessionFactBody::ModelStarted {
                    turn_id: turn_id.clone(),
                    effect_id: effect_id.clone(),
                }],
            )
            .await
            .unwrap()
            .published();
        kernel.flush(claim, started[0].seq()).await.unwrap();
    }

    kernel
        .publish(&target_claim, vec![target_first])
        .await
        .unwrap();
    kernel
        .publish(&blocker_claim, vec![blocker_first])
        .await
        .unwrap();
    store.fail_appends_for(target_session);
    store.pause_second_following_append();
    let waiting = tokio::spawn({
        let kernel = kernel.clone();
        let target_claim = target_claim.clone();
        async move { kernel.publish(&target_claim, vec![target_second]).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store.wait_until_append_is_blocked(),
    )
    .await
    .expect("the unrelated Session flush must remain blocked");

    let result = tokio::time::timeout(std::time::Duration::from_millis(100), waiting)
        .await
        .expect("the Session's permanent flush failure must wake its pressured publication")
        .unwrap();
    assert!(matches!(result, Err(TurnError::Flush(_))));

    store.release_blocked_append();
    assert!(kernel.shutdown(worker).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn live_session_working_set_has_an_exact_global_bound() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        submit(&kernel, &format!("session-bound-{index}"), "queued").await;
    }
    assert_eq!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: fresh(header("session-bound-overflow")),
                text: "overflow".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn active_observer_capacity_is_exact_and_released_on_drop() {
    let store = Arc::new(MemoryStore::new());
    let store_contract: Arc<dyn SessionStore> = store;
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-observer-bound", "queued").await;
    let mut observers = Vec::with_capacity(DEFAULT_MAXIMUM_ACTIVE_OBSERVERS);
    for _ in 0..DEFAULT_MAXIMUM_ACTIVE_OBSERVERS {
        observers.push(kernel.observe(&submitted.session_id, 0).await.unwrap());
    }
    assert!(matches!(
        kernel.observe(&submitted.session_id, 0).await,
        Err(TurnError::ObserverCapacity)
    ));
    drop(observers.pop());
    observers.push(kernel.observe(&submitted.session_id, 0).await.unwrap());
    drop(observers);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn observation_reports_durability_that_advanced_while_unpolled() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-durable-update", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.flush(&claim, submitted.accepted_seq).await.unwrap();
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    let published = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id,
                effect_id: EffectId::new("effect-observed").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    assert!(matches!(
        observation.next().await,
        Some(Ok(TurnUpdate::Fact { fact, .. })) if fact.seq() == published[0].seq()
    ));
    kernel.flush(&claim, published[0].seq()).await.unwrap();

    let update = tokio::time::timeout(std::time::Duration::from_millis(100), observation.next())
        .await
        .expect("an unseen durability advance must wake the stream")
        .expect("observation remains open")
        .unwrap();
    assert_eq!(
        update,
        TurnUpdate::Durable {
            durable_seq: published[0].seq()
        }
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancelling_evicted_terminal_turns_does_not_consume_live_session_capacity() {
    let store = Arc::new(MemoryStore::new());
    let mut terminal_turns = Vec::new();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("terminal-session-{index}")).unwrap();
        let turn_id = TurnId::new(format!("terminal-turn-{index}")).unwrap();
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: Some(header(session_id.as_str())),
                facts: vec![
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            turn_id: turn_id.clone(),
                            text: "done".into(),
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
                            turn_id: turn_id.clone(),
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            })
            .await
            .unwrap();
        terminal_turns.push((session_id, turn_id));
    }
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    for (session_id, turn_id) in terminal_turns {
        assert!(
            kernel
                .cancel(&session_id, &turn_id, Some("late".into()))
                .await
                .unwrap()
                .already_terminal
        );
    }

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header("capacity-remains-free")),
            text: "new".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn invalid_resumes_of_idle_durable_sessions_do_not_consume_live_capacity() {
    let store = Arc::new(MemoryStore::new());
    let mut terminal_sessions = Vec::new();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("invalid-resume-session-{index}")).unwrap();
        let turn_id = TurnId::new(format!("invalid-resume-turn-{index}")).unwrap();
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: Some(header(session_id.as_str())),
                facts: vec![
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            turn_id: turn_id.clone(),
                            text: "done".into(),
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
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            })
            .await
            .unwrap();
        terminal_sessions.push(session_id);
    }
    let kernel = kernel(store).await;
    let oversized = "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1);
    for session_id in terminal_sessions {
        assert!(matches!(
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: resume(&kernel, session_id).await,
                    text: oversized.clone(),
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(TurnError::Invalid(_))
        ));
    }

    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header("capacity-after-invalid-resumes")),
            text: "new".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("invalid resume input must not retain idle durable sessions");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn failed_admission_after_hydration_releases_idle_resident_capacity() {
    let memory = Arc::new(MemoryStore::new());
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        append_terminal_history(&memory, &format!("failed-admission-session-{index}"), 1).await;
    }
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: 1,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        assert!(matches!(
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: resume(
                        &kernel,
                        SessionId::new(format!("failed-admission-session-{index}")).unwrap(),
                    )
                    .await,
                    text: "cannot fit".into(),
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(TurnError::Capacity)
        ));
    }

    store.reset_header_read_attempts();
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: fresh(header("capacity-after-failed-admissions")),
                text: "cannot fit either".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    ));
    assert_eq!(
        store.header_read_attempts(),
        1,
        "failed admission must release each newly hydrated idle session before fresh capacity is checked"
    );
}

#[tokio::test]
async fn historical_outcome_lookup_does_not_page_the_complete_session_log() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-indexed-outcome", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.reset_read_attempts();

    assert_eq!(
        kernel
            .outcome(
                &SessionId::new("session-indexed-outcome").unwrap(),
                &TurnId::new("turn-history-299").unwrap(),
            )
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(
        store.read_attempts(),
        0,
        "an outcome lookup must use the Store's turn index, not full-log pages"
    );
}

#[tokio::test]
async fn recovery_skips_fact_pages_for_sessions_without_open_turns() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-recovery-index", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();

    SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
        .await
        .unwrap();

    assert_eq!(
        store.read_attempts(),
        0,
        "recovery must query the bounded open-turn index before decoding Fact bodies"
    );
    assert_eq!(
        store.open_turn_read_attempts(),
        0,
        "closed sessions must be excluded by Store enumeration, not probed one by one"
    );
}

#[tokio::test]
async fn durable_observation_pages_store_reads_instead_of_reading_one_fact_at_a_time() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-observation-pages", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.reset_read_attempts();

    let session = SessionId::new("session-observation-pages").unwrap();
    let mut observation = kernel.observe(&session, 0).await.unwrap();
    let mut facts = 0;
    while let Some(update) = observation.next().await {
        if matches!(update.unwrap(), TurnUpdate::Fact { .. }) {
            facts += 1;
        }
    }

    assert_eq!(facts, 600);
    assert_eq!(
        store.read_attempts(),
        2,
        "600 durable Facts fit in two protocol-bounded Store pages"
    );
}

#[tokio::test]
async fn concurrent_resumes_join_one_control_state_load() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-joined-load", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();
    store.pause_next_open_turn_read();

    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-joined-load").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-joined-load").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "second".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    store.release_captured_open_turn_read();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_eq!(
        store.open_turn_read_attempts(),
        1,
        "concurrent resumes of one idle session must join one Store load"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn concurrent_resume_joins_the_resident_load_when_source_becomes_unavailable() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-source-race", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let composition = Arc::new(MutableComposition::new('a'));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();
    let kernel = SessionKernel::recover_with_clock(
        store_contract,
        composition_contract,
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    store.pause_next_open_turn_read();

    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-source-race").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    composition.set_unavailable();
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-source-race").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "second".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    store.release_captured_open_turn_read();

    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(store.open_turn_read_attempts(), 1);
    assert_eq!(composition.calls.load(Ordering::Acquire), 1);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancelled_fresh_header_lookup_releases_its_exact_reservation() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.block_header_reads();
    let drops = Arc::new(AtomicUsize::new(0));
    let session_header = header("session-cancelled-fresh");
    let pin = AgentCompositionPin::new(
        session_header.agent_preset_id().clone(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(DropOwner(Arc::clone(&drops))),
    )
    .unwrap();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Fresh(
                        PreparedFreshSession::new(session_header, pin).unwrap(),
                    ),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while store.header_read_attempts() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "fresh header lookup did not block exactly once; observed {} attempts",
            store.header_read_attempts()
        )
    });
    first.abort();
    let _ = first.await;
    assert_eq!(drops.load(Ordering::Acquire), 1);
    store.release_blocked_header_reads();
    let worker = kernel.start_write_behind();

    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header("session-cancelled-fresh")),
            text: "retry".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("dropping the first lookup must release its reservation");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn failed_fresh_submission_releases_its_prepared_generation_pin() {
    let store = Arc::new(MemoryStore::new());
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_clock_and_limits(
        store_contract,
        composition(),
        Arc::new(FixedClock),
        KernelLimits {
            maximum_process_pending_fact_bytes: 1,
            ..KernelLimits::default()
        },
    )
    .await
    .unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let session_header = header("session-failed-fresh-pin");
    let session_id = session_header.session_id().clone();
    let pin = AgentCompositionPin::new(
        session_header.agent_preset_id().clone(),
        "a".repeat(64),
        Arc::new(EmptyTools),
        Arc::new(DropOwner(Arc::clone(&drops))),
    )
    .unwrap();

    assert_eq!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: SubmitSession::Fresh(
                    PreparedFreshSession::new(session_header, pin).unwrap(),
                ),
                text: "cannot fit".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(store.header(&session_id).await.is_err());
}

#[tokio::test]
async fn cancelled_hydration_leader_settles_followers_and_releases_capacity() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-cancelled-hydration", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.pause_next_open_turn_read();
    let leader = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-cancelled-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "leader".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let follower = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            let session_id = SessionId::new("session-cancelled-hydration").unwrap();
            let prepared = kernel.prepare_resume(&session_id).await?;
            kernel
                .submit(SubmitTurn {
                    turn_id: client_turn_id(),
                    session: SubmitSession::Resume(prepared),
                    text: "follower".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    leader.abort();
    let _ = leader.await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), follower)
            .await
            .expect("a cancelled leader must settle its followers")
            .unwrap()
            .is_err()
    );

    let worker = kernel.start_write_behind();
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: resume(
                &kernel,
                SessionId::new("session-cancelled-hydration").unwrap(),
            )
            .await,
            text: "retry".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("a later hydration attempt must be admitted");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cold_resume_resolves_its_header_before_resident_capacity_rejection() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-capacity-cold-resume", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel =
        SessionKernel::recover_with_clock(store_contract, composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_write_behind();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        submit(&kernel, &format!("session-resident-{index}"), "queued").await;
    }
    store.reset_header_read_attempts();

    assert_eq!(
        kernel
            .submit(SubmitTurn {
                turn_id: client_turn_id(),
                session: resume(
                    &kernel,
                    SessionId::new("session-capacity-cold-resume").unwrap(),
                )
                .await,
                text: "must resolve the durable preset first".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
    assert_eq!(
        store.header_read_attempts(),
        1,
        "cold resume must read its durable preset before resident admission"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn recovery_appends_interrupted_for_a_started_external_effect_and_never_requeues_it() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery").unwrap();
    let turn = TurnId::new("turn-recovery").unwrap();
    let effect = EffectId::new("effect-recovery").unwrap();
    let facts = vec![
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "hello".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
        SessionFact::new(
            2,
            2,
            SessionFactBody::ModelIntent {
                turn_id: turn.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::ModelStarted {
                turn_id: turn.clone(),
                effect_id: effect,
            },
        )
        .unwrap(),
    ];
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-recovery")),
            facts,
        })
        .await
        .unwrap();
    let kernel = kernel(store.clone()).await;
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Interrupted {
            effect: Some(EffectKind::Model),
            reason: "Kernel recovery found a turn without a durable terminal Fact".into(),
        })
    );
    let repaired = store.read_facts(&session, 3, 8).await.unwrap();
    assert_eq!(repaired.facts.len(), 1);
    assert!(matches!(
        repaired.facts[0].body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Interrupted {
                effect: Some(EffectKind::Model),
                ..
            },
            ..
        }
    ));
    let _lease = kernel.register("executor".into()).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(
        kernel
            .claim("executor", cancellation)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn startup_recovery_repairs_open_turns_without_resolving_the_preset() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-unavailable-preset").unwrap();
    let turn = TurnId::new("turn-recovery-unavailable-preset").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![accepted_fact(1, &turn)],
        })
        .await
        .unwrap();
    let composition = Arc::new(MutableComposition::new('a'));
    composition.set_unavailable();
    let composition_contract: Arc<dyn AgentComposition> = composition.clone();

    let kernel =
        SessionKernel::recover_with_clock(store, composition_contract, Arc::new(FixedClock))
            .await
            .expect("startup repair must not require an executable Agent preset");

    assert!(matches!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Interrupted { effect: None, .. })
    ));
    assert_eq!(composition.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn recovery_preserves_a_durable_cancellation_classification() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-cancelled").unwrap();
    let turn = TurnId::new("turn-recovery-cancelled").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-recovery-cancelled")),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn.clone(),
                        text: "hello".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::CancelRequested {
                        turn_id: turn.clone(),
                        reason: Some("stop".into()),
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();

    let kernel = kernel(store.clone()).await;
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    let repaired = store.read_facts(&session, 2, 8).await.unwrap();
    assert!(matches!(
        repaired.facts.as_slice(),
        [fact]
            if matches!(
                fact.body(),
                SessionFactBody::TurnTerminal {
                    outcome: TurnOutcome::Cancelled,
                    ..
                }
            )
    ));
}

#[tokio::test]
async fn recovery_rejects_usage_and_markers_that_exceed_the_frozen_budget() {
    let overused = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-budget-usage").unwrap();
    let turn = TurnId::new("turn-recovery-budget-usage").unwrap();
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();
    let bounded_header = SessionHeader::new(
        session.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new_with_budget(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
            budget,
        )
        .unwrap(),
    )
    .unwrap();
    overused
        .append(AppendBatch {
            session_id: session,
            expected_seq: 0,
            header: Some(bounded_header),
            facts: {
                let first = EffectId::new("effect-one").unwrap();
                let second = EffectId::new("effect-two").unwrap();
                vec![
                    accepted_fact(1, &turn),
                    model_intent_fact(2, &turn, &first),
                    model_started_fact(3, &turn, &first),
                    model_finished_fact(4, &turn, &first),
                    model_intent_fact(5, &turn, &second),
                ]
            },
        })
        .await
        .unwrap();
    let overused_store: Arc<dyn SessionStore> = overused;
    assert!(
        SessionKernel::recover_with_clock(overused_store, composition(), Arc::new(FixedClock),)
            .await
            .is_err(),
        "recovery must apply the immutable provider-attempt limit"
    );

    let mismatched = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-budget-marker").unwrap();
    let turn = TurnId::new("turn-recovery-budget-marker").unwrap();
    mismatched
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![
                accepted_fact(1, &turn),
                budget_fact(2, &turn, BudgetDimension::ProviderAttempts, 1, 1),
            ],
        })
        .await
        .unwrap();
    let mismatched_store: Arc<dyn SessionStore> = mismatched;
    assert!(
        SessionKernel::recover_with_clock(mismatched_store, composition(), Arc::new(FixedClock),)
            .await
            .is_err(),
        "a durable exhaustion marker must match the immutable budget"
    );
}

#[tokio::test]
async fn recovery_preserves_a_valid_durable_budget_classification() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-valid-budget").unwrap();
    let turn = TurnId::new("turn-recovery-valid-budget").unwrap();
    let effect = EffectId::new("effect-recovery-valid-budget").unwrap();
    let budget = TurnBudget::new(1_800_000, 1, 256, 65_536, 67_108_864).unwrap();
    let bounded_header = SessionHeader::new(
        session.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new_with_budget(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
            budget,
        )
        .unwrap(),
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(bounded_header),
            facts: vec![
                accepted_fact(1, &turn),
                model_intent_fact(2, &turn, &effect),
                model_started_fact(3, &turn, &effect),
                model_finished_fact(4, &turn, &effect),
                budget_fact(5, &turn, BudgetDimension::ProviderAttempts, 2, 1),
            ],
        })
        .await
        .unwrap();

    let kernel = kernel(store.clone()).await;
    let expected = TurnOutcome::BudgetExceeded {
        dimension: BudgetDimension::ProviderAttempts,
        consumed: 2,
        limit: 1,
    };
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(expected.clone())
    );
    assert!(matches!(
        store.read_facts(&session, 5, 8).await.unwrap().facts.as_slice(),
        [fact]
            if matches!(
                fact.body(),
                SessionFactBody::TurnTerminal { outcome, .. } if outcome == &expected
            )
    ));
}

#[tokio::test]
async fn ordinary_factory_waits_for_store_and_withdraws_all_turn_contracts() {
    let runtime = Runtime::default();
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
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    let store = Arc::new(MemoryStore::new());
    let store_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.store.memory",
                "store",
                UpdateMode::Replayable,
                Arc::new(MemoryStoreFactory::new(store)),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    let composition_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "test.agent.composition",
                "composition",
                UpdateMode::Replayable,
                Arc::new(TestCompositionFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_some()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .is_some()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnFinalizationContract>()
            .is_some()
    );
    assert!(kernel_fiber.dispose().await.is_clean());
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnFinalizationContract>()
            .is_none()
    );
    assert!(store_fiber.dispose().await.is_clean());
    assert!(composition_fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn finalizers_are_effect_owned_concurrent_and_resolve_failures_by_registration_order() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let make = |name, fail| {
        Arc::new(RecordingFinalizer {
            name,
            calls: calls.clone(),
            fail,
        }) as Arc<dyn TurnFinalizer>
    };
    let first = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "first".into(),
        make("first", false),
    )
    .unwrap();
    let failing = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "failing".into(),
        make("failing", true),
    )
    .unwrap();
    let _never = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "never".into(),
        make("never", false),
    )
    .unwrap();
    assert!(matches!(
        rsi_agent_turn_protocol::TurnFinalization::register(
            &kernel,
            "first".into(),
            make("duplicate", false)
        ),
        Err(TurnFinalizationError::Invalid(_))
    ));

    let session = SessionId::new("session-finalizers").unwrap();
    let turn = TurnId::new("turn-finalizers").unwrap();
    let context = TurnFinalizationContext {
        session_id: session,
        turn_id: turn,
        job_scope: None,
    };
    assert_eq!(
        rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context).await,
        Err(TurnFinalizationError::Failed {
            code: "test.failed".into(),
            message: "test finalizer failed".into(),
        })
    );
    let mut observed = calls.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, vec!["failing", "first", "never"]);

    calls.lock().unwrap().clear();
    drop(failing);
    let _replacement = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "failing".into(),
        make("replacement", false),
    )
    .unwrap();
    rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context)
        .await
        .unwrap();
    let mut observed = calls.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, vec!["first", "never", "replacement"]);

    calls.lock().unwrap().clear();
    drop(first);
    rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context)
        .await
        .unwrap();
    let mut observed = calls.lock().unwrap().clone();
    observed.sort_unstable();
    assert_eq!(observed, vec!["never", "replacement"]);
}

#[tokio::test]
async fn finalizer_snapshot_starts_every_hook_before_waiting_and_contains_panics() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let entered = Arc::new(AtomicUsize::new(0));
    let entered_changed = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let make = |fail| {
        Arc::new(CoordinatedFinalizer {
            entered: Arc::clone(&entered),
            entered_changed: Arc::clone(&entered_changed),
            release: Arc::clone(&release),
            fail,
        }) as Arc<dyn TurnFinalizer>
    };
    let one =
        rsi_agent_turn_protocol::TurnFinalization::register(&kernel, "one".into(), make(false))
            .unwrap();
    let two =
        rsi_agent_turn_protocol::TurnFinalization::register(&kernel, "two".into(), make(true))
            .unwrap();
    let three =
        rsi_agent_turn_protocol::TurnFinalization::register(&kernel, "three".into(), make(false))
            .unwrap();
    let context = TurnFinalizationContext {
        session_id: SessionId::new("session-concurrent-finalizers").unwrap(),
        turn_id: TurnId::new("turn-concurrent-finalizers").unwrap(),
        job_scope: None,
    };
    let concurrent_kernel = kernel.clone();
    let concurrent_context = context.clone();
    let finalization = tokio::spawn(async move {
        rsi_agent_turn_protocol::TurnFinalization::finalize(&concurrent_kernel, &concurrent_context)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let notified = entered_changed.notified();
            if entered.load(Ordering::Acquire) == 3 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("all finalizers must start concurrently");
    release.notify_waiters();
    assert!(matches!(
        finalization.await.unwrap(),
        Err(TurnFinalizationError::Failed { code, .. }) if code == "test.concurrent_failure"
    ));

    drop((one, two, three));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _panic = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "panic".into(),
        Arc::new(PanickingFinalizer),
    )
    .unwrap();
    let _after = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "after-panic".into(),
        Arc::new(RecordingFinalizer {
            name: "after-panic",
            calls: Arc::clone(&calls),
            fail: false,
        }),
    )
    .unwrap();
    assert!(matches!(
        rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &context).await,
        Err(TurnFinalizationError::Failed { code, .. }) if code == "turn.finalizer_panic"
    ));
    assert_eq!(*calls.lock().unwrap(), vec!["after-panic"]);
}
