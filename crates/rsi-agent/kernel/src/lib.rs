//! Durable Agent turn scheduler and write-behind ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::{
    FutureExt as _,
    stream::{self, StreamExt as _},
};
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentCompositionError, AgentCompositionPin,
    PreparedFreshSession,
};
use rsi_agent_session_protocol::{
    ActivationOutcome, AgentControlRecord, AgentControlRecordBody, AgentMessage,
    AgentMessageContent, AgentMessageSource, AgentPath, BudgetDimension, EffectId, EffectKind,
    ForkOrigin, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, MAXIMUM_DURABLE_AGENT_TREE_NODES,
    MAXIMUM_FACTS_PER_READ, MAXIMUM_PENDING_AGENT_MESSAGES, MAXIMUM_RUNNING_AGENT_TREE_NODES,
    MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_SESSION_HEADER_BYTES, MAXIMUM_TURN_TEXT_BYTES,
    MessageDiscardReason, MessageId, MessageOptions, MessageTarget, SessionFact, SessionFactBody,
    SessionHeader, SessionId, StepOutcome, TurnBudget, TurnId, TurnOutcome, WaitResumeCause,
    validate_identifier,
};
use rsi_agent_store_protocol::{
    AgentActivationGuard, AppendBatch, AppendCommit, AtomicAgentCommit, AtomicAgentCommitResult,
    AtomicSessionAppend, MAXIMUM_CONTEXT_CHECKPOINT_BYTES, MAXIMUM_SESSIONS_PER_READ,
    MAXIMUM_STORE_BATCH_BYTES, MAXIMUM_STORE_BATCH_FACTS, SessionStore, SessionStoreContract,
    StoreActivationPhase, StoreAgentChild, StoreAgentMessage, StoreAgentMessageState,
    StoreAgentSubtreeSnapshot, StoreError, StoredContextCheckpoint, WriteContextCheckpoint,
};
use rsi_agent_turn_protocol::{
    AgentCallerAuthority, AgentListScope, AgentNode, AgentNodeState, AgentWaitResult, CancelResult,
    CancelTarget, ClaimFactPage, ClaimMessage, ContextCheckpoint, ExecutorLease, ForkFactPage,
    MessageReceipt, MessageState, ObservationCursor, PreparedResumeSession, PublishAttempt,
    Result as TurnResult, ResumeAdmissionIssuer, SendAgentMessage, SessionObservation,
    SessionObservationStream, SpawnAgentRequest, SpawnedAgent, SubmitImage, SubmitMessage,
    SubmitSession, SubmitTurn, SubmittedTurn, TurnClaim, TurnClaimIssuer, TurnError, TurnExecution,
    TurnExecutionContract, TurnFinalization, TurnFinalizationContext, TurnFinalizationContract,
    TurnFinalizationError, TurnFinalizationReport, TurnFinalizer, TurnFinalizerLease,
    TurnObservation, TurnService, TurnServiceContract, TurnUpdate,
};
use rsi_agent_turn_protocol::{
    DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES, ObservationRetention, ObservedControl, ObservedFact,
};
use rsi_agent_turn_protocol::{SettlementHealth, SettlementSessionError};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{
    Mutex as AsyncMutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore, watch,
};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Maximum nonterminal turns retained by one session.
pub const MAXIMUM_LIVE_TURNS: usize = 256;
/// Maximum sessions with live or speculative state retained by one Kernel.
pub const MAXIMUM_ACTIVE_SESSIONS: usize = 256;
/// Maximum speculative Fact bytes retained between durable commits.
pub const MAXIMUM_PENDING_FACT_BYTES: usize = MAXIMUM_STORE_BATCH_BYTES;
/// Normal write-behind interval.
pub const WRITE_BEHIND_INTERVAL: Duration = Duration::from_millis(200);
const DURABLE_OBSERVER_FALLBACK_INTERVAL: Duration = Duration::from_secs(5);
const READY_SCHEDULER_FALLBACK_INTERVAL: Duration = Duration::from_secs(5);
const WAITING_SETTLEMENT_FALLBACK_INTERVAL: Duration = Duration::from_secs(5);
const MAXIMUM_NEXT_STEP_MESSAGE_PAYLOAD_BYTES: usize = MAXIMUM_STORE_BATCH_BYTES / 2;

