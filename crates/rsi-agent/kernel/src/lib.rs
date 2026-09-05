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
    ForkOrigin, InputMessageSource, MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
    MAXIMUM_DURABLE_AGENT_TREE_NODES, MAXIMUM_FACTS_PER_READ, MAXIMUM_PENDING_AGENT_MESSAGES,
    MAXIMUM_RUNNING_AGENT_TREE_NODES, MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_SESSION_HEADER_BYTES,
    MAXIMUM_TURN_TEXT_BYTES, MessageDiscardReason, MessageId, MessageOptions, MessageTarget,
    SessionFact, SessionFactBody, SessionHeader, SessionId, StepOutcome, TurnBudget, TurnId,
    TurnOutcome, WaitResumeCause, validate_identifier,
};
use rsi_agent_store_protocol::{
    AgentActivationGuard, AppendBatch, AppendCommit, AtomicAgentCommit, AtomicAgentCommitResult,
    AtomicSessionAppend, MAXIMUM_CONTEXT_CHECKPOINT_BYTES, MAXIMUM_SESSIONS_PER_READ,
    MAXIMUM_STORE_BATCH_BYTES, MAXIMUM_STORE_BATCH_FACTS, SessionStore, SessionStoreContract,
    StoreActivationPhase, StoreAgentChild, StoreAgentMessage, StoreAgentMessageState,
    StoreDescendantControlSnapshot, StoreError, StoredContextCheckpoint, WriteContextCheckpoint,
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
use rsi_agent_workspace_context::{
    WorkspaceContext, WorkspaceContextContract, WorkspaceContextSnapshot,
};
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

const fn default_active_observers() -> usize {
    DEFAULT_MAXIMUM_ACTIVE_OBSERVERS
}

impl Default for KernelLimits {
    fn default() -> Self {
        Self {
            maximum_process_pending_fact_bytes: default_process_pending_fact_bytes(),
            maximum_store_read_bytes: default_store_read_bytes(),
            maximum_active_observers: default_active_observers(),
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
        if self.maximum_store_read_bytes < MAXIMUM_SESSION_FACT_BYTES {
            return Err(KernelError::Capacity(format!(
                "maximum_store_read_bytes must admit one maximum Fact ({MAXIMUM_SESSION_FACT_BYTES} bytes)"
            )));
        }
        Ok(())
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
pub struct SessionKernel {
    inner: Arc<KernelInner>,
}

impl fmt::Debug for SessionKernel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionKernel")
            .finish_non_exhaustive()
    }
}

struct KernelInner {
    store: Arc<dyn SessionStore>,
    composition: Arc<dyn AgentComposition>,
    workspace_context: Arc<dyn WorkspaceContext>,
    resume_issuer: ResumeAdmissionIssuer,
    claim_issuer: TurnClaimIssuer,
    clock: Arc<dyn Clock>,
    state: Mutex<KernelState>,
    submission_admission: SubmissionAdmission,
    ready_activation: AsyncMutex<Option<SessionId>>,
    claim_changed: Notify,
    flush_requested: Notify,
    settlement_requested: Notify,
    stop_worker: CancellationToken,
    limits: KernelLimits,
    process_pending_bytes: AtomicUsize,
    process_pending_changed: Notify,
    active_observers: AtomicUsize,
    store_read_admission: Arc<Semaphore>,
}

struct SubmissionAdmission {
    slots: Arc<Semaphore>,
    sessions: Mutex<BTreeMap<SessionId, Weak<AsyncMutex<()>>>>,
    closed: CancellationToken,
}

impl SubmissionAdmission {
    fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(MAXIMUM_ACTIVE_SESSIONS)),
            sessions: Mutex::new(BTreeMap::new()),
            closed: CancellationToken::new(),
        }
    }

    async fn acquire(&self, session_id: &SessionId) -> TurnResult<SubmissionAdmissionLease> {
        let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions.retain(|_, admission| admission.strong_count() > 0);
            if let Some(admission) = sessions.get(session_id).and_then(Weak::upgrade) {
                admission
            } else {
                let admission = Arc::new(AsyncMutex::new(()));
                sessions.insert(session_id.clone(), Arc::downgrade(&admission));
                admission
            }
        };
        let guard = tokio::select! {
            biased;
            () = self.closed.cancelled() => return Err(TurnError::ShuttingDown),
            result = tokio::time::timeout_at(deadline, session.lock_owned()) => {
                result.map_err(|_| TurnError::Capacity)?
            }
        };
        let slot = tokio::select! {
            biased;
            () = self.closed.cancelled() => return Err(TurnError::ShuttingDown),
            result = tokio::time::timeout_at(deadline, Arc::clone(&self.slots).acquire_owned()) => {
                match result {
                    Ok(Ok(slot)) => slot,
                    Ok(Err(_)) => return Err(TurnError::ShuttingDown),
                    Err(_) => return Err(TurnError::Capacity),
                }
            }
        };
        Ok(SubmissionAdmissionLease {
            _slot: slot,
            _guard: guard,
        })
    }

    async fn acquire_many(
        &self,
        session_ids: impl IntoIterator<Item = SessionId>,
    ) -> TurnResult<Vec<SubmissionAdmissionLease>> {
        let mut session_ids = session_ids.into_iter().collect::<BTreeSet<_>>();
        let mut leases = Vec::with_capacity(session_ids.len());
        while let Some(session_id) = session_ids.pop_first() {
            leases.push(self.acquire(&session_id).await?);
        }
        Ok(leases)
    }

    fn close(&self) {
        self.closed.cancel();
        self.slots.close();
    }
}