fn rebase_write_behind_tick(scheduled: Instant, now: Instant) -> Instant {
    scheduled.max(now + WRITE_BEHIND_INTERVAL)
}
const MINIMUM_RETRY_BACKOFF: Duration = Duration::from_millis(100);
const MAXIMUM_RETRY_BACKOFF: Duration = Duration::from_secs(5);
const MAXIMUM_CONSECUTIVE_FLUSH_FAILURES: u32 = 8;
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);
const DURABILITY_WAIT_TIMEOUT: Duration = Duration::from_mins(1);
/// Default process-wide speculative Fact byte capacity.
pub const DEFAULT_MAXIMUM_PROCESS_PENDING_FACT_BYTES: usize = 64 * 1024 * 1024;
/// Default process-wide concurrent Store-read materialization capacity.
pub const DEFAULT_MAXIMUM_STORE_READ_BYTES: usize = 64 * 1024 * 1024;
/// Default number of simultaneously attached observers.
pub const DEFAULT_MAXIMUM_ACTIVE_OBSERVERS: usize = 1_024;

/// Process-wide Kernel resource limits; all defaults may only be tightened.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelLimits {
    /// Total speculative Fact bytes across all resident sessions.
    #[serde(default = "default_process_pending_fact_bytes")]
    pub maximum_process_pending_fact_bytes: usize,
    /// Total maximum-page reservations across concurrent Store reads.
    #[serde(default = "default_store_read_bytes")]
    pub maximum_store_read_bytes: usize,
    /// Canonical payload bytes retained by observation items and all their clones.
    #[serde(default = "default_retained_observation_bytes")]
    pub maximum_retained_observation_bytes: usize,
    /// Simultaneously attached live observations.
    #[serde(default = "default_active_observers")]
    pub maximum_active_observers: usize,
}

const fn default_process_pending_fact_bytes() -> usize {
    DEFAULT_MAXIMUM_PROCESS_PENDING_FACT_BYTES
}

const fn default_store_read_bytes() -> usize {
    DEFAULT_MAXIMUM_STORE_READ_BYTES
}

const fn default_retained_observation_bytes() -> usize {
    DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES
}

const fn default_active_observers() -> usize {
    DEFAULT_MAXIMUM_ACTIVE_OBSERVERS
}

impl Default for KernelLimits {
    fn default() -> Self {
        Self {
            maximum_process_pending_fact_bytes: default_process_pending_fact_bytes(),
            maximum_store_read_bytes: default_store_read_bytes(),
            maximum_active_observers: default_active_observers(),
            maximum_retained_observation_bytes: default_retained_observation_bytes(),
        }
    }
}

impl KernelLimits {
    /// Revalidates positive values no wider than the fixed process maxima.
    pub fn validate(&self) -> Result<()> {
        for (name, value, maximum) in [
            (
                "maximum_process_pending_fact_bytes",
                self.maximum_process_pending_fact_bytes,
                DEFAULT_MAXIMUM_PROCESS_PENDING_FACT_BYTES,
            ),
            (
                "maximum_store_read_bytes",
                self.maximum_store_read_bytes,
                DEFAULT_MAXIMUM_STORE_READ_BYTES,
            ),
            (
                "maximum_retained_observation_bytes",
                self.maximum_retained_observation_bytes,
                DEFAULT_MAXIMUM_RETAINED_OBSERVATION_BYTES,
            ),
            (
                "maximum_active_observers",
                self.maximum_active_observers,
                DEFAULT_MAXIMUM_ACTIVE_OBSERVERS,
            ),
        ] {
            if value == 0 || value > maximum {
                return Err(KernelError::Capacity(format!(
                    "{name} must be within 1..={maximum}"
                )));
            }
        }
        if self.maximum_retained_observation_bytes < MAXIMUM_SESSION_FACT_BYTES {
            return Err(KernelError::Capacity(
                "observation retention must admit one maximum Fact".into(),
            ));
        }
        if self.maximum_store_read_bytes < MAXIMUM_SESSION_FACT_BYTES {
            return Err(KernelError::Capacity(format!(
                "maximum_store_read_bytes must admit one maximum Fact ({MAXIMUM_SESSION_FACT_BYTES} bytes)"
            )));
        }
        Ok(())
    }
}

struct ValidatedKernelLimits(KernelLimits);

impl ValidatedKernelLimits {
    fn new(limits: KernelLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self(limits))
    }
}

/// Millisecond source injected into deterministic tests.
pub trait Clock: fmt::Debug + Send + Sync + 'static {
    /// Returns a nonzero Unix millisecond timestamp.
    fn now_ms(&self) -> u64;
}

/// Host wall clock used for durable Fact timestamps.
#[derive(Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
            .max(1)
    }
}

/// Cloneable in-process Kernel service.
#[derive(Clone)]
pub struct AgentKernel {
    inner: Arc<KernelInner>,
}

impl fmt::Debug for AgentKernel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentKernel")
            .finish_non_exhaustive()
    }
}

struct KernelInner {
    tasks: TaskTracker,
    store: Arc<dyn SessionStore>,
    evidence_cache: Mutex<evidence::Cache>,
    composition: Arc<dyn AgentComposition>,
    resume_issuer: ResumeAdmissionIssuer,
    claim_issuer: TurnClaimIssuer,
    clock: Arc<dyn Clock>,
    state: Mutex<KernelState>,
    submission_admission: SubmissionAdmission,
    commands: commands::CommandRequests,
    continuation_issuer: rsi_agent_turn_protocol::ContinuationIssuer,
    continuations:
        Mutex<BTreeMap<(SessionId, String), rsi_agent_turn_protocol::WeakContinuationLease>>,
    program_generation: String,
    programs: Mutex<BTreeMap<SessionId, Weak<program::LiveRun>>>,
    projection_admission: Arc<Semaphore>,
    ready_activation: Mutex<ready::ReadySchedulerState>,
    claim_changed: Notify,
    session_changes: SessionWatchHub,
    flush_requested: Notify,
    settlement_requested: Notify,
    settlement_health: Mutex<SettlementHealth>,
    stop_settlement: CancellationToken,
    stop_worker: CancellationToken,
    limits: KernelLimits,
    process_pending_bytes: AtomicUsize,
    process_pending_changed: Notify,
    observers: observer_resources::ObserverResources,
    observation_retention: ObservationRetention,
    store_read_admission: Arc<Semaphore>,
}

type SubmissionSessions = Mutex<BTreeMap<SessionId, Weak<SubmissionKey>>>;
const MAXIMUM_PENDING_SUBMISSIONS: usize = MAXIMUM_ACTIVE_SESSIONS;

struct SubmissionKey {
    id: SessionId,
    mutex: Arc<AsyncMutex<()>>,
    registry: Weak<SubmissionSessions>,
}

impl Drop for SubmissionKey {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut sessions = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if sessions
                .get(&self.id)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                sessions.remove(&self.id);
            }
        }
    }
}

// Fields drop in declaration order: unlock before withdrawing the key owner.
struct SubmissionGuard {
    _guard: OwnedMutexGuard<()>,
    _key: Arc<SubmissionKey>,
}

struct SubmissionAdmission {
    slots: Arc<Semaphore>,
    pending: Arc<Semaphore>,
    sessions: Arc<SubmissionSessions>,
    closed: CancellationToken,
}

impl SubmissionAdmission {
    fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(MAXIMUM_ACTIVE_SESSIONS)),
            pending: Arc::new(Semaphore::new(MAXIMUM_PENDING_SUBMISSIONS)),
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            closed: CancellationToken::new(),
        }
    }

    async fn acquire(&self, session: &SessionId) -> TurnResult<SubmissionAdmissionLease> {
        self.acquire_until(session, None, &self.closed).await
    }

    // Only an already admitted mutation may enter after producer shutdown.
    async fn acquire_retained(
        &self,
        session: &SessionId,
        _proof: &mutation::AgentMutationLease,
    ) -> TurnResult<SubmissionAdmissionLease> {
        self.acquire_until(session, None, &CancellationToken::new())
            .await
    }

    async fn acquire_pair(
        &self,
        session: &SessionId,
        parent: Option<&SessionId>,
    ) -> TurnResult<SubmissionAdmissionLease> {
        self.acquire_until(session, parent, &self.closed).await
    }

    fn register(&self, id: &SessionId) -> Arc<SubmissionKey> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(key) = sessions.get(id).and_then(Weak::upgrade) {
            return key;
        }
        let key = Arc::new(SubmissionKey {
            id: id.clone(),
            mutex: Arc::new(AsyncMutex::new(())),
            registry: Arc::downgrade(&self.sessions),
        });
        sessions.insert(id.clone(), Arc::downgrade(&key));
        key
    }

    async fn acquire_until(
        &self,
        session: &SessionId,
        parent: Option<&SessionId>,
        closed: &CancellationToken,
    ) -> TurnResult<SubmissionAdmissionLease> {
        if closed.is_cancelled() {
            return Err(TurnError::ShuttingDown);
        }
        let _pending = Arc::clone(&self.pending)
            .try_acquire_owned()
            .map_err(|_| TurnError::Capacity)?;
        let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
        let (first, second) = match parent {
            Some(parent) if parent < session => (parent, Some(session)),
            Some(parent) if parent > session => (session, Some(parent)),
            _ => (session, None),
        };
        let mut guards = Vec::with_capacity(1 + usize::from(second.is_some()));
        for id in std::iter::once(first).chain(second) {
            let key = self.register(id);
            let guard = tokio::select! {
                biased;
                () = closed.cancelled() => return Err(TurnError::ShuttingDown),
                result = tokio::time::timeout_at(deadline, Arc::clone(&key.mutex).lock_owned()) => {
                    result.map_err(|_| TurnError::Capacity)?
                }
            };
            guards.push(SubmissionGuard {
                _guard: guard,
                _key: key,
            });
        }
        let slot = tokio::select! {
            biased;
            () = closed.cancelled() => return Err(TurnError::ShuttingDown),
            result = tokio::time::timeout_at(deadline, Arc::clone(&self.slots).acquire_owned()) => {
                match result {
                    Ok(Ok(slot)) => slot,
                    Ok(Err(_)) => return Err(TurnError::ShuttingDown),
                    Err(_) => return Err(TurnError::Capacity),
                }
            }
        };
        Ok(SubmissionAdmissionLease {
            _guards: guards,
            _slot: slot,
        })
    }

    fn close(&self) {
        self.closed.cancel();
    }
}