struct SubmissionAdmissionLease {
    _slot: OwnedSemaphorePermit,
    _guard: OwnedMutexGuard<()>,
}

struct KernelState {
    accepting: bool,
    sessions: BTreeMap<SessionId, SessionRuntime>,
    loading_sessions: BTreeMap<SessionId, Arc<SessionLoad>>,
    fresh_reservations: BTreeSet<SessionId>,
    executors: BTreeMap<String, u64>,
    next_executor_registration: u64,
    finalizers: BTreeMap<u64, FinalizerEntry>,
    finalizer_names: BTreeSet<String>,
    next_finalizer_registration: u64,
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

#[derive(Clone)]
struct FinalizerEntry {
    name: String,
    finalizer: Arc<dyn TurnFinalizer>,
}

struct SessionRuntime {
    header: Arc<SessionHeader>,
    composition: AgentCompositionPin,
    durable_seq: u64,
    pending: VecDeque<Arc<SessionFact>>,
    pending_bytes: usize,
    header_pending: bool,
    turns: BTreeMap<TurnId, TurnControl>,
    turn_order: Vec<TurnId>,
    updates: watch::Sender<LiveWatermarks>,
    flush_status: watch::Sender<FlushStatus>,
    flush_inflight: bool,
    retry_failures: u32,
    retry_not_before: Option<Instant>,
    permanent_flush_error: Option<String>,
    admission_reservations: usize,
    workspace_context: WorkspaceContextState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WorkspaceContextState {
    instructions_sha256: Option<String>,
    skill_catalog_sha256: Option<String>,
}

#[derive(Debug)]
struct EmptyWorkspaceContext;

#[async_trait]
impl WorkspaceContext for EmptyWorkspaceContext {
    async fn snapshot(
        &self,
        _header: &SessionHeader,
        _messages: &[&AgentMessage],
    ) -> std::result::Result<
        WorkspaceContextSnapshot,
        rsi_agent_workspace_context::WorkspaceContextError,
    > {
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct FlushStatus {
    durable_seq: u64,
    permanent_error: Option<String>,
}

struct PreparedFlushBatch {
    session_id: SessionId,
    expected_seq: u64,
    header: Option<SessionHeader>,
    facts: Vec<Arc<SessionFact>>,
}

impl PreparedFlushBatch {
    fn into_store_batch(self) -> AppendBatch {
        AppendBatch {
            session_id: self.session_id,
            expected_seq: self.expected_seq,
            header: self.header,
            facts: self
                .facts
                .into_iter()
                .map(|fact| fact.as_ref().clone())
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveWatermarks {
    live_seq: u64,
    durable_seq: u64,
}

struct TurnControl {
    accepted_at_ms: u64,
    accepted_seq: u64,
    activation_id: Option<rsi_agent_session_protocol::ActivationId>,
    current_step: Option<rsi_agent_session_protocol::StepId>,
    terminal: Option<TurnOutcome>,
    terminal_seq: Option<u64>,
    cancel_requested: bool,
    cancellation: CancellationToken,
    claim: Option<ClaimOwner>,
    effects: BTreeMap<EffectId, ActiveEffect>,
    budget_usage: BudgetUsage,
    budget_exhausted: Option<(BudgetDimension, u64, u64)>,
}

#[derive(Clone)]
struct DurableMessageEntry {
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
    generated_facts: u64,
    generated_fact_bytes: u64,
}

#[derive(Clone)]
struct ClaimOwner {
    executor: String,
    registration: u64,
    claim: u64,
    live_seq: u64,
    tree_lane: Arc<TreeClaimLane>,
}

struct TreeClaimLane {
    pool: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

#[derive(Clone)]
enum ActiveEffect {
    Model {
        effect_id: EffectId,
        started: bool,
    },
    Image {
        effect_id: EffectId,
        started: bool,
        next_index: u32,
    },
    Tool {
        effect_id: EffectId,
        identity: rsi_tools_protocol::ToolResultIdentity,
        started: bool,
        parallel_safe: bool,
    },
}

impl TurnControl {
    fn new(accepted_at_ms: u64, accepted_seq: u64) -> Self {
        Self {
            accepted_at_ms,
            accepted_seq,
            activation_id: None,
            current_step: None,
            terminal: None,
            terminal_seq: None,
            cancel_requested: false,
            cancellation: CancellationToken::new(),
            claim: None,
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
            turns: BTreeMap::new(),
            turn_order: Vec::new(),
            updates,
            flush_status,
            flush_inflight: false,
            retry_failures: 0,
            retry_not_before: None,
            permanent_flush_error: None,
            admission_reservations: 0,
            workspace_context: WorkspaceContextState::default(),
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
    session.retry_failures = 0;
    session.retry_not_before = None;
    let _previous = session.flush_status.send_replace(FlushStatus {
        durable_seq: commit.durable_seq,
        permanent_error: None,
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
mod execution;
mod lifecycle;
mod observation;
mod recovery;
mod turn_service;
mod turn_state;

use observation::{
    activation_outcome, activation_terminal_controls, agent_root_and_path,
    apply_workspace_context_state, bounded_step_message_prefix, completion_message,
    completion_message_id, context_checkpoints_enabled, control_tail, descendant_session_ids,
    durable_agent_node_state, durable_observation_next, entered_message_source,
    list_agent_descendants, list_direct_agent_children, message_receipt, observation_next,
    observe_agent_wait_change, read_controls_bounded, read_facts_bounded,
    read_fork_page_from_header, read_header_bounded, read_turn_boundary_bounded,
    read_turn_facts_bounded, ready_sessions_for_root, scan_durable_messages,
    workspace_context_bodies,
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
    durable_facts: VecDeque<Arc<SessionFact>>,
    ended: bool,
    _observer_lease: ObserverLease,
}

struct DurableObservationState {
    inner: Weak<KernelInner>,
    session_id: SessionId,
    control_seq: u64,
    fact_seq: u64,
    pending: VecDeque<SessionObservation>,
    stopped: bool,
    _observer_lease: ObserverLease,
}

struct ObserverLease {
    inner: Weak<KernelInner>,
}

impl ObserverLease {
    fn acquire(inner: &Arc<KernelInner>) -> TurnResult<Self> {
        let mut current = inner.active_observers.load(Ordering::Acquire);
        loop {
            if current >= inner.limits.maximum_active_observers {
                return Err(TurnError::ObserverCapacity);
            }
            match inner.active_observers.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(Self {
                        inner: Arc::downgrade(inner),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for ObserverLease {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.active_observers.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

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
        limits
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let config = serde_json::to_value(limits)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(PreparedActivation::new(config)
            .requiring_local::<SessionStoreContract>()
            .requiring_local::<AgentCompositionContract>()
            .requiring_local::<WorkspaceContextContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let limits: KernelLimits = serde_json::from_value(plan.config().as_ref().clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let workspace_context = plan.local::<WorkspaceContextContract>()?;
        let kernel = SessionKernel::recover_with_context_clock_and_limits(
            plan.local::<SessionStoreContract>()?,
            plan.local::<AgentCompositionContract>()?,
            workspace_context,
            Arc::new(SystemClock),
            limits,
        )
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let worker = kernel.start_write_behind();
        let turns: Arc<dyn TurnService> = Arc::new(kernel.clone());
        let execution: Arc<dyn TurnExecution> = Arc::new(kernel.clone());
        let finalization: Arc<dyn TurnFinalization> = Arc::new(kernel.clone());
        let turns_supply = match plan.context().provide_local::<TurnServiceContract>(turns) {
            Ok(supply) => supply,
            Err(error) => {
                kernel.inner.stop_worker.cancel();
                let _ignored = worker.await;
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
                kernel.inner.stop_worker.cancel();
                let _ignored = worker.await;
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
                kernel.inner.stop_worker.cancel();
                let _ignored = worker.await;
                return Err(error);
            }
        };
        plan.defer(
            "shutdown Agent Kernel",
            Box::new(move || {
                Box::pin(async move {
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