struct SubmissionAdmissionLease {
    _guards: Vec<SubmissionGuard>,
    _slot: OwnedSemaphorePermit,
}

struct KernelState {
    accepting: bool,
    controlled_work: controlled_work::Registry,
    sessions: BTreeMap<SessionId, SessionRuntime>,
    loading_sessions: BTreeMap<SessionId, Arc<SessionLoad>>,
    fresh_reservations: BTreeSet<SessionId>,
    executors: BTreeMap<String, u64>,
    next_executor_registration: u64,
    finalizers: finalization::Registry,
    tree_lanes: BTreeMap<SessionId, Weak<Semaphore>>,
    next_claim: u64,
    claim_queue: VecDeque<(SessionId, TurnId)>,
    queued: BTreeSet<(SessionId, TurnId)>,
}

struct SessionLoad {
    result: Mutex<Option<TurnResult<()>>>,
    completed: Notify,
}

struct FreshReservationGuard {
    inner: Arc<KernelInner>,
    session_id: SessionId,
}

struct ResumeAdmissionGuard {
    inner: Arc<KernelInner>,
    session_id: SessionId,
}

impl Drop for ResumeAdmissionGuard {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner);
        let remove = state
            .sessions
            .get_mut(&self.session_id)
            .is_some_and(|session| {
                debug_assert!(session.admission_reservations > 0);
                session.admission_reservations = session.admission_reservations.saturating_sub(1);
                session.admission_reservations == 0
                    && session.turns.is_empty()
                    && session.pending.is_empty()
                    && !session.header_pending
                    && !session.flush_inflight
            });
        if remove {
            state.sessions.remove(&self.session_id);
        }
    }
}

impl FreshReservationGuard {
    fn new(inner: &Arc<KernelInner>, session_id: SessionId) -> Self {
        Self {
            inner: Arc::clone(inner),
            session_id,
        }
    }
}

impl Drop for FreshReservationGuard {
    fn drop(&mut self) {
        lock_state(&self.inner)
            .fresh_reservations
            .remove(&self.session_id);
    }
}

struct SessionLoadGuard {
    inner: Arc<KernelInner>,
    session_id: SessionId,
    load: Arc<SessionLoad>,
    armed: bool,
}

impl SessionLoadGuard {
    fn new(inner: &Arc<KernelInner>, session_id: SessionId, load: Arc<SessionLoad>) -> Self {
        Self {
            inner: Arc::clone(inner),
            session_id,
            load,
            armed: true,
        }
    }

    fn complete(mut self, result: TurnResult<()>) {
        self.load.complete(result);
        let mut state = lock_state(&self.inner);
        if state
            .loading_sessions
            .get(&self.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.load))
        {
            state.loading_sessions.remove(&self.session_id);
        }
        self.armed = false;
    }
}

impl Drop for SessionLoadGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.load.complete(Err(TurnError::Invariant(
            "session hydration owner was cancelled before completion".into(),
        )));
        let mut state = lock_state(&self.inner);
        if state
            .loading_sessions
            .get(&self.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.load))
        {
            state.loading_sessions.remove(&self.session_id);
        }
    }
}

impl SessionLoad {
    fn pending() -> Self {
        Self {
            result: Mutex::new(None),
            completed: Notify::new(),
        }
    }

    async fn wait(&self) -> TurnResult<()> {
        loop {
            let completed = self.completed.notified();
            if let Some(result) = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return result;
            }
            completed.await;
        }
    }

    fn complete(&self, result: TurnResult<()>) {
        let mut slot = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(result);
            self.completed.notify_waiters();
        }
    }
}

struct SessionRuntime {
    header: Arc<SessionHeader>,
    composition: AgentCompositionPin,
    durable_seq: u64,
    pending: VecDeque<Arc<SessionFact>>,
    pending_bytes: usize,
    header_pending: bool,
    pending_domain_baseline: Option<AgentControlRecord>,
    turns: BTreeMap<TurnId, TurnControl>,
    turn_order: Vec<TurnId>,
    updates: watch::Sender<LiveWatermarks>,
    flush_status: watch::Sender<FlushStatus>,
    flush_inflight: bool,
    retry_failures: u32,
    retry_not_before: Option<Instant>,
    permanent_flush_error: Option<String>,
    admission_reservations: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FlushStatus {
    durable_seq: u64,
    permanent_error: Option<String>,
}

struct DurabilityWait {
    status: watch::Receiver<FlushStatus>,
    through_seq: u64,
}

impl DurabilityWait {
    fn new(session: &SessionRuntime, through_seq: u64) -> Self {
        Self {
            status: session.flush_status.subscribe(),
            through_seq,
        }
    }
}

struct PreparedFlushBatch {
    session_id: SessionId,
    expected_seq: u64,
    header: Option<SessionHeader>,
    facts: Vec<Arc<SessionFact>>,
    baseline: Option<AgentControlRecord>,
}

impl PreparedFlushBatch {
    fn into_store_batch(self) -> (AppendBatch, Option<AgentControlRecord>) {
        (
            AppendBatch {
                session_id: self.session_id,
                expected_seq: self.expected_seq,
                header: self.header,
                facts: self.facts,
            },
            self.baseline,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveWatermarks {
    live_seq: u64,
    durable_seq: u64,
}

struct TurnControl {
    initial_messages: BTreeSet<MessageId>,
    claim_composition: Option<AgentCompositionPin>,
    program_roles: Arc<BTreeMap<String, rsi_tools_protocol::ToolProgramRole>>,
    conclusion: Option<(u64, rsi_agent_session_protocol::ToolConclusion)>,
    evidence_inline_bytes: usize,
    tool_source: Option<Arc<tool_origin::ToolSource>>,
    seen_model_effects: Arc<BTreeSet<EffectId>>,
    elapsed: Arc<elapsed::ElapsedState>,
    accepted_at_ms: u64,
    accepted_seq: u64,
    activation_id: Option<rsi_agent_session_protocol::ActivationId>,
    current_step: Option<rsi_agent_session_protocol::StepId>,
    terminal: Option<TurnOutcome>,
    terminal_seq: Option<u64>,
    cancel_requested: bool,
    cancellation: CancellationToken,
    claim: Option<ClaimOwner>,
    prepared_lane: Option<Arc<TreeClaimLane>>,
    effects: BTreeMap<EffectId, ActiveEffect>,
    budget_usage: BudgetUsage,
    budget_exhausted: Option<(BudgetDimension, u64, u64)>,
}

#[derive(Clone)]
struct DurableMessageEntry {
    delivery: rsi_agent_session_protocol::MessageDelivery,
    message: AgentMessage,
    encoded_message_bytes: usize,
    root_session_id: SessionId,
    target: MessageTarget,
    wake_required: bool,
    accepted_control_seq: u64,
    state: MessageState,
}

struct DurableMessageScan {
    selected: Option<DurableMessageEntry>,
    pending_count: usize,
    pending: Vec<DurableMessageEntry>,
    durable_control_seq: u64,
    durable_fact_seq: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BudgetUsage {
    provider_attempts: u64,
    tool_calls: u64,
    generated_records: u64,
    generated_record_bytes: u64,
}

#[derive(Clone)]
struct ClaimOwner {
    jobs: Option<Weak<dyn rsi_agent_turn_protocol::TurnJobStatusSource>>,
    executor: String,
    registration: u64,
    claim: u64,
    live_seq: u64,
    mutations: Arc<mutation::ClaimMutationGate>,
    tree_lane: Arc<TreeClaimLane>,
}

struct TreeClaimLane {
    pool: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

#[derive(Clone)]
enum ActiveEffect {
    Model {
        purpose: rsi_agent_session_protocol::ModelEventPurpose,
        effect_id: EffectId,
        started: bool,
    },
    Image {
        effect_id: EffectId,
        started: bool,
        next_index: u32,
    },
    Tool {
        name: String,
        source_selection: Arc<rsi_agent_session_protocol::ModelSelection>,
        effect_id: EffectId,
        identity: rsi_tools_protocol::ToolResultIdentity,
        started: bool,
        parallel_safe: bool,
        origin: rsi_agent_session_protocol::ToolOrigin,
        program_role: rsi_tools_protocol::ToolProgramRole,
        next_program_ordinal: u32,
    },
}

impl TurnControl {
    fn new(accepted_at_ms: u64, accepted_seq: u64) -> Self {
        Self {
            initial_messages: BTreeSet::new(),
            claim_composition: None,
            program_roles: Arc::new(BTreeMap::new()),
            conclusion: None,
            tool_source: None,
            seen_model_effects: Arc::new(BTreeSet::new()),
            evidence_inline_bytes: 0,
            elapsed: Arc::new(elapsed::ElapsedState::default()),
            accepted_at_ms,
            accepted_seq,
            activation_id: None,
            current_step: None,
            terminal: None,
            terminal_seq: None,
            cancel_requested: false,
            cancellation: CancellationToken::new(),
            claim: None,
            prepared_lane: None,
            effects: BTreeMap::new(),
            budget_usage: BudgetUsage::default(),
            budget_exhausted: None,
        }
    }
}

impl SessionRuntime {
    fn new(
        header: SessionHeader,
        composition: AgentCompositionPin,
        durable_seq: u64,
        header_pending: bool,
    ) -> Self {
        let (updates, _) = watch::channel(LiveWatermarks {
            live_seq: durable_seq,
            durable_seq,
        });
        let (flush_status, _) = watch::channel(FlushStatus {
            durable_seq,
            permanent_error: None,
        });
        Self {
            header: Arc::new(header),
            composition,
            durable_seq,
            pending: VecDeque::new(),
            pending_bytes: 0,
            header_pending,
            pending_domain_baseline: None,
            turns: BTreeMap::new(),
            turn_order: Vec::new(),
            updates,
            flush_status,
            flush_inflight: false,
            retry_failures: 0,
            retry_not_before: None,
            permanent_flush_error: None,
            admission_reservations: 0,
        }
    }

    fn live_seq(&self) -> Result<u64> {
        self.durable_seq
            .checked_add(
                u64::try_from(self.pending.len())
                    .map_err(|_| KernelError::Invariant("pending Fact count exceeds u64".into()))?,
            )
            .ok_or_else(|| KernelError::Invariant("live Fact sequence exhausted".into()))
    }

    fn oldest_claimable(&self) -> Option<&TurnId> {
        for turn_id in &self.turn_order {
            let turn = self.turns.get(turn_id)?;
            if turn.terminal.is_none() {
                return Some(turn_id);
            }
            if turn
                .terminal_seq
                .is_some_and(|terminal_seq| terminal_seq > self.durable_seq)
            {
                return None;
            }
        }
        None
    }
}

fn apply_committed_flush(
    session: &mut SessionRuntime,
    commit: AppendCommit,
    process_pending_bytes: &AtomicUsize,
) -> Vec<TurnId> {
    let committed_cancellations = session
        .pending
        .iter()
        .take_while(|fact| fact.seq() <= commit.durable_seq)
        .filter_map(|fact| match fact.body() {
            SessionFactBody::CancelRequested { turn_id, .. } => session
                .turns
                .get(turn_id)
                .map(|turn| turn.cancellation.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    while session
        .pending
        .front()
        .is_some_and(|fact| fact.seq() <= commit.durable_seq)
    {
        let fact = session.pending.pop_front().expect("front existed");
        session.pending_bytes = session.pending_bytes.saturating_sub(fact.encoded_len());
        process_pending_bytes.fetch_sub(fact.encoded_len(), Ordering::AcqRel);
    }
    session.durable_seq = commit.durable_seq;
    session.header_pending = false;
    session.pending_domain_baseline = None;
    session.retry_failures = 0;
    session.retry_not_before = None;
    let _previous = session.flush_status.send_replace(FlushStatus {
        durable_seq: commit.durable_seq,
        permanent_error: session.permanent_flush_error.clone(),
    });
    publish_live_watermarks(session);
    for cancellation in committed_cancellations {
        cancellation.cancel();
    }
    let pruned_turns = session
        .turns
        .iter()
        .filter(|(_, turn)| {
            turn.terminal_seq
                .is_some_and(|terminal_seq| terminal_seq <= commit.durable_seq)
        })
        .map(|(turn_id, _)| turn_id.clone())
        .collect::<Vec<_>>();
    for turn_id in &pruned_turns {
        session.turns.remove(turn_id);
    }
    session
        .turn_order
        .retain(|turn_id| !pruned_turns.contains(turn_id));
    pruned_turns
}

mod admission;
mod commands;
mod continuation;
mod contributions;
mod controlled_work;
mod domains;
mod elapsed;
mod ending;
mod evidence;
mod execution;
mod finalization;
mod human_wait;
mod jobs;
mod lifecycle;
mod notifications;
mod program;
mod projection;
mod resource;
mod structured;
use notifications::{SessionWatch, SessionWatchHub};
mod completion_reply;
mod observation;
mod recovery;
mod store_reads;
mod tool_origin;
mod turn_service;
mod turn_state;

use observation::{
    activation_outcome, activation_terminal_controls, agent_root_and_path,
    bounded_step_message_prefix, completion_message, completion_message_id,
    context_checkpoints_enabled, control_tail, descendant_session_ids, durable_observation_next,
    fill_observation_page, list_agent_descendants, list_direct_agent_children, message_receipt,
    observation_next, observe_agent_wait_change, read_facts_bounded, read_fork_page_from_header,
    read_header_bounded, read_observed_facts, read_turn_boundary_bounded, read_turn_facts_bounded,
    read_validated_header_bounded, scan_durable_messages,
};
use recovery::{
    is_terminal_fact, load_control_state, read_stored_outcome, repair_unfinished_session,
    validate_durable_intent_fence,
};
use turn_state::{
    apply_executor_body, apply_recovered_fact, bounded_diagnostic, canonicalize_terminal,
    clone_turn_control, deregister_executor, enforce_turn_budget, enqueue, kernel_turn_error,
    lock_state, next_fact, publish_live_watermarks, push_pending, reserve_atomic_capacity,
    submission_conflict, turn_composition_error, turn_kernel_error, turn_not_found,
    turn_store_error,
};

struct ObservationState {
    inner: Weak<KernelInner>,
    session_id: SessionId,
    cursor: u64,
    durable_target: u64,
    live_target: u64,
    receiver: watch::Receiver<LiveWatermarks>,
    flush_status: Option<watch::Receiver<FlushStatus>>,
    durable_facts: VecDeque<ObservedFact>,
    ended: bool,
    _observer_lease: ObserverLease,
}

enum ObservationPageKind {
    Control,
    Fact,
}

struct DurableObservationState {
    inner: Weak<KernelInner>,
    session_id: SessionId,
    control_seq: u64,
    fact_seq: u64,
    pending: VecDeque<SessionObservation>,
    watch: SessionWatch,
    next_page: ObservationPageKind,
    read_controls: bool,
    read_facts: bool,
    stopped: bool,
    _observer_lease: ObserverLease,
}

mod observer_resources;
use observer_resources::{ObserverKind, ObserverLease};
pub use observer_resources::{ObserverSnapshot, ObserverUsage};

enum ObservationSignal {
    Update(std::result::Result<(), watch::error::RecvError>),
    Flush(std::result::Result<(), watch::error::RecvError>),
}

/// Closed Kernel construction and durable-worker failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum KernelError {
    /// Session protocol rejected an internally constructed durable value.
    #[error(transparent)]
    Session(#[from] rsi_agent_session_protocol::SessionError),
    /// Mechanical Store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A durable session's Agent preset could not produce a healthy generation.
    #[error("Agent composition failed: {0}")]
    Composition(String),
    /// Configured process-wide resource admission was exhausted or invalid.
    #[error("Agent Kernel capacity failed: {0}")]
    Capacity(String),
    /// Speculative suffix could not become durable.
    #[error("Agent flush failed: {0}")]
    Flush(String),
    /// Kernel state became contradictory.
    #[error("Agent Kernel invariant failed: {0}")]
    Invariant(String),
    /// Bounded final shutdown failed.
    #[error("Agent Kernel shutdown failed: {0}")]
    Shutdown(String),
}

/// Kernel result.
pub type Result<T> = std::result::Result<T, KernelError>;

/// Ordinary Kernel factory requiring exact Agent Store and composition supplies.
#[derive(Clone, Debug, Default)]
pub struct KernelFactory;

#[async_trait]
impl PluginFactory for KernelFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let limits = if desired.is_null() {
            KernelLimits::default()
        } else {
            serde_json::from_value::<KernelLimits>(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        let validated = ValidatedKernelLimits::new(limits)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let config = serde_json::to_value(limits)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(PreparedActivation::with_state(
            config,
            validated,
            std::mem::size_of::<ValidatedKernelLimits>(),
        )
        .requiring_local::<SessionStoreContract>()
        .requiring_local::<AgentCompositionContract>())
    }

    #[allow(clippy::too_many_lines)] // The generation publishes its related services with one fail-closed worker shutdown path.
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let limits = plan.take_state::<ValidatedKernelLimits>()?;
        let kernel = AgentKernel::recover_with_validated_limits(
            plan.local::<SessionStoreContract>()?,
            plan.local::<AgentCompositionContract>()?,
            Arc::new(SystemClock),
            limits,
        )
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        lock_state(&kernel.inner).finalizers.runtime = Some(plan.context().runtime_identity());
        let worker = kernel.start_workers();
        let turns: Arc<dyn TurnService> = Arc::new(kernel.clone());
        let execution: Arc<dyn TurnExecution> = Arc::new(kernel.clone());
        let finalization: Arc<dyn TurnFinalization> = Arc::new(kernel.clone());
        let turns_supply = match plan.context().provide_local::<TurnServiceContract>(turns) {
            Ok(supply) => supply,
            Err(error) => {
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let execution_supply = match plan
            .context()
            .provide_local::<TurnExecutionContract>(execution)
        {
            Ok(supply) => supply,
            Err(error) => {
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let finalization_supply = match plan
            .context()
            .provide_local::<TurnFinalizationContract>(finalization)
        {
            Ok(supply) => supply,
            Err(error) => {
                drop(execution_supply);
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let commands_supply = match plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::SessionCommandsContract>(Arc::new(
                kernel.clone(),
            )) {
            Ok(supply) => supply,
            Err(error) => {
                drop(finalization_supply);
                drop(execution_supply);
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let projections_supply = match plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::SessionProjectionsContract>(
            Arc::new(kernel.clone()),
        ) {
            Ok(supply) => supply,
            Err(error) => {
                drop(commands_supply);
                drop(finalization_supply);
                drop(execution_supply);
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let continuations_supply = match plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::SessionContinuationsContract>(
            Arc::new(kernel.clone()),
        ) {
            Ok(supply) => supply,
            Err(error) => {
                drop(projections_supply);
                drop(commands_supply);
                drop(finalization_supply);
                drop(execution_supply);
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let jobs_supply = match plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::TurnJobsContract>(Arc::new(kernel.clone()))
        {
            Ok(supply) => supply,
            Err(error) => {
                drop(continuations_supply);
                drop(projections_supply);
                drop(commands_supply);
                drop(finalization_supply);
                drop(execution_supply);
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        let resources_supply = match plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::SessionResourcesContract>(Arc::new(
                kernel.clone(),
            )) {
            Ok(supply) => supply,
            Err(error) => {
                drop(jobs_supply);
                drop(continuations_supply);
                drop(projections_supply);
                drop(commands_supply);
                drop(finalization_supply);
                drop(execution_supply);
                drop(turns_supply);
                let _ignored = kernel.shutdown(worker).await;
                return Err(error);
            }
        };
        plan.defer(
            "shutdown Agent Kernel",
            Box::new(move || {
                Box::pin(async move {
                    drop(resources_supply);
                    drop(jobs_supply);
                    drop(continuations_supply);
                    drop(projections_supply);
                    drop(commands_supply);
                    drop(finalization_supply);
                    drop(execution_supply);
                    drop(turns_supply);
                    kernel
                        .shutdown(worker)
                        .await
                        .map_err(|error| error.to_string())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests;

mod mutation;

/// Owned handles for the independent flush and settlement workers.
#[derive(Debug)]
pub struct KernelWorkers {
    flush: JoinHandle<()>,
    settlement: JoinHandle<()>,
}

impl KernelWorkers {
    /// Stops and joins the workers without a final flush, as on an abrupt stop.
    /// Admitted commit tasks retain their ownership independently.
    pub async fn abort(self) {
        self.flush.abort();
        self.settlement.abort();
        let _ = self.flush.await;
        let _ = self.settlement.await;
    }
}

mod ready;
