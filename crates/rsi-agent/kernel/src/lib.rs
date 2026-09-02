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
};
use rsi_agent_session_protocol::{
    BudgetDimension, EffectId, EffectKind, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, MAXIMUM_FACTS_PER_READ,
    MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_SESSION_HEADER_BYTES, MAXIMUM_TURN_TEXT_BYTES, SessionFact,
    SessionFactBody, SessionHeader, SessionId, TurnBudget, TurnId, TurnOutcome,
    validate_identifier,
};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, MAXIMUM_CONTEXT_CHECKPOINT_BYTES, MAXIMUM_SESSIONS_PER_READ,
    MAXIMUM_STORE_BATCH_BYTES, MAXIMUM_STORE_BATCH_FACTS, SessionStore, SessionStoreContract,
    StoreError, StoredContextCheckpoint, WriteContextCheckpoint,
};
use rsi_agent_turn_protocol::{
    CancelResult, ClaimFactPage, ContextCheckpoint, ExecutorLease, PreparedResumeSession,
    PublishAttempt, Result as TurnResult, ResumeAdmissionIssuer, SubmitImage, SubmitSession,
    SubmitTurn, SubmittedTurn, TurnClaim, TurnClaimIssuer, TurnError, TurnExecution,
    TurnExecutionContract, TurnFinalization, TurnFinalizationContext, TurnFinalizationContract,
    TurnFinalizationError, TurnFinalizationReport, TurnFinalizer, TurnFinalizerLease,
    TurnObservation, TurnService, TurnServiceContract, TurnUpdate,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
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
    resume_issuer: ResumeAdmissionIssuer,
    claim_issuer: TurnClaimIssuer,
    clock: Arc<dyn Clock>,
    state: Mutex<KernelState>,
    submission_admission: SubmissionAdmission,
    claim_changed: Notify,
    flush_requested: Notify,
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
    armed: bool,
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
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for FreshReservationGuard {
    fn drop(&mut self) {
        if self.armed {
            lock_state(&self.inner)
                .fresh_reservations
                .remove(&self.session_id);
        }
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
    terminal: Option<TurnOutcome>,
    terminal_seq: Option<u64>,
    cancel_requested: bool,
    cancellation: CancellationToken,
    claim: Option<ClaimOwner>,
    effect: Option<ActiveEffect>,
    budget_usage: BudgetUsage,
    budget_exhausted: Option<(BudgetDimension, u64, u64)>,
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
    },
}

impl TurnControl {
    fn new(accepted_at_ms: u64, accepted_seq: u64) -> Self {
        Self {
            accepted_at_ms,
            accepted_seq,
            terminal: None,
            terminal_seq: None,
            cancel_requested: false,
            cancellation: CancellationToken::new(),
            claim: None,
            effect: None,
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

impl SessionKernel {
    /// Recovers every durable session and repairs unfinished tails before return.
    pub async fn recover(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
    ) -> Result<Self> {
        Self::recover_with_clock(store, composition, Arc::new(SystemClock)).await
    }

    /// Recovers with a deterministic timestamp source.
    pub async fn recover_with_clock(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        Self::recover_with_clock_and_limits(store, composition, clock, KernelLimits::default())
            .await
    }

    /// Recovers with a deterministic clock and explicit process-wide limits.
    pub async fn recover_with_clock_and_limits(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
        clock: Arc<dyn Clock>,
        limits: KernelLimits,
    ) -> Result<Self> {
        limits.validate()?;
        let mut after = None;
        loop {
            let page = store
                .list_open_sessions(after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
                .await?;
            let has_more = page.has_more;
            let next_after = page.sessions.last().cloned();
            for session_id in page.sessions {
                repair_unfinished_session(&store, clock.as_ref(), &session_id).await?;
            }
            if !has_more {
                break;
            }
            after = Some(next_after.ok_or_else(|| {
                KernelError::Invariant("session enumeration made no progress".into())
            })?);
        }
        Ok(Self {
            inner: Arc::new(KernelInner {
                store,
                composition,
                resume_issuer: ResumeAdmissionIssuer::new(),
                claim_issuer: TurnClaimIssuer::new(),
                clock,
                state: Mutex::new(KernelState {
                    accepting: true,
                    sessions: BTreeMap::new(),
                    loading_sessions: BTreeMap::new(),
                    fresh_reservations: BTreeSet::new(),
                    executors: BTreeMap::new(),
                    next_executor_registration: 0,
                    finalizers: BTreeMap::new(),
                    finalizer_names: BTreeSet::new(),
                    next_finalizer_registration: 0,
                    next_claim: 0,
                    claim_queue: VecDeque::new(),
                    queued: BTreeSet::new(),
                }),
                submission_admission: SubmissionAdmission::new(),
                claim_changed: Notify::new(),
                flush_requested: Notify::new(),
                stop_worker: CancellationToken::new(),
                limits,
                process_pending_bytes: AtomicUsize::new(0),
                process_pending_changed: Notify::new(),
                active_observers: AtomicUsize::new(0),
                store_read_admission: Arc::new(Semaphore::new(limits.maximum_store_read_bytes)),
            }),
        })
    }

    /// Starts the sole background write-behind worker.
    pub fn start_write_behind(&self) -> JoinHandle<()> {
        let kernel = self.clone();
        let first_tick = Instant::now() + WRITE_BEHIND_INTERVAL;
        tokio::spawn(async move { kernel.flush_loop(first_tick).await })
    }

    /// Stops admission, durably drains pending Facts, ends the worker, and
    /// releases resident generation pins.
    pub async fn shutdown(&self, mut worker: JoinHandle<()>) -> Result<()> {
        {
            let mut state = lock_state(&self.inner);
            state.accepting = false;
        }
        self.inner.submission_admission.close();
        self.inner.claim_changed.notify_waiters();
        self.inner.flush_requested.notify_waiters();
        let flush_result =
            match tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, self.flush_every_session()).await {
                Ok(result) => result,
                Err(_) => Err(KernelError::Shutdown("final flush timed out".into())),
            };
        self.inner.stop_worker.cancel();
        self.inner.flush_requested.notify_waiters();
        let worker_result = if let Ok(result) =
            tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, &mut worker).await
        {
            result.map_err(|error| KernelError::Shutdown(format!("flush worker failed: {error}")))
        } else {
            worker.abort();
            let _ = worker.await;
            Err(KernelError::Shutdown(
                "flush worker did not stop before the shutdown deadline".into(),
            ))
        };
        self.quiesce();
        flush_result.and(worker_result)
    }

    fn quiesce(&self) {
        let (sessions, loads, finalizers) = {
            let mut state = lock_state(&self.inner);
            let sessions = std::mem::take(&mut state.sessions);
            let loads = std::mem::take(&mut state.loading_sessions)
                .into_values()
                .collect::<Vec<_>>();
            let finalizers = std::mem::take(&mut state.finalizers);
            state.fresh_reservations.clear();
            state.executors.clear();
            state.finalizer_names.clear();
            state.claim_queue.clear();
            state.queued.clear();
            (sessions, loads, finalizers)
        };
        self.inner.process_pending_bytes.store(0, Ordering::Release);
        self.inner.process_pending_changed.notify_waiters();
        for session in sessions.values() {
            for turn in session.turns.values() {
                turn.cancellation.cancel();
            }
        }
        for load in loads {
            load.complete(Err(TurnError::ShuttingDown));
        }
        self.inner.claim_changed.notify_waiters();
        drop(sessions);
        drop(finalizers);
    }

    async fn flush_loop(self, mut next_tick: Instant) {
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(next_tick) => {
                }
                () = self.inner.flush_requested.notified() => {}
                () = self.inner.stop_worker.cancelled() => break,
            }
            self.flush_ready_sessions().await;
            next_tick = rebase_write_behind_tick(next_tick, Instant::now());
        }
    }

    async fn flush_ready_sessions(&self) {
        let session_ids = {
            let state = lock_state(&self.inner);
            state.sessions.keys().cloned().collect::<Vec<_>>()
        };
        for session_id in session_ids {
            let Some(prepared) = self.prepare_flush_batch(&session_id) else {
                continue;
            };
            let batch = prepared.into_store_batch();
            let result = self.inner.store.append(batch).await;
            self.complete_flush(&session_id, result);
        }
    }

    fn prepare_flush_batch(&self, session_id: &SessionId) -> Option<PreparedFlushBatch> {
        let mut state = lock_state(&self.inner);
        let session = state.sessions.get_mut(session_id)?;
        if session.flush_inflight
            || session.pending.is_empty()
            || session.permanent_flush_error.is_some()
            || session
                .retry_not_before
                .is_some_and(|deadline| deadline > Instant::now())
        {
            return None;
        }
        let mut facts = Vec::new();
        let mut bytes = 0_usize;
        for fact in &session.pending {
            if facts.len() == MAXIMUM_STORE_BATCH_FACTS {
                break;
            }
            let encoded = fact.encoded_len();
            if !facts.is_empty() && bytes.saturating_add(encoded) > MAXIMUM_STORE_BATCH_BYTES {
                break;
            }
            bytes = bytes.saturating_add(encoded);
            facts.push(Arc::clone(fact));
        }
        session.flush_inflight = true;
        Some(PreparedFlushBatch {
            session_id: session_id.clone(),
            expected_seq: session.durable_seq,
            header: session
                .header_pending
                .then(|| session.header.as_ref().clone()),
            facts,
        })
    }

    fn complete_flush(
        &self,
        session_id: &SessionId,
        result: std::result::Result<rsi_agent_store_protocol::AppendCommit, StoreError>,
    ) {
        let mut request_more = false;
        let mut enqueue_after_commit = false;
        let mut claim_available = false;
        let mut pruned_turns = Vec::new();
        let mut evict_session = false;
        let mut released_process_capacity = false;
        let mut latched_permanent_failure = false;
        {
            let mut state = lock_state(&self.inner);
            let Some(session) = state.sessions.get_mut(session_id) else {
                return;
            };
            session.flush_inflight = false;
            match result {
                Ok(commit) => {
                    pruned_turns =
                        apply_committed_flush(session, commit, &self.inner.process_pending_bytes);
                    released_process_capacity = true;
                    enqueue_after_commit = true;
                    request_more = !session.pending.is_empty();
                    evict_session = session.admission_reservations == 0
                        && session.turns.is_empty()
                        && session.pending.is_empty();
                }
                Err(StoreError::Io(_)) => {
                    session.retry_failures = session.retry_failures.saturating_add(1);
                    if session.retry_failures >= MAXIMUM_CONSECUTIVE_FLUSH_FAILURES {
                        session.permanent_flush_error = Some(format!(
                            "Store append failed {MAXIMUM_CONSECUTIVE_FLUSH_FAILURES} consecutive times"
                        ));
                        latched_permanent_failure = true;
                        let _previous = session.flush_status.send_replace(FlushStatus {
                            durable_seq: session.durable_seq,
                            permanent_error: session.permanent_flush_error.clone(),
                        });
                    } else {
                        let shift = session.retry_failures.saturating_sub(1).min(6);
                        let multiplier = 1_u32 << shift;
                        let backoff = MINIMUM_RETRY_BACKOFF
                            .checked_mul(multiplier)
                            .unwrap_or(MAXIMUM_RETRY_BACKOFF)
                            .min(MAXIMUM_RETRY_BACKOFF);
                        session.retry_not_before = Some(Instant::now() + backoff);
                    }
                }
                Err(error) => {
                    session.permanent_flush_error = Some(error.to_string());
                    latched_permanent_failure = true;
                    let _previous = session.flush_status.send_replace(FlushStatus {
                        durable_seq: session.durable_seq,
                        permanent_error: session.permanent_flush_error.clone(),
                    });
                }
            }
            if enqueue_after_commit {
                let next = session.oldest_claimable().cloned();
                let _ = session;
                if let Some(turn_id) = next {
                    enqueue(&mut state, session_id.clone(), turn_id);
                    claim_available = true;
                }
            }
            for turn_id in &pruned_turns {
                state.queued.remove(&(session_id.clone(), turn_id.clone()));
            }
            if !pruned_turns.is_empty() {
                state.claim_queue.retain(|(queued_session, queued_turn)| {
                    queued_session != session_id || !pruned_turns.contains(queued_turn)
                });
            }
            if evict_session {
                state.sessions.remove(session_id);
            }
        }
        if claim_available {
            self.inner.claim_changed.notify_waiters();
        }
        if released_process_capacity || latched_permanent_failure {
            self.inner.process_pending_changed.notify_waiters();
        }
        if request_more {
            self.inner.flush_requested.notify_one();
        }
    }

    async fn flush_every_session(&self) -> Result<()> {
        let targets = {
            let state = lock_state(&self.inner);
            state
                .sessions
                .iter()
                .map(|(session_id, session)| {
                    session
                        .live_seq()
                        .map(|seq| (session_id.clone(), session.flush_status.subscribe(), seq))
                })
                .collect::<Result<Vec<_>>>()?
        };
        self.inner.flush_requested.notify_one();
        let mut failures = Vec::new();
        for (session_id, status, through_seq) in targets {
            if let Err(error) = self.wait_on_flush_status(status, through_seq).await {
                failures.push(format!("{}: {error}", session_id.as_str()));
            }
        }
        if !failures.is_empty() {
            let count = failures.len();
            let first = failures.remove(0);
            return Err(KernelError::Shutdown(format!(
                "{count} session flush(es) failed; first failure: {first}"
            )));
        }
        Ok(())
    }

    async fn wait_for_durable(&self, session_id: &SessionId, through_seq: u64) -> Result<u64> {
        let status = {
            let state = lock_state(&self.inner);
            flush_status_receiver(&state, session_id)?
        };
        self.inner.flush_requested.notify_one();
        self.wait_on_flush_status(status, through_seq).await
    }

    async fn wait_on_flush_status(
        &self,
        mut status: watch::Receiver<FlushStatus>,
        through_seq: u64,
    ) -> Result<u64> {
        let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
        loop {
            let current = status.borrow().clone();
            if current.durable_seq >= through_seq {
                return Ok(current.durable_seq);
            }
            if let Some(error) = current.permanent_error {
                return Err(KernelError::Flush(error));
            }
            tokio::select! {
                changed = status.changed() => {
                    changed.map_err(|_| KernelError::Shutdown("flush status closed".into()))?;
                }
                () = self.inner.stop_worker.cancelled() => {
                    return Err(KernelError::Shutdown("flush worker stopped".into()));
                }
                () = tokio::time::sleep_until(deadline) => {
                    return Err(KernelError::Flush(format!(
                        "durability wait timed out after {} seconds",
                        DURABILITY_WAIT_TIMEOUT.as_secs()
                    )));
                }
            }
        }
    }

    fn validate_claim<'a>(
        &self,
        state: &'a KernelState,
        claim: &TurnClaim,
    ) -> TurnResult<&'a TurnControl> {
        if !self.inner.claim_issuer.validates(claim) {
            return Err(TurnError::StaleClaim);
        }
        let registration_id = state
            .executors
            .get(claim.executor_id())
            .copied()
            .ok_or(TurnError::StaleClaim)?;
        let session = state
            .sessions
            .get(claim.session_id())
            .ok_or(TurnError::StaleClaim)?;
        let turn = session
            .turns
            .get(claim.turn_id())
            .ok_or(TurnError::StaleClaim)?;
        match &turn.claim {
            Some(owner)
                if owner.executor == claim.executor_id()
                    && owner.registration == registration_id
                    && owner.claim == claim.claim_id()
                    && owner.live_seq == claim.live_seq()
                    && turn.accepted_at_ms == claim.accepted_at_ms()
                    && turn.accepted_seq == claim.accepted_seq()
                    && self
                        .inner
                        .claim_issuer
                        .validates_header(claim, &session.header) =>
            {
                Ok(turn)
            }
            _ => Err(TurnError::StaleClaim),
        }
    }

    fn validate_issued_claim(&self, claim: &TurnClaim) -> TurnResult<()> {
        self.inner
            .claim_issuer
            .validates(claim)
            .then_some(())
            .ok_or(TurnError::StaleClaim)
    }

    async fn reserve_fresh_session(
        &self,
        header: &SessionHeader,
        durable_absence_known: bool,
    ) -> TurnResult<()> {
        let session_id = header.session_id();
        {
            let mut state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            if state.sessions.contains_key(session_id)
                || state.loading_sessions.contains_key(session_id)
                || state.fresh_reservations.contains(session_id)
            {
                return Err(TurnError::Invalid(
                    "fresh submission selected an existing or reserved session".into(),
                ));
            }
            if state
                .sessions
                .len()
                .saturating_add(state.loading_sessions.len())
                .saturating_add(state.fresh_reservations.len())
                >= MAXIMUM_ACTIVE_SESSIONS
            {
                return Err(TurnError::Capacity);
            }
            state.fresh_reservations.insert(session_id.clone());
        }
        let reservation = FreshReservationGuard::new(&self.inner, session_id.clone());
        if durable_absence_known {
            reservation.disarm();
            return Ok(());
        }
        match read_header_bounded(&self.inner, session_id).await {
            Err(StoreError::NotFound(_)) => {
                reservation.disarm();
                Ok(())
            }
            Ok(_) => Err(TurnError::Invalid(
                "fresh submission selected an existing session".into(),
            )),
            Err(error) => Err(turn_store_error(error)),
        }
    }

    async fn prepare_resume_session(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<PreparedResumeSession> {
        loop {
            let concurrent_load = {
                let state = lock_state(&self.inner);
                if !state.accepting {
                    return Err(TurnError::ShuttingDown);
                }
                if let Some(session) = state.sessions.get(session_id) {
                    return self
                        .inner
                        .resume_issuer
                        .issue(session.header.as_ref().clone(), session.composition.clone());
                }
                if state.fresh_reservations.contains(session_id) {
                    return Err(TurnError::Invalid(
                        "resume selected a session that is still being created".into(),
                    ));
                }
                state.loading_sessions.get(session_id).cloned()
            };
            if let Some(load) = concurrent_load {
                load.wait().await?;
                continue;
            }

            let header = read_header_bounded(&self.inner, session_id)
                .await
                .map_err(turn_store_error)?;
            let composition = match self.inner.composition.pin(header.agent_preset_id()).await {
                Ok(composition) => composition,
                Err(error) => {
                    let concurrent_load = {
                        let state = lock_state(&self.inner);
                        if !state.accepting {
                            return Err(TurnError::ShuttingDown);
                        }
                        if let Some(session) = state.sessions.get(session_id) {
                            return self.inner.resume_issuer.issue(
                                session.header.as_ref().clone(),
                                session.composition.clone(),
                            );
                        }
                        if state.fresh_reservations.contains(session_id) {
                            return Err(TurnError::Invalid(
                                "resume selected a session that is still being created".into(),
                            ));
                        }
                        state.loading_sessions.get(session_id).cloned()
                    };
                    if let Some(load) = concurrent_load {
                        load.wait().await?;
                        continue;
                    }
                    return Err(turn_composition_error(error));
                }
            };

            let concurrent_load = {
                let state = lock_state(&self.inner);
                if !state.accepting {
                    return Err(TurnError::ShuttingDown);
                }
                if let Some(session) = state.sessions.get(session_id) {
                    return self
                        .inner
                        .resume_issuer
                        .issue(session.header.as_ref().clone(), session.composition.clone());
                }
                if state.fresh_reservations.contains(session_id) {
                    return Err(TurnError::Invalid(
                        "resume selected a session that is still being created".into(),
                    ));
                }
                state.loading_sessions.get(session_id).cloned()
            };
            if let Some(load) = concurrent_load {
                load.wait().await?;
                continue;
            }
            return self.inner.resume_issuer.issue(header, composition);
        }
    }

    async fn ensure_prepared_session_loaded(
        &self,
        prepared: &PreparedResumeSession,
    ) -> TurnResult<()> {
        let (header, composition) = self.inner.resume_issuer.inspect(prepared)?;
        let header = header.clone();
        let composition = composition.clone();
        let session_id = header.session_id().clone();
        let (load, leader) = {
            let mut state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            if state.sessions.contains_key(&session_id) {
                return Ok(());
            }
            if state.fresh_reservations.contains(&session_id) {
                return Err(TurnError::Invalid(
                    "resume selected a session that is still being created".into(),
                ));
            }
            if let Some(load) = state.loading_sessions.get(&session_id) {
                (Arc::clone(load), false)
            } else {
                if state
                    .sessions
                    .len()
                    .saturating_add(state.loading_sessions.len())
                    .saturating_add(state.fresh_reservations.len())
                    >= MAXIMUM_ACTIVE_SESSIONS
                {
                    return Err(TurnError::Capacity);
                }
                let load = Arc::new(SessionLoad::pending());
                state
                    .loading_sessions
                    .insert(session_id.clone(), Arc::clone(&load));
                (load, true)
            }
        };
        if !leader {
            return load.wait().await;
        }
        let load_guard = SessionLoadGuard::new(&self.inner, session_id.clone(), Arc::clone(&load));
        let budget = header.settings().turn_budget().clone();
        let loaded = load_control_state(&self.inner.store, Some(&self.inner), &session_id, &budget)
            .await
            .map_err(turn_kernel_error);
        let result = {
            let mut state = lock_state(&self.inner);
            if !state.accepting {
                Err(TurnError::ShuttingDown)
            } else if state.sessions.contains_key(&session_id) {
                Ok(())
            } else {
                match loaded {
                    Err(error) => Err(error),
                    Ok((durable_seq, turns, turn_order)) => {
                        let mut session =
                            SessionRuntime::new(header, composition, durable_seq, false);
                        session.turns = turns;
                        session.turn_order = turn_order;
                        let queued = session.turn_order.clone();
                        state.sessions.insert(session_id.clone(), session);
                        for turn_id in queued {
                            enqueue(&mut state, session_id.clone(), turn_id);
                        }
                        Ok(())
                    }
                }
            }
        };
        load_guard.complete(result.clone());
        if result.is_ok() {
            self.inner.claim_changed.notify_waiters();
        }
        result
    }

    async fn ensure_session_loaded(&self, session_id: &SessionId) -> TurnResult<()> {
        let prepared = self.prepare_resume_session(session_id).await?;
        self.ensure_prepared_session_loaded(&prepared).await
    }

    async fn reserve_resume_submission(
        &self,
        prepared: &PreparedResumeSession,
    ) -> TurnResult<ResumeAdmissionGuard> {
        let (header, _) = self.inner.resume_issuer.inspect(prepared)?;
        let session_id = header.session_id().clone();
        loop {
            self.ensure_prepared_session_loaded(prepared).await?;
            let mut state = lock_state(&self.inner);
            let Some(session) = state.sessions.get_mut(&session_id) else {
                continue;
            };
            session.admission_reservations = session
                .admission_reservations
                .checked_add(1)
                .ok_or_else(|| {
                    TurnError::Invariant("resume admission reservation count overflowed".into())
                })?;
            return Ok(ResumeAdmissionGuard {
                inner: Arc::clone(&self.inner),
                session_id,
            });
        }
    }

    fn accept_turn(
        &self,
        session_selection: SubmitSession,
        turn_id: TurnId,
        body: SessionFactBody,
    ) -> TurnResult<SubmittedTurn> {
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let session_id = session_selection.session_id().clone();
        let mut state = lock_state(&self.inner);
        if !state.accepting {
            if matches!(&session_selection, SubmitSession::Fresh(_)) {
                state.fresh_reservations.remove(&session_id);
            }
            return Err(TurnError::ShuttingDown);
        }
        let inserted_fresh = matches!(&session_selection, SubmitSession::Fresh(_));
        match session_selection {
            SubmitSession::Fresh(prepared) => {
                let (header, composition) = prepared.into_parts();
                if state.sessions.contains_key(&session_id)
                    || !state.fresh_reservations.remove(&session_id)
                {
                    return Err(TurnError::Invalid(
                        "fresh submission lacks its exact resident reservation".into(),
                    ));
                }
                state.sessions.insert(
                    session_id.clone(),
                    SessionRuntime::new(header, composition, 0, true),
                );
            }
            SubmitSession::Resume(prepared) => {
                let _parts = self.inner.resume_issuer.consume(prepared)?;
                if !state.sessions.contains_key(&session_id) {
                    return Err(TurnError::SessionNotFound(session_id.to_string()));
                }
            }
        }
        let staged = (|| {
            let session = state
                .sessions
                .get_mut(&session_id)
                .expect("fresh was inserted and resume was checked");
            if let Some(error) = &session.permanent_flush_error {
                return Err(TurnError::Flush(error.clone()));
            }
            let live_turns = session
                .turns
                .values()
                .filter(|turn| turn.terminal.is_none())
                .count();
            if live_turns >= MAXIMUM_LIVE_TURNS {
                return Err(TurnError::Capacity);
            }
            if session.turns.contains_key(&turn_id) {
                return Err(TurnError::Invariant(
                    "duplicate turn identity escaped submission retry handling".into(),
                ));
            }
            let fact = next_fact(&self.inner, session, body).map_err(turn_kernel_error)?;
            push_pending(&self.inner, session, fact.clone()).map_err(turn_kernel_error)?;
            Ok(fact)
        })();
        let fact = match staged {
            Ok(fact) => fact,
            Err(error) => {
                if inserted_fresh {
                    state.sessions.remove(&session_id);
                }
                return Err(error);
            }
        };
        let accepted_seq = fact.seq();
        let session = state
            .sessions
            .get_mut(&session_id)
            .expect("accepted session exists");
        session.turns.insert(
            turn_id.clone(),
            TurnControl::new(fact.timestamp_ms(), accepted_seq),
        );
        session.turn_order.push(turn_id.clone());
        publish_live_watermarks(session);
        enqueue(&mut state, session_id.clone(), turn_id.clone());
        drop(state);
        self.inner.claim_changed.notify_waiters();
        Ok(SubmittedTurn {
            session_id,
            turn_id,
            accepted_seq,
        })
    }

    async fn existing_submission(
        &self,
        header: &SessionHeader,
        turn_id: &TurnId,
        body: &SessionFactBody,
        header_is_durable: bool,
    ) -> TurnResult<(Option<(SubmittedTurn, bool)>, bool)> {
        let session_id = header.session_id();
        {
            let state = lock_state(&self.inner);
            if let Some(session) = state.sessions.get(session_id)
                && let Some(turn) = session.turns.get(turn_id)
            {
                if session.header.as_ref() != header {
                    return Err(submission_conflict(session_id, turn_id));
                }
                if turn.accepted_seq > session.durable_seq {
                    let accepted = session
                        .pending
                        .iter()
                        .find(|fact| fact.seq() == turn.accepted_seq)
                        .ok_or_else(|| {
                            TurnError::Invariant(
                                "live submission acceptance is absent from the pending suffix"
                                    .into(),
                            )
                        })?;
                    if accepted.body() != body {
                        return Err(submission_conflict(session_id, turn_id));
                    }
                    return Ok((
                        Some((
                            SubmittedTurn {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                accepted_seq: turn.accepted_seq,
                            },
                            true,
                        )),
                        true,
                    ));
                }
            } else if state.sessions.contains_key(session_id) {
                // A resident session may have pruned this turn's durable
                // terminal entry; fall through to the indexed Store read.
            }
        }

        if !header_is_durable {
            let durable_header = match read_header_bounded(&self.inner, session_id).await {
                Ok(header) => header,
                Err(StoreError::NotFound(_)) => return Ok((None, false)),
                Err(error) => return Err(turn_store_error(error)),
            };
            if &durable_header != header {
                return Err(submission_conflict(session_id, turn_id));
            }
        }
        let boundary = match read_turn_boundary_bounded(&self.inner, session_id, turn_id).await {
            Ok(boundary) => boundary,
            Err(StoreError::NotFound(_) | StoreError::TurnNotFound { .. }) => {
                return Ok((None, true));
            }
            Err(error) => return Err(turn_store_error(error)),
        };
        let accepted_seq = boundary.accepted_seq();
        let (_, accepted, _, _) = boundary.into_parts();
        if accepted.body() != body {
            return Err(submission_conflict(session_id, turn_id));
        }
        Ok((
            Some((
                SubmittedTurn {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    accepted_seq,
                },
                false,
            )),
            true,
        ))
    }

    async fn submit_body(
        &self,
        session: SubmitSession,
        turn_id: TurnId,
        body: SessionFactBody,
    ) -> TurnResult<SubmittedTurn> {
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if let SubmitSession::Resume(prepared) = &session {
            self.inner.resume_issuer.inspect(prepared)?;
        }
        let header = session.header();
        let submission_admission = self
            .inner
            .submission_admission
            .acquire(header.session_id())
            .await?;
        let header_is_durable = matches!(&session, SubmitSession::Resume(_));
        let (existing, durable_session_exists) = self
            .existing_submission(header, &turn_id, &body, header_is_durable)
            .await?;
        if let Some((receipt, pending)) = existing {
            drop(submission_admission);
            if pending {
                self.wait_for_durable(&receipt.session_id, receipt.accepted_seq)
                    .await
                    .map_err(turn_kernel_error)?;
            }
            return Ok(receipt);
        }
        let resume_admission = match &session {
            SubmitSession::Fresh(_) => None,
            SubmitSession::Resume(prepared) => {
                Some(self.reserve_resume_submission(prepared).await?)
            }
        };
        if let SubmitSession::Fresh(prepared) = &session {
            self.reserve_fresh_session(prepared.header(), !durable_session_exists)
                .await?;
        }
        let result = self.accept_turn(session, turn_id, body);
        drop(resume_admission);
        drop(submission_admission);
        let receipt = result?;
        self.wait_for_durable(&receipt.session_id, receipt.accepted_seq)
            .await
            .map_err(turn_kernel_error)?;
        Ok(receipt)
    }
}

fn flush_status_receiver(
    state: &KernelState,
    session_id: &SessionId,
) -> Result<watch::Receiver<FlushStatus>> {
    if let Some(session) = state.sessions.get(session_id) {
        return Ok(session.flush_status.subscribe());
    }
    if !state.accepting {
        return Err(KernelError::Shutdown(
            "session was released while the Kernel was shutting down".into(),
        ));
    }
    Err(KernelError::Invariant(
        "session disappeared during flush".into(),
    ))
}

#[async_trait]
impl TurnService for SessionKernel {
    async fn prepare_resume(&self, session_id: &SessionId) -> TurnResult<PreparedResumeSession> {
        self.prepare_resume_session(session_id).await
    }

    async fn submit(&self, request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        let SubmitTurn {
            session,
            turn_id,
            text,
            model,
            sandbox,
        } = request;
        if text.is_empty() || text.len() > MAXIMUM_TURN_TEXT_BYTES {
            return Err(TurnError::Invalid(format!(
                "turn text must contain 1..={MAXIMUM_TURN_TEXT_BYTES} UTF-8 bytes"
            )));
        }
        if let Some(model) = &model {
            model
                .validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
        }
        let header = session.header();
        let profile = header.settings();
        let sandbox = sandbox.unwrap_or(profile.sandbox());
        let require_approval =
            profile.require_approval() || sandbox == rsi_sandbox::SandboxMode::DangerFullAccess;
        let body = SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text,
            model,
            sandbox,
            require_approval,
        };
        self.submit_body(session, turn_id, body).await
    }

    async fn submit_image(&self, request: SubmitImage) -> TurnResult<SubmittedTurn> {
        let SubmitImage {
            session,
            turn_id,
            model,
            request,
        } = request;
        model
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        request
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let body = SessionFactBody::ImageRequested {
            turn_id: turn_id.clone(),
            model,
            request,
        };
        self.submit_body(session, turn_id, body).await
    }

    async fn cancel(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        let body = SessionFactBody::CancelRequested {
            turn_id: turn_id.clone(),
            reason,
        };
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if !lock_state(&self.inner).sessions.contains_key(session_id) {
            if read_stored_outcome(&self.inner, session_id, turn_id)
                .await?
                .is_some()
            {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            }
            self.ensure_session_loaded(session_id).await?;
        }
        let live = lock_state(&self.inner)
            .sessions
            .get(session_id)
            .is_some_and(|session| session.turns.contains_key(turn_id));
        if !live {
            if lock_state(&self.inner)
                .sessions
                .get(session_id)
                .is_some_and(|session| session.durable_seq == 0)
            {
                return Err(turn_not_found(session_id, turn_id));
            }
            return match read_stored_outcome(&self.inner, session_id, turn_id).await? {
                Some(_) => Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                }),
                None => Err(TurnError::Invariant(
                    "nonterminal durable turn is absent from live control state".into(),
                )),
            };
        }
        let (cancel_seq, cancellation) = {
            let mut state = lock_state(&self.inner);
            let session = state
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?;
            let turn = session
                .turns
                .get(turn_id)
                .ok_or_else(|| TurnError::TurnNotFound {
                    session: session_id.to_string(),
                    turn: turn_id.to_string(),
                })?;
            if turn.terminal.is_some() {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            }
            if turn.cancel_requested {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: false,
                });
            }
            let cancellation = turn.cancellation.clone();
            let fact = next_fact(&self.inner, session, body).map_err(turn_kernel_error)?;
            let cancel_seq = fact.seq();
            push_pending(&self.inner, session, fact.clone()).map_err(turn_kernel_error)?;
            session
                .turns
                .get_mut(turn_id)
                .expect("validated turn exists")
                .cancel_requested = true;
            publish_live_watermarks(session);
            (cancel_seq, cancellation)
        };
        self.wait_for_durable(session_id, cancel_seq)
            .await
            .map_err(turn_kernel_error)?;
        cancellation.cancel();
        Ok(CancelResult {
            accepted: true,
            already_terminal: false,
        })
    }

    async fn observe(&self, session_id: &SessionId, after_seq: u64) -> TurnResult<TurnObservation> {
        let observer_lease = ObserverLease::acquire(&self.inner)?;
        let live_snapshot = {
            let state = lock_state(&self.inner);
            state.sessions.get(session_id).map(|session| {
                (
                    session.updates.subscribe(),
                    Some(session.flush_status.subscribe()),
                    session.durable_seq,
                    session.live_seq(),
                )
            })
        };
        let (receiver, flush_status, durable_target, live_seq, durable_facts) =
            if let Some((receiver, flush_status, durable_target, live_seq)) = live_snapshot {
                (
                    receiver,
                    flush_status,
                    durable_target,
                    live_seq.map_err(turn_kernel_error)?,
                    VecDeque::new(),
                )
            } else {
                let page =
                    read_facts_bounded(&self.inner, session_id, after_seq, MAXIMUM_FACTS_PER_READ)
                        .await
                        .map_err(|error| match error {
                            StoreError::Invalid(_) => TurnError::Invalid(
                                "observation cursor exceeds the durable tail".into(),
                            ),
                            other => turn_store_error(other),
                        })?;
                let page_durable_seq = page.durable_seq;
                let durable_facts = page.facts.into_iter().map(Arc::new).collect();
                let state = lock_state(&self.inner);
                if let Some(session) = state.sessions.get(session_id) {
                    (
                        session.updates.subscribe(),
                        Some(session.flush_status.subscribe()),
                        session.durable_seq.max(page_durable_seq),
                        session
                            .live_seq()
                            .map_err(turn_kernel_error)?
                            .max(page_durable_seq),
                        durable_facts,
                    )
                } else {
                    let (sender, receiver) = watch::channel(LiveWatermarks {
                        live_seq: page_durable_seq,
                        durable_seq: page_durable_seq,
                    });
                    drop(sender);
                    (
                        receiver,
                        None,
                        page_durable_seq,
                        page_durable_seq,
                        durable_facts,
                    )
                }
            };
        if after_seq > live_seq {
            return Err(TurnError::Invalid(
                "observation cursor exceeds the live tail".into(),
            ));
        }
        let state = ObservationState {
            session_id: session_id.clone(),
            cursor: after_seq,
            durable_target,
            live_target: live_seq,
            inner: Arc::downgrade(&self.inner),
            receiver,
            flush_status,
            durable_facts,
            ended: false,
            _observer_lease: observer_lease,
        };
        Ok(stream::unfold(state, observation_next).boxed())
    }

    async fn outcome(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> TurnResult<Option<TurnOutcome>> {
        {
            let state = lock_state(&self.inner);
            if let Some(session) = state.sessions.get(session_id) {
                if let Some(turn) = session.turns.get(turn_id) {
                    return Ok(turn
                        .terminal_seq
                        .filter(|terminal_seq| *terminal_seq <= session.durable_seq)
                        .and_then(|_| turn.terminal.clone()));
                }
                if session.durable_seq == 0 {
                    return Err(turn_not_found(session_id, turn_id));
                }
            }
        }
        read_stored_outcome(&self.inner, session_id, turn_id).await
    }

    async fn session_header(&self, session_id: &SessionId) -> TurnResult<SessionHeader> {
        if let Some(header) = lock_state(&self.inner)
            .sessions
            .get(session_id)
            .map(|session| session.header.as_ref().clone())
        {
            return Ok(header);
        }
        read_header_bounded(&self.inner, session_id)
            .await
            .map_err(turn_store_error)
    }
}

#[async_trait]
impl TurnFinalization for SessionKernel {
    fn register(
        &self,
        name: String,
        finalizer: Arc<dyn TurnFinalizer>,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizerLease> {
        validate_identifier("turn finalizer", &name)
            .map_err(|error| TurnFinalizationError::Invalid(error.to_string()))?;
        let registration = {
            let mut state = lock_state(&self.inner);
            if state.finalizer_names.contains(&name) {
                return Err(TurnFinalizationError::Invalid(format!(
                    "turn finalizer `{name}` is already registered"
                )));
            }
            if state.finalizers.len() >= 64 {
                return Err(TurnFinalizationError::Invalid(
                    "turn finalizer capacity is exhausted".into(),
                ));
            }
            state.next_finalizer_registration = state
                .next_finalizer_registration
                .checked_add(1)
                .ok_or_else(|| {
                    TurnFinalizationError::Invalid("turn finalizer identity is exhausted".into())
                })?;
            let registration = state.next_finalizer_registration;
            state.finalizer_names.insert(name.clone());
            state.finalizers.insert(
                registration,
                FinalizerEntry {
                    name: name.clone(),
                    finalizer,
                },
            );
            registration
        };
        let inner = Arc::downgrade(&self.inner);
        Ok(TurnFinalizerLease::new(move || {
            if let Some(inner) = inner.upgrade() {
                let mut state = inner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state
                    .finalizers
                    .get(&registration)
                    .is_some_and(|entry| entry.name == name)
                {
                    state.finalizers.remove(&registration);
                    state.finalizer_names.remove(&name);
                }
            }
        }))
    }

    async fn finalize(
        &self,
        context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        let finalizers = lock_state(&self.inner)
            .finalizers
            .values()
            .map(|entry| (entry.name.clone(), Arc::clone(&entry.finalizer)))
            .collect::<Vec<_>>();
        let results = futures_util::future::join_all(finalizers.into_iter().map(
            |(name, finalizer)| async move {
                let result = std::panic::AssertUnwindSafe(finalizer.finalize(context))
                    .catch_unwind()
                    .await;
                (name, result)
            },
        ))
        .await;

        for (name, result) in &results {
            match result {
                Ok(Err(error)) => return Err(error.clone()),
                Err(_) => {
                    return Err(TurnFinalizationError::Failed {
                        code: "turn.finalizer_panic".into(),
                        message: format!("turn finalizer `{name}` panicked"),
                    });
                }
                Ok(Ok(_)) => {}
            }
        }
        for (_, result) in results {
            if let Ok(Ok(report)) = result
                && let Some(blocker) = report.completion_blocker()
            {
                return Ok(TurnFinalizationReport::blocked(blocker.clone()));
            }
        }
        Ok(TurnFinalizationReport::complete())
    }
}

#[async_trait]
impl TurnExecution for SessionKernel {
    fn register(&self, executor_id: String) -> TurnResult<ExecutorLease> {
        validate_identifier("executor", &executor_id)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let registration_id = {
            let mut state = lock_state(&self.inner);
            if state.executors.contains_key(&executor_id) {
                return Err(TurnError::Invalid(
                    "executor identity is already registered".into(),
                ));
            }
            state.next_executor_registration = state
                .next_executor_registration
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("executor identity exhausted".into()))?;
            let registration_id = state.next_executor_registration;
            state.executors.insert(executor_id.clone(), registration_id);
            registration_id
        };
        self.inner.claim_changed.notify_waiters();
        let inner = Arc::downgrade(&self.inner);
        Ok(ExecutorLease::new(move || {
            deregister_executor(&inner, &executor_id, registration_id);
        }))
    }

    async fn claim(
        &self,
        executor_id: &str,
        cancellation: CancellationToken,
    ) -> TurnResult<Option<TurnClaim>> {
        loop {
            // Create the waiter before inspecting the queue so a notification
            // between the empty-queue check and `select!` is still observed.
            let claim_changed = self.inner.claim_changed.notified();
            {
                let mut state = lock_state(&self.inner);
                let registration_id = state
                    .executors
                    .get(executor_id)
                    .copied()
                    .ok_or(TurnError::StaleClaim)?;
                let candidates = state.claim_queue.len();
                for _ in 0..candidates {
                    let Some((session_id, turn_id)) = state.claim_queue.pop_front() else {
                        break;
                    };
                    state.queued.remove(&(session_id.clone(), turn_id.clone()));
                    let claimable = state.sessions.get(&session_id).is_some_and(|session| {
                        session.permanent_flush_error.is_none()
                            && session.oldest_claimable() == Some(&turn_id)
                            && session
                                .turns
                                .get(&turn_id)
                                .is_some_and(|turn| turn.claim.is_none())
                    });
                    if !claimable {
                        continue;
                    }
                    state.next_claim = state
                        .next_claim
                        .checked_add(1)
                        .ok_or_else(|| TurnError::Invariant("claim identity exhausted".into()))?;
                    let claim_id = state.next_claim;
                    let session = state
                        .sessions
                        .get_mut(&session_id)
                        .expect("claimable session was observed");
                    let live_seq = session.live_seq().map_err(turn_kernel_error)?;
                    let turn = session
                        .turns
                        .get_mut(&turn_id)
                        .expect("claimable turn was observed");
                    let accepted_at_ms = turn.accepted_at_ms;
                    let accepted_seq = turn.accepted_seq;
                    turn.claim = Some(ClaimOwner {
                        executor: executor_id.into(),
                        registration: registration_id,
                        claim: claim_id,
                        live_seq,
                    });
                    return Ok(Some(self.inner.claim_issuer.issue(
                        executor_id.into(),
                        claim_id,
                        session_id,
                        turn_id,
                        session.header.clone(),
                        accepted_at_ms,
                        accepted_seq,
                        live_seq,
                    )));
                }
                if !state.accepting {
                    return Ok(None);
                }
            }
            tokio::select! {
                () = claim_changed => {}
                () = cancellation.cancelled() => return Ok(None),
                () = self.inner.stop_worker.cancelled() => return Ok(None),
            }
        }
    }

    fn composition(&self, claim: &TurnClaim) -> TurnResult<AgentCompositionPin> {
        let state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        state
            .sessions
            .get(claim.session_id())
            .map(|session| session.composition.clone())
            .ok_or(TurnError::StaleClaim)
    }

    async fn read_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> TurnResult<ClaimFactPage> {
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "Fact read limit is out of bounds".into(),
            ));
        }
        let (durable_seq, live_seq, hidden_turns) = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            let claimed_index = session
                .turn_order
                .iter()
                .position(|turn_id| turn_id == claim.turn_id())
                .ok_or_else(|| TurnError::Invariant("claimed turn is missing from order".into()))?;
            (
                session.durable_seq,
                session.live_seq().map_err(turn_kernel_error)?,
                session.turn_order[claimed_index + 1..]
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
        };
        if after_seq > live_seq {
            return Err(TurnError::Invalid("Fact cursor exceeds live tail".into()));
        }
        let mut facts = Vec::new();
        let mut through_seq = after_seq;
        let mut scanned = 0_usize;
        if after_seq < durable_seq {
            let page = read_facts_bounded(&self.inner, claim.session_id(), after_seq, limit)
                .await
                .map_err(turn_store_error)?;
            scanned = page.facts.len();
            through_seq = page
                .facts
                .last()
                .map_or(after_seq, SessionFact::seq)
                .min(live_seq);
            facts.extend(page.facts.into_iter().filter_map(|fact| {
                (fact.seq() <= live_seq && !hidden_turns.contains(fact.body().turn_id()))
                    .then(|| Arc::new(fact))
            }));
        }
        let state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        let session = state
            .sessions
            .get(claim.session_id())
            .expect("validated claim session exists");
        if through_seq < session.durable_seq || through_seq == live_seq || scanned == limit {
            return Ok(ClaimFactPage { facts, through_seq });
        }
        if scanned < limit && through_seq < live_seq {
            let pending_after = through_seq;
            for fact in session
                .pending
                .iter()
                .filter(|fact| fact.seq() > pending_after && fact.seq() <= live_seq)
                .take(limit - scanned)
            {
                through_seq = fact.seq();
                if !hidden_turns.contains(fact.body().turn_id()) {
                    facts.push(fact.clone());
                }
            }
        }
        Ok(ClaimFactPage { facts, through_seq })
    }

    async fn read_checkpoint_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> TurnResult<Option<ClaimFactPage>> {
        self.validate_issued_claim(claim)?;
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "checkpoint Fact read limit is out of bounds".into(),
            ));
        }
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(None);
        }
        let (session_present, expected_durable) = {
            let state = lock_state(&self.inner);
            match state.sessions.get(claim.session_id()) {
                Some(session) => {
                    let live_seq = session.live_seq().map_err(turn_kernel_error)?;
                    (
                        true,
                        (live_seq == session.durable_seq).then_some(session.durable_seq),
                    )
                }
                None => (false, None),
            }
        };
        if session_present && expected_durable.is_none() {
            return Ok(None);
        }
        if read_stored_outcome(&self.inner, claim.session_id(), claim.turn_id())
            .await?
            .is_none()
        {
            return Ok(None);
        }
        let page = read_facts_bounded(&self.inner, claim.session_id(), after_seq, limit)
            .await
            .map_err(turn_store_error)?;
        if expected_durable.is_some_and(|expected| page.durable_seq != expected) {
            return Ok(None);
        }
        let through_seq = page.facts.last().map_or(after_seq, SessionFact::seq);
        Ok(Some(ClaimFactPage {
            facts: page.facts.into_iter().map(Arc::new).collect(),
            through_seq,
        }))
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<Option<ContextCheckpoint>> {
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(None);
        }
        let permits = u32::try_from(MAXIMUM_CONTEXT_CHECKPOINT_BYTES)
            .map_err(|_| TurnError::Invariant("checkpoint bound exceeds semaphore range".into()))?;
        let permit = Arc::clone(&self.inner.store_read_admission)
            .acquire_many_owned(permits)
            .await
            .map_err(|_| TurnError::Invariant("Store-read admission closed".into()))?;
        let checkpoint = match self.inner.store.read_context_checkpoint(session_id).await {
            Ok(checkpoint) => checkpoint,
            Err(StoreError::NotFound(_)) => None,
            Err(error) => return Err(turn_store_error(error)),
        };
        drop(permit);
        Ok(checkpoint.map(|checkpoint| ContextCheckpoint {
            header_fingerprint: checkpoint.header_fingerprint,
            through_seq: checkpoint.through_seq,
            fact_prefix_sha256: checkpoint.fact_prefix_sha256,
            bytes: checkpoint.bytes,
        }))
    }

    async fn write_context_checkpoint(
        &self,
        claim: &TurnClaim,
        checkpoint: ContextCheckpoint,
    ) -> TurnResult<bool> {
        self.validate_issued_claim(claim)?;
        let expected_fingerprint = claim
            .header()
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if checkpoint.header_fingerprint != expected_fingerprint {
            return Err(TurnError::Invalid(
                "checkpoint header fingerprint changed session identity".into(),
            ));
        }
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(false);
        }
        {
            let state = lock_state(&self.inner);
            if let Some(session) = state.sessions.get(claim.session_id())
                && (!self
                    .inner
                    .claim_issuer
                    .validates_header(claim, &session.header)
                    || session.durable_seq != checkpoint.through_seq
                    || session.live_seq().map_err(turn_kernel_error)? != checkpoint.through_seq)
            {
                return Ok(false);
            }
        }
        if read_stored_outcome(&self.inner, claim.session_id(), claim.turn_id())
            .await?
            .is_none()
        {
            return Ok(false);
        }
        let write = WriteContextCheckpoint {
            session_id: claim.session_id().clone(),
            expected_durable_seq: checkpoint.through_seq,
            checkpoint: StoredContextCheckpoint {
                header_fingerprint: checkpoint.header_fingerprint,
                through_seq: checkpoint.through_seq,
                fact_prefix_sha256: checkpoint.fact_prefix_sha256,
                bytes: checkpoint.bytes,
            },
        };
        match self.inner.store.write_context_checkpoint(write).await {
            Ok(()) => Ok(true),
            Err(StoreError::Conflict { .. }) => Ok(false),
            Err(error) => Err(turn_store_error(error)),
        }
    }

    async fn publish(
        &self,
        claim: &TurnClaim,
        mut bodies: Vec<SessionFactBody>,
    ) -> TurnResult<PublishAttempt> {
        if bodies.is_empty() || bodies.len() > MAXIMUM_STORE_BATCH_FACTS {
            return Err(TurnError::Invalid(
                "Fact publication batch is empty or too large".into(),
            ));
        }
        let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
        loop {
            let process_capacity_changed = self.inner.process_pending_changed.notified();
            tokio::pin!(process_capacity_changed);
            let _enabled = process_capacity_changed.as_mut().enable();
            match try_publish_once(self, claim, bodies)? {
                PublishAdmission::Complete(result) => return Ok(result),
                PublishAdmission::ProcessPressure(unpublished) => {
                    bodies = unpublished;
                    self.inner.flush_requested.notify_one();
                    tokio::select! {
                        () = &mut process_capacity_changed => {}
                        () = self.inner.stop_worker.cancelled() => {
                            return Err(TurnError::ShuttingDown);
                        }
                        () = tokio::time::sleep_until(deadline) => {
                            return Err(TurnError::Capacity);
                        }
                    }
                }
            }
        }
    }

    async fn flush(&self, claim: &TurnClaim, through_seq: u64) -> TurnResult<u64> {
        let status = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            let live_seq = session.live_seq().map_err(turn_kernel_error)?;
            if through_seq == 0 || through_seq > live_seq {
                return Err(TurnError::Invalid(
                    "flush target is zero or exceeds the live tail".into(),
                ));
            }
            session.flush_status.subscribe()
        };
        self.inner.flush_requested.notify_one();
        self.wait_on_flush_status(status, through_seq)
            .await
            .map_err(turn_kernel_error)
    }

    fn cancellation(&self, claim: &TurnClaim) -> TurnResult<CancellationToken> {
        let state = lock_state(&self.inner);
        let turn = self.validate_claim(&state, claim)?;
        Ok(turn.cancellation.clone())
    }

    fn release(&self, claim: &TurnClaim) -> TurnResult<()> {
        let mut state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        let turn = state
            .sessions
            .get_mut(claim.session_id())
            .expect("validated claim session exists")
            .turns
            .get_mut(claim.turn_id())
            .expect("validated claim turn exists");
        if turn.terminal.is_none() {
            turn.claim = None;
            enqueue(
                &mut state,
                claim.session_id().clone(),
                claim.turn_id().clone(),
            );
        }
        drop(state);
        self.inner.claim_changed.notify_waiters();
        Ok(())
    }
}

enum PublishAdmission {
    Complete(PublishAttempt),
    ProcessPressure(Vec<SessionFactBody>),
}

fn try_publish_once(
    kernel: &SessionKernel,
    claim: &TurnClaim,
    bodies: Vec<SessionFactBody>,
) -> TurnResult<PublishAdmission> {
    let mut state = lock_state(&kernel.inner);
    kernel.validate_claim(&state, claim)?;
    if !state.accepting {
        return Err(TurnError::ShuttingDown);
    }
    let session = state
        .sessions
        .get_mut(claim.session_id())
        .expect("validated claim session exists");
    if let Some(error) = &session.permanent_flush_error {
        return Err(TurnError::Flush(error.clone()));
    }
    let original = session
        .turns
        .get(claim.turn_id())
        .expect("validated claim turn exists");
    let mut staged = clone_turn_control(original);
    let mut normalized = Vec::with_capacity(bodies.len());
    for body in bodies {
        if body.turn_id() != claim.turn_id() {
            return Err(TurnError::Invalid(
                "executor Fact changed the claimed turn identity".into(),
            ));
        }
        validate_durable_intent_fence(session, &body)?;
        let body = canonicalize_terminal(body, staged.cancel_requested);
        apply_executor_body(&mut staged, &body)?;
        normalized.push(body);
    }
    let mut next_seq = session.live_seq().map_err(turn_kernel_error)?;
    let mut facts = Vec::with_capacity(normalized.len());
    let mut added_bytes = 0_usize;
    for body in normalized {
        next_seq = next_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("Fact sequence exhausted".into()))?;
        let fact = SessionFact::new(next_seq, kernel.inner.clock.now_ms().max(1), body)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        added_bytes = added_bytes
            .checked_add(fact.encoded_len())
            .ok_or_else(|| TurnError::Invalid("Fact bytes overflowed".into()))?;
        facts.push(fact);
    }
    staged.budget_usage = enforce_turn_budget(
        session.header.settings().turn_budget(),
        original,
        &facts,
        kernel.inner.clock.now_ms().max(1),
    )?;
    let projected_pending_bytes = session
        .pending_bytes
        .checked_add(added_bytes)
        .ok_or_else(|| TurnError::Invariant("pending Fact bytes overflowed".into()))?;
    if added_bytes > MAXIMUM_PENDING_FACT_BYTES
        || added_bytes > kernel.inner.limits.maximum_process_pending_fact_bytes
    {
        return Err(TurnError::Invalid(
            "Fact publication batch exceeds an empty pending-byte budget".into(),
        ));
    }
    if projected_pending_bytes > MAXIMUM_PENDING_FACT_BYTES
        || projected_pending_bytes > kernel.inner.limits.maximum_process_pending_fact_bytes
    {
        return Ok(PublishAdmission::Complete(PublishAttempt::FlushRequired {
            unpublished: facts.into_iter().map(SessionFact::into_body).collect(),
        }));
    }
    match reserve_atomic_capacity(
        &kernel.inner.process_pending_bytes,
        added_bytes,
        kernel.inner.limits.maximum_process_pending_fact_bytes,
    ) {
        Ok(()) => {}
        Err(KernelError::Capacity(_)) => {
            return Ok(PublishAdmission::ProcessPressure(
                facts.into_iter().map(SessionFact::into_body).collect(),
            ));
        }
        Err(error) => return Err(turn_kernel_error(error)),
    }
    let facts = facts.into_iter().map(Arc::new).collect::<Vec<_>>();
    if staged.terminal.is_some() {
        staged.terminal_seq = facts.last().map(|fact| fact.seq());
    }
    *session
        .turns
        .get_mut(claim.turn_id())
        .expect("validated claim turn exists") = staged;
    for fact in &facts {
        session.pending_bytes = session
            .pending_bytes
            .checked_add(fact.encoded_len())
            .expect("the complete batch pending-byte projection was validated");
        session.pending.push_back(fact.clone());
        if !is_terminal_fact(fact) {
            publish_live_watermarks(session);
        }
    }
    Ok(PublishAdmission::Complete(PublishAttempt::Published(facts)))
}

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

async fn read_facts_bounded(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
    after_seq: u64,
    requested_limit: usize,
) -> std::result::Result<rsi_agent_store_protocol::StoreFactPage, StoreError> {
    let (effective_limit, permit) = acquire_store_read(inner, requested_limit).await?;
    let result = inner
        .store
        .read_facts(session_id, after_seq, effective_limit)
        .await;
    drop(permit);
    result
}

async fn read_turn_facts_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
    turn_id: &TurnId,
    after_seq: u64,
    requested_limit: usize,
) -> std::result::Result<rsi_agent_store_protocol::StoreTurnFactPage, StoreError> {
    let (effective_limit, permit) = acquire_store_read(inner, requested_limit).await?;
    let result = inner
        .store
        .read_turn_facts(session_id, turn_id, after_seq, effective_limit)
        .await;
    drop(permit);
    result
}

async fn read_turn_boundary_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> std::result::Result<rsi_agent_store_protocol::StoreTurnBoundary, StoreError> {
    let permit = acquire_store_read_bytes(inner, MAXIMUM_SESSION_FACT_BYTES).await?;
    let result = inner.store.read_turn_boundary(session_id, turn_id).await;
    drop(permit);
    result
}

async fn read_header_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
) -> std::result::Result<SessionHeader, StoreError> {
    let permit = acquire_store_read_bytes(inner, MAXIMUM_SESSION_HEADER_BYTES).await?;
    let result = inner.store.header(session_id).await;
    drop(permit);
    result
}

async fn acquire_store_read(
    inner: &KernelInner,
    requested_limit: usize,
) -> std::result::Result<(usize, tokio::sync::OwnedSemaphorePermit), StoreError> {
    let (effective_limit, reservation) = if requested_limit == 1 {
        (1, MAXIMUM_SESSION_FACT_BYTES)
    } else if inner.limits.maximum_store_read_bytes >= MAXIMUM_STORE_BATCH_BYTES {
        (requested_limit, MAXIMUM_STORE_BATCH_BYTES)
    } else {
        (1, MAXIMUM_SESSION_FACT_BYTES)
    };
    let permit = acquire_store_read_bytes(inner, reservation).await?;
    Ok((effective_limit, permit))
}

async fn acquire_store_read_bytes(
    inner: &KernelInner,
    reservation: usize,
) -> std::result::Result<tokio::sync::OwnedSemaphorePermit, StoreError> {
    let permits = u32::try_from(reservation).map_err(|_| {
        StoreError::Invalid("Store-read reservation exceeds semaphore representation".into())
    })?;
    Arc::clone(&inner.store_read_admission)
        .acquire_many_owned(permits)
        .await
        .map_err(|_| StoreError::Io("Kernel Store-read admission closed".into()))
}

const fn context_checkpoints_enabled(inner: &KernelInner) -> bool {
    inner.limits.maximum_store_read_bytes >= MAXIMUM_CONTEXT_CHECKPOINT_BYTES
}

async fn observation_next(
    mut state: ObservationState,
) -> Option<(TurnResult<TurnUpdate>, ObservationState)> {
    if state.ended {
        return None;
    }
    loop {
        let previous_durable = state.durable_target;
        let current = *state.receiver.borrow_and_update();
        state.live_target = state.live_target.max(current.live_seq);
        state.durable_target = state.durable_target.max(current.durable_seq);
        if state.cursor < state.durable_target {
            if let Some(update) = take_buffered_durable_fact(&mut state) {
                return Some((update, state));
            }
            let inner = state.inner.upgrade()?;
            match read_facts_bounded(
                &inner,
                &state.session_id,
                state.cursor,
                MAXIMUM_FACTS_PER_READ,
            )
            .await
            {
                Ok(page) => {
                    state.durable_target = state.durable_target.max(page.durable_seq);
                    state.live_target = state.live_target.max(page.durable_seq);
                    state.durable_facts = page.facts.into_iter().map(Arc::new).collect();
                    if state.durable_facts.is_empty() {
                        state.ended = true;
                        return Some((
                            Err(TurnError::Invariant(
                                "durable observation cursor made no progress".into(),
                            )),
                            state,
                        ));
                    }
                    continue;
                }
                Err(error) => {
                    state.ended = true;
                    return Some((Err(turn_store_error(error)), state));
                }
            }
        }
        if state.durable_target > previous_durable && state.cursor >= state.durable_target {
            return Some((
                Ok(TurnUpdate::Durable {
                    durable_seq: state.durable_target,
                }),
                state,
            ));
        }
        if state.cursor < state.live_target
            && let Some(fact) = next_speculative_observation_fact(&mut state)
        {
            state.cursor = fact.seq();
            return Some((
                Ok(TurnUpdate::Fact {
                    fact,
                    durable_seq: state.durable_target,
                }),
                state,
            ));
        }
        let permanent_error = state
            .flush_status
            .as_ref()
            .and_then(|status| status.borrow().permanent_error.clone());
        if let Some(error) = permanent_error {
            let update = observation_flush_result(&mut state, error);
            return Some((update, state));
        }
        let signal = if let Some(status) = state.flush_status.as_mut() {
            tokio::select! {
                update = state.receiver.changed() => ObservationSignal::Update(update),
                changed = status.changed() => ObservationSignal::Flush(changed),
            }
        } else {
            ObservationSignal::Update(state.receiver.changed().await)
        };
        match signal {
            ObservationSignal::Flush(Ok(())) | ObservationSignal::Update(Ok(())) => {
                // The next loop consumes and compares any exact unseen value.
            }
            ObservationSignal::Flush(Err(_)) => {
                state.flush_status = None;
            }
            ObservationSignal::Update(Err(_)) => return None,
        }
    }
}

fn take_buffered_durable_fact(state: &mut ObservationState) -> Option<TurnResult<TurnUpdate>> {
    let fact = state.durable_facts.pop_front()?;
    if fact.seq() != state.cursor.saturating_add(1) || fact.seq() > state.durable_target {
        state.ended = true;
        return Some(Err(TurnError::Invariant(
            "buffered durable observation Facts are not contiguous".into(),
        )));
    }
    state.cursor = fact.seq();
    Some(Ok(TurnUpdate::Fact {
        fact,
        durable_seq: state.durable_target,
    }))
}

fn next_speculative_observation_fact(state: &mut ObservationState) -> Option<Arc<SessionFact>> {
    let inner = state.inner.upgrade()?;
    let kernel = lock_state(&inner);
    let session = kernel.sessions.get(&state.session_id)?;
    let live_seq = session.live_seq().ok()?;
    state.live_target = state.live_target.max(live_seq);
    state.durable_target = state.durable_target.max(session.durable_seq);
    let next_seq = state.cursor.checked_add(1)?;
    let pending_start = session.durable_seq.checked_add(1)?;
    let offset = next_seq.checked_sub(pending_start)?;
    let fact = session.pending.get(usize::try_from(offset).ok()?)?;
    (fact.seq() == next_seq && (!is_terminal_fact(fact) || fact.seq() <= session.durable_seq))
        .then(|| Arc::clone(fact))
}

fn observation_flush_result(state: &mut ObservationState, error: String) -> TurnResult<TurnUpdate> {
    state.ended = true;
    Err(TurnError::Flush(error))
}

async fn repair_unfinished_session(
    store: &Arc<dyn SessionStore>,
    clock: &dyn Clock,
    session_id: &SessionId,
) -> Result<()> {
    let open = store.list_open_turns(session_id, 0, 1).await?;
    if open.turns.is_empty() {
        return Ok(());
    }
    let header = store.header(session_id).await?;
    let (durable_seq, turns, turn_order) =
        load_control_state(store, None, session_id, header.settings().turn_budget()).await?;
    if turns.len() != turn_order.len()
        || turn_order
            .iter()
            .any(|turn_id| !turns.contains_key(turn_id))
    {
        return Err(KernelError::Invariant(
            "recovery control state and turn order disagree".into(),
        ));
    }
    if turns.is_empty() {
        return Ok(());
    }
    let timestamp = clock.now_ms().max(1);
    let mut final_seq = durable_seq;
    let mut repair = Vec::with_capacity(turn_order.len());
    for turn_id in turn_order {
        let turn = turns
            .get(&turn_id)
            .expect("validated recovery turn order references exact state");
        let outcome = if turn.cancel_requested {
            TurnOutcome::Cancelled
        } else if let Some((dimension, consumed, limit)) = turn.budget_exhausted {
            TurnOutcome::BudgetExceeded {
                dimension,
                consumed,
                limit,
            }
        } else {
            let effect = match &turn.effect {
                Some(ActiveEffect::Model { started: true, .. }) => Some(EffectKind::Model),
                Some(ActiveEffect::Image { started: true, .. }) => Some(EffectKind::Image),
                Some(ActiveEffect::Tool { started: true, .. }) => Some(EffectKind::Tool),
                Some(
                    ActiveEffect::Model { started: false, .. }
                    | ActiveEffect::Image { started: false, .. }
                    | ActiveEffect::Tool { started: false, .. },
                )
                | None => None,
            };
            TurnOutcome::Interrupted {
                effect,
                reason: "Kernel recovery found a turn without a durable terminal Fact".into(),
            }
        };
        final_seq = final_seq
            .checked_add(1)
            .ok_or_else(|| KernelError::Invariant("recovery Fact sequence exhausted".into()))?;
        repair.push(SessionFact::new(
            final_seq,
            timestamp,
            SessionFactBody::TurnTerminal { turn_id, outcome },
        )?);
    }
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: durable_seq,
            header: None,
            facts: repair,
        })
        .await?;
    Ok(())
}

fn is_terminal_fact(fact: &SessionFact) -> bool {
    matches!(fact.body(), SessionFactBody::TurnTerminal { .. })
}

fn validate_durable_intent_fence(
    session: &SessionRuntime,
    body: &SessionFactBody,
) -> TurnResult<()> {
    let is_start = matches!(
        body,
        SessionFactBody::ModelStarted { .. }
            | SessionFactBody::ImageStarted { .. }
            | SessionFactBody::ToolStarted { .. }
    );
    if !is_start {
        return Ok(());
    }
    let turn = session
        .turns
        .get(body.turn_id())
        .ok_or_else(|| TurnError::Invalid("effect start references an unknown turn".into()))?;
    let matches_active_intent = match (&turn.effect, body) {
        (
            Some(ActiveEffect::Model {
                effect_id,
                started: false,
            }),
            SessionFactBody::ModelStarted {
                effect_id: started, ..
            },
        )
        | (
            Some(ActiveEffect::Image {
                effect_id,
                started: false,
                ..
            }),
            SessionFactBody::ImageStarted {
                effect_id: started, ..
            },
        )
        | (
            Some(ActiveEffect::Tool {
                effect_id,
                started: false,
                ..
            }),
            SessionFactBody::ToolStarted {
                effect_id: started, ..
            },
        ) => effect_id == started,
        _ => false,
    };
    let intent_is_pending = session.pending.iter().any(|fact| {
        matches!(
            (fact.body(), body),
            (
                SessionFactBody::ModelIntent { effect_id: intent, .. },
                SessionFactBody::ModelStarted { effect_id: started, .. }
            ) if intent == started
        ) || matches!(
            (fact.body(), body),
            (
                SessionFactBody::ImageIntent { effect_id: intent, .. },
                SessionFactBody::ImageStarted { effect_id: started, .. }
            ) if intent == started
        ) || matches!(
            (fact.body(), body),
            (
                SessionFactBody::ToolIntent { effect_id: intent, .. },
                SessionFactBody::ToolStarted { effect_id: started, .. }
            ) if intent == started
        )
    });
    if !matches_active_intent || intent_is_pending {
        return Err(TurnError::Invalid(
            "effect start requires its matching durable intent".into(),
        ));
    }
    Ok(())
}

async fn load_control_state(
    store: &Arc<dyn SessionStore>,
    admission: Option<&KernelInner>,
    session_id: &SessionId,
    budget: &TurnBudget,
) -> Result<(u64, BTreeMap<TurnId, TurnControl>, Vec<TurnId>)> {
    let mut open_cursor = 0_u64;
    let mut durable_seq = None;
    let mut turns = BTreeMap::new();
    let mut order = Vec::new();
    loop {
        let page = store
            .list_open_turns(session_id, open_cursor, MAXIMUM_FACTS_PER_READ)
            .await?;
        if durable_seq
            .replace(page.durable_seq)
            .is_some_and(|previous| previous != page.durable_seq)
        {
            return Err(KernelError::Invariant(
                "Store durable watermark changed during control-state load".into(),
            ));
        }
        for open_turn in &page.turns {
            let mut turn_cursor = 0_u64;
            loop {
                let turn_page = match admission {
                    Some(inner) => {
                        read_turn_facts_bounded(
                            inner,
                            session_id,
                            &open_turn.turn_id,
                            turn_cursor,
                            MAXIMUM_FACTS_PER_READ,
                        )
                        .await?
                    }
                    None => {
                        store
                            .read_turn_facts(
                                session_id,
                                &open_turn.turn_id,
                                turn_cursor,
                                MAXIMUM_FACTS_PER_READ,
                            )
                            .await?
                    }
                };
                if turn_page.durable_seq != page.durable_seq {
                    return Err(KernelError::Invariant(
                        "Store durable watermark changed during per-turn load".into(),
                    ));
                }
                for fact in &turn_page.facts {
                    apply_recovered_fact(&mut turns, &mut order, budget, fact)?;
                    turn_cursor = fact.seq();
                }
                if !turn_page.has_more {
                    break;
                }
                if turn_page.facts.is_empty() {
                    return Err(KernelError::Invariant(
                        "Store turn Fact page made no progress".into(),
                    ));
                }
            }
            if !turns.contains_key(&open_turn.turn_id) {
                return Err(KernelError::Invariant(
                    "Store open-turn index selected a terminal Fact stream".into(),
                ));
            }
            open_cursor = open_turn.accepted_seq;
        }
        if !page.has_more {
            return Ok((durable_seq.unwrap_or(page.durable_seq), turns, order));
        }
        if page.turns.is_empty() {
            return Err(KernelError::Invariant(
                "Store open-turn page made no progress".into(),
            ));
        }
    }
}

async fn read_stored_outcome(
    inner: &KernelInner,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> TurnResult<Option<TurnOutcome>> {
    let boundary = read_turn_boundary_bounded(inner, session_id, turn_id)
        .await
        .map_err(turn_store_error)?;
    let Some(terminal) = boundary.terminal() else {
        return Ok(None);
    };
    match terminal.body() {
        SessionFactBody::TurnTerminal { outcome, .. } => Ok(Some(outcome.clone())),
        _ => Err(TurnError::Invariant(
            "Store turn boundary returned a nonterminal terminal Fact".into(),
        )),
    }
}

fn apply_recovered_fact(
    turns: &mut BTreeMap<TurnId, TurnControl>,
    order: &mut Vec<TurnId>,
    budget: &TurnBudget,
    fact: &SessionFact,
) -> Result<()> {
    match fact.body() {
        SessionFactBody::TurnAccepted { turn_id, .. }
        | SessionFactBody::ImageRequested { turn_id, .. } => {
            if turns.len() >= MAXIMUM_LIVE_TURNS {
                return Err(KernelError::Invariant(
                    "durable session exceeds the live turn bound".into(),
                ));
            }
            if turns
                .insert(
                    turn_id.clone(),
                    TurnControl::new(fact.timestamp_ms(), fact.seq()),
                )
                .is_some()
            {
                return Err(KernelError::Invariant(
                    "durable turn was accepted more than once".into(),
                ));
            }
            order.push(turn_id.clone());
        }
        SessionFactBody::CancelRequested { turn_id, .. } => {
            let turn = turns
                .get_mut(turn_id)
                .ok_or_else(|| KernelError::Invariant("cancel references unknown turn".into()))?;
            if turn.terminal.is_some() || turn.cancel_requested {
                return Err(KernelError::Invariant(
                    "durable cancellation is duplicate or follows terminal".into(),
                ));
            }
            turn.cancel_requested = true;
            turn.cancellation.cancel();
        }
        SessionFactBody::TurnTerminal { turn_id, .. } => {
            let turn = turns
                .get_mut(turn_id)
                .ok_or_else(|| KernelError::Invariant("terminal references unknown turn".into()))?;
            apply_executor_body(turn, fact.body())
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            turns.remove(turn_id);
            order.retain(|candidate| candidate != turn_id);
        }
        body => {
            let turn = turns.get_mut(body.turn_id()).ok_or_else(|| {
                KernelError::Invariant("durable Fact references unknown turn".into())
            })?;
            validate_budget_marker(budget, fact)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            let mut usage = turn.budget_usage;
            record_budget_usage(&mut usage, fact).map_err(KernelError::Invariant)?;
            check_budget_usage(budget, usage)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            apply_executor_body(turn, body)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
            turn.budget_usage = usage;
        }
    }
    Ok(())
}

fn apply_executor_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    if turn.terminal.is_some() {
        return Err(TurnError::Invalid("Fact follows a terminal turn".into()));
    }
    if turn.budget_exhausted.is_some() && !matches!(body, SessionFactBody::TurnTerminal { .. }) {
        return Err(TurnError::Invalid(
            "only the terminal Fact may follow budget exhaustion".into(),
        ));
    }
    match body {
        SessionFactBody::ModelIntent { .. }
        | SessionFactBody::ModelStarted { .. }
        | SessionFactBody::ModelEvent { .. } => apply_model_body(turn, body)?,
        SessionFactBody::ImageIntent { .. }
        | SessionFactBody::ImageStarted { .. }
        | SessionFactBody::ImageOutput { .. } => apply_image_body(turn, body)?,
        SessionFactBody::ToolIntent { .. }
        | SessionFactBody::ToolStarted { .. }
        | SessionFactBody::ToolResult { .. } => apply_tool_body(turn, body)?,
        SessionFactBody::BudgetExhausted {
            dimension,
            consumed,
            limit,
            ..
        } => {
            turn.budget_exhausted = Some((*dimension, *consumed, *limit));
        }
        SessionFactBody::TurnTerminal { outcome, .. } => {
            outcome
                .validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
            match (turn.budget_exhausted, outcome) {
                (
                    Some((dimension, consumed, limit)),
                    TurnOutcome::BudgetExceeded {
                        dimension: outcome_dimension,
                        consumed: outcome_consumed,
                        limit: outcome_limit,
                    },
                ) if dimension == *outcome_dimension
                    && consumed == *outcome_consumed
                    && limit == *outcome_limit => {}
                (Some(_), TurnOutcome::Cancelled) if turn.cancel_requested => {}
                (Some(_), _) => {
                    return Err(TurnError::Invalid(
                        "budget exhaustion and terminal outcome disagree".into(),
                    ));
                }
                (None, TurnOutcome::BudgetExceeded { .. }) => {
                    return Err(TurnError::Invalid(
                        "budget terminal lacks its preceding exhaustion Fact".into(),
                    ));
                }
                (None, _) => {}
            }
            turn.terminal = Some(outcome.clone());
            turn.effect = None;
        }
        SessionFactBody::TurnAccepted { .. }
        | SessionFactBody::ImageRequested { .. }
        | SessionFactBody::CancelRequested { .. } => {
            return Err(TurnError::Invalid(
                "executor cannot publish acceptance or cancellation Facts".into(),
            ));
        }
    }
    Ok(())
}

fn apply_model_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::ModelIntent { effect_id, .. } => {
            ensure_no_active_effect(turn)?;
            turn.effect = Some(ActiveEffect::Model {
                effect_id: effect_id.clone(),
                started: false,
            });
        }
        SessionFactBody::ModelStarted { effect_id, .. } => match &mut turn.effect {
            Some(ActiveEffect::Model {
                effect_id: current,
                started,
            }) if current == effect_id && !*started => *started = true,
            _ => return Err(TurnError::Invalid("model start has no exact intent".into())),
        },
        SessionFactBody::ModelEvent {
            effect_id, event, ..
        } => {
            match &turn.effect {
                Some(ActiveEffect::Model {
                    effect_id: current,
                    started: true,
                }) if current == effect_id => {}
                _ => return Err(TurnError::Invalid("model event has no exact start".into())),
            }
            if matches!(
                event,
                rsi_ai_protocol::LanguageEvent::Finished { .. }
                    | rsi_ai_protocol::LanguageEvent::Failed { .. }
            ) {
                turn.effect = None;
            }
        }
        _ => unreachable!("caller selected a Model Fact"),
    }
    Ok(())
}

fn apply_image_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::ImageIntent { effect_id, .. } => {
            ensure_no_active_effect(turn)?;
            turn.effect = Some(ActiveEffect::Image {
                effect_id: effect_id.clone(),
                started: false,
                next_index: 0,
            });
        }
        SessionFactBody::ImageStarted { effect_id, .. } => match &mut turn.effect {
            Some(ActiveEffect::Image {
                effect_id: current,
                started,
                ..
            }) if current == effect_id && !*started => *started = true,
            _ => return Err(TurnError::Invalid("Image start has no exact intent".into())),
        },
        SessionFactBody::ImageOutput {
            effect_id, index, ..
        } => match &mut turn.effect {
            Some(ActiveEffect::Image {
                effect_id: current,
                started: true,
                next_index,
            }) if current == effect_id && *index == *next_index => {
                *next_index = next_index
                    .checked_add(1)
                    .ok_or_else(|| TurnError::Invalid("Image output index exhausted".into()))?;
            }
            _ => {
                return Err(TurnError::Invalid(
                    "Image output has no exact start or contiguous index".into(),
                ));
            }
        },
        _ => unreachable!("caller selected an Image Fact"),
    }
    Ok(())
}

fn apply_tool_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    match body {
        SessionFactBody::ToolIntent {
            effect_id,
            identity,
            ..
        } => {
            ensure_no_active_effect(turn)?;
            turn.effect = Some(ActiveEffect::Tool {
                effect_id: effect_id.clone(),
                identity: identity.clone(),
                started: false,
            });
        }
        SessionFactBody::ToolStarted {
            effect_id,
            identity,
            ..
        } => match &mut turn.effect {
            Some(ActiveEffect::Tool {
                effect_id: current,
                identity: current_identity,
                started,
            }) if current == effect_id && current_identity == identity && !*started => {
                *started = true;
            }
            _ => return Err(TurnError::Invalid("Tool start has no exact intent".into())),
        },
        SessionFactBody::ToolResult {
            effect_id,
            identity,
            ..
        } => match &turn.effect {
            Some(ActiveEffect::Tool {
                effect_id: current,
                identity: current_identity,
                started: true,
            }) if current == effect_id && current_identity == identity => turn.effect = None,
            _ => return Err(TurnError::Invalid("Tool result has no exact start".into())),
        },
        _ => unreachable!("caller selected a Tool Fact"),
    }
    Ok(())
}

fn ensure_no_active_effect(turn: &TurnControl) -> TurnResult<()> {
    if turn.effect.is_some() {
        return Err(TurnError::Invalid("external effect already active".into()));
    }
    Ok(())
}

fn enforce_turn_budget(
    budget: &TurnBudget,
    turn: &TurnControl,
    facts: &[SessionFact],
    now_ms: u64,
) -> TurnResult<BudgetUsage> {
    let admits_work = facts.iter().any(|fact| {
        !matches!(
            fact.body(),
            SessionFactBody::BudgetExhausted { .. } | SessionFactBody::TurnTerminal { .. }
        )
    });
    if admits_work {
        let elapsed = now_ms.saturating_sub(turn.accepted_at_ms);
        if elapsed >= budget.maximum_elapsed_ms() {
            return Err(TurnError::BudgetExceeded {
                dimension: BudgetDimension::Elapsed,
                consumed: elapsed,
                limit: budget.maximum_elapsed_ms(),
            });
        }
    }
    for fact in facts {
        validate_budget_marker(budget, fact)?;
    }

    let mut usage = turn.budget_usage;
    for fact in facts {
        record_budget_usage(&mut usage, fact).map_err(TurnError::Invariant)?;
    }
    check_budget_usage(budget, usage)?;
    Ok(usage)
}

fn validate_budget_marker(budget: &TurnBudget, fact: &SessionFact) -> TurnResult<()> {
    if let SessionFactBody::BudgetExhausted {
        dimension,
        consumed,
        limit,
        ..
    } = fact.body()
        && (*limit != budget_limit(budget, *dimension) || *consumed < *limit)
    {
        return Err(TurnError::Invalid(
            "budget exhaustion does not match the frozen turn budget".into(),
        ));
    }
    Ok(())
}

const fn budget_limit(budget: &TurnBudget, dimension: BudgetDimension) -> u64 {
    match dimension {
        BudgetDimension::Elapsed => budget.maximum_elapsed_ms(),
        BudgetDimension::ProviderAttempts => budget.maximum_provider_attempts(),
        BudgetDimension::ToolCalls => budget.maximum_tool_calls(),
        BudgetDimension::GeneratedFacts => budget.maximum_generated_facts(),
        BudgetDimension::GeneratedFactBytes => budget.maximum_generated_fact_bytes(),
    }
}

fn check_budget_usage(budget: &TurnBudget, usage: BudgetUsage) -> TurnResult<()> {
    for (dimension, consumed, limit) in [
        (
            BudgetDimension::ProviderAttempts,
            usage.provider_attempts,
            budget.maximum_provider_attempts(),
        ),
        (
            BudgetDimension::ToolCalls,
            usage.tool_calls,
            budget.maximum_tool_calls(),
        ),
        (
            BudgetDimension::GeneratedFacts,
            usage.generated_facts,
            budget.maximum_generated_facts(),
        ),
        (
            BudgetDimension::GeneratedFactBytes,
            usage.generated_fact_bytes,
            budget.maximum_generated_fact_bytes(),
        ),
    ] {
        if consumed > limit {
            return Err(TurnError::BudgetExceeded {
                dimension,
                consumed,
                limit,
            });
        }
    }
    Ok(())
}

fn record_budget_usage(
    usage: &mut BudgetUsage,
    fact: &SessionFact,
) -> std::result::Result<(), String> {
    if matches!(
        fact.body(),
        SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::ImageRequested { .. }
            | SessionFactBody::CancelRequested { .. }
            | SessionFactBody::BudgetExhausted { .. }
            | SessionFactBody::TurnTerminal { .. }
    ) {
        return Ok(());
    }
    usage.generated_facts = usage
        .generated_facts
        .checked_add(1)
        .ok_or_else(|| "generated Fact count overflowed".to_owned())?;
    usage.generated_fact_bytes = usage
        .generated_fact_bytes
        .checked_add(
            u64::try_from(fact.encoded_len())
                .map_err(|_| "generated Fact byte length exceeds u64".to_owned())?,
        )
        .ok_or_else(|| "generated Fact bytes overflowed".to_owned())?;
    match fact.body() {
        SessionFactBody::ModelIntent { .. } | SessionFactBody::ImageIntent { .. } => {
            usage.provider_attempts = usage
                .provider_attempts
                .checked_add(1)
                .ok_or_else(|| "provider attempt count overflowed".to_owned())?;
        }
        SessionFactBody::ToolIntent { .. } => {
            usage.tool_calls = usage
                .tool_calls
                .checked_add(1)
                .ok_or_else(|| "Tool-call count overflowed".to_owned())?;
        }
        SessionFactBody::TurnAccepted { .. }
        | SessionFactBody::ImageRequested { .. }
        | SessionFactBody::CancelRequested { .. }
        | SessionFactBody::BudgetExhausted { .. }
        | SessionFactBody::ModelStarted { .. }
        | SessionFactBody::ImageStarted { .. }
        | SessionFactBody::ImageOutput { .. }
        | SessionFactBody::ModelEvent { .. }
        | SessionFactBody::ToolStarted { .. }
        | SessionFactBody::ToolResult { .. }
        | SessionFactBody::TurnTerminal { .. } => {}
    }
    Ok(())
}

fn clone_turn_control(turn: &TurnControl) -> TurnControl {
    TurnControl {
        accepted_at_ms: turn.accepted_at_ms,
        accepted_seq: turn.accepted_seq,
        terminal: turn.terminal.clone(),
        terminal_seq: turn.terminal_seq,
        cancel_requested: turn.cancel_requested,
        cancellation: turn.cancellation.clone(),
        claim: turn.claim.clone(),
        effect: turn.effect.clone(),
        budget_usage: turn.budget_usage,
        budget_exhausted: turn.budget_exhausted,
    }
}

fn canonicalize_terminal(body: SessionFactBody, cancelled: bool) -> SessionFactBody {
    match body {
        SessionFactBody::TurnTerminal {
            turn_id,
            outcome: _,
        } if cancelled => SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::Cancelled,
        },
        SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::Cancelled,
        } => SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::Failed {
                code: "executor.unrequested_cancellation".into(),
                message: "executor proposed cancellation without a durable request".into(),
            },
        },
        other => other,
    }
}

fn next_fact(
    inner: &KernelInner,
    session: &SessionRuntime,
    body: SessionFactBody,
) -> Result<Arc<SessionFact>> {
    let seq = session
        .live_seq()?
        .checked_add(1)
        .ok_or_else(|| KernelError::Invariant("Fact sequence exhausted".into()))?;
    SessionFact::new(seq, inner.clock.now_ms().max(1), body)
        .map(Arc::new)
        .map_err(KernelError::Session)
}

fn push_pending(
    inner: &KernelInner,
    session: &mut SessionRuntime,
    fact: Arc<SessionFact>,
) -> Result<()> {
    let bytes = fact.encoded_len();
    let projected = session
        .pending_bytes
        .checked_add(bytes)
        .ok_or_else(|| KernelError::Invariant("pending Fact bytes overflowed".into()))?;
    if projected > MAXIMUM_PENDING_FACT_BYTES {
        return Err(KernelError::Capacity(
            "speculative Fact buffer capacity is exhausted".into(),
        ));
    }
    reserve_atomic_capacity(
        &inner.process_pending_bytes,
        bytes,
        inner.limits.maximum_process_pending_fact_bytes,
    )?;
    session.pending_bytes = projected;
    session.pending.push_back(fact);
    Ok(())
}

fn publish_live_watermarks(session: &SessionRuntime) {
    let live_seq = session
        .live_seq()
        .expect("validated pending suffix has a representable live sequence");
    session.updates.send_replace(LiveWatermarks {
        live_seq,
        durable_seq: session.durable_seq,
    });
}

fn reserve_atomic_capacity(counter: &AtomicUsize, amount: usize, limit: usize) -> Result<()> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let projected = current
            .checked_add(amount)
            .ok_or_else(|| KernelError::Capacity("process capacity counter overflowed".into()))?;
        if projected > limit {
            return Err(KernelError::Capacity(
                "process speculative Fact capacity is exhausted".into(),
            ));
        }
        match counter.compare_exchange_weak(current, projected, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

fn enqueue(state: &mut KernelState, session_id: SessionId, turn_id: TurnId) {
    let candidate = (session_id, turn_id);
    if state.queued.insert(candidate.clone()) {
        state.claim_queue.push_back(candidate);
    }
}

fn deregister_executor(inner: &Weak<KernelInner>, executor_id: &str, registration_id: u64) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let mut state = lock_state(&inner);
    if state.executors.get(executor_id) != Some(&registration_id) {
        return;
    }
    state.executors.remove(executor_id);
    let mut released = Vec::new();
    for (session_id, session) in &mut state.sessions {
        for (turn_id, turn) in &mut session.turns {
            if turn.claim.as_ref().is_some_and(|owner| {
                owner.executor == executor_id && owner.registration == registration_id
            }) && turn.terminal.is_none()
            {
                turn.claim = None;
                released.push((session_id.clone(), turn_id.clone()));
            }
        }
    }
    for (session_id, turn_id) in released {
        enqueue(&mut state, session_id, turn_id);
    }
    drop(state);
    inner.claim_changed.notify_waiters();
}

fn lock_state(inner: &KernelInner) -> std::sync::MutexGuard<'_, KernelState> {
    inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn turn_store_error(error: StoreError) -> TurnError {
    match error {
        StoreError::Invalid(message) => TurnError::Invalid(bounded_diagnostic(&message)),
        StoreError::NotFound(session) => TurnError::SessionNotFound(session),
        StoreError::TurnNotFound { session, turn } => TurnError::TurnNotFound { session, turn },
        other => TurnError::Store(bounded_diagnostic(&other.to_string())),
    }
}

fn turn_composition_error(error: AgentCompositionError) -> TurnError {
    match error {
        AgentCompositionError::InvalidInput(message) => {
            TurnError::Invalid(bounded_diagnostic(&message))
        }
        AgentCompositionError::Unavailable { .. }
        | AgentCompositionError::DefaultUnavailable { .. }
        | AgentCompositionError::Capacity => {
            TurnError::Composition(bounded_diagnostic(&error.to_string()))
        }
        AgentCompositionError::ShuttingDown => TurnError::ShuttingDown,
    }
}

fn bounded_diagnostic(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let character = if matches!(character, '\0' | '\u{7f}') {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > MAXIMUM_AGENT_DIAGNOSTIC_BYTES {
            break;
        }
        output.push(character);
    }
    output
}

fn turn_not_found(session_id: &SessionId, turn_id: &TurnId) -> TurnError {
    TurnError::TurnNotFound {
        session: session_id.to_string(),
        turn: turn_id.to_string(),
    }
}

fn submission_conflict(session_id: &SessionId, turn_id: &TurnId) -> TurnError {
    TurnError::SubmissionConflict {
        session: session_id.to_string(),
        turn: turn_id.to_string(),
    }
}

fn turn_kernel_error(error: KernelError) -> TurnError {
    match error {
        KernelError::Flush(message) | KernelError::Shutdown(message) => TurnError::Flush(message),
        KernelError::Session(error) => TurnError::Invalid(error.to_string()),
        KernelError::Composition(message) => TurnError::Composition(message),
        KernelError::Capacity(_) => TurnError::Capacity,
        KernelError::Invariant(message) => TurnError::Invariant(message),
        KernelError::Store(error) => turn_store_error(error),
    }
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
            .requiring_local::<AgentCompositionContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let limits: KernelLimits = serde_json::from_value(plan.config().as_ref().clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let kernel = SessionKernel::recover_with_clock_and_limits(
            plan.local::<SessionStoreContract>()?,
            plan.local::<AgentCompositionContract>()?,
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
mod tests {
    use super::*;

    #[test]
    fn missing_flush_session_after_quiesce_is_a_shutdown_failure() {
        let state = KernelState {
            accepting: false,
            sessions: BTreeMap::new(),
            loading_sessions: BTreeMap::new(),
            fresh_reservations: BTreeSet::new(),
            executors: BTreeMap::new(),
            next_executor_registration: 0,
            finalizers: BTreeMap::new(),
            finalizer_names: BTreeSet::new(),
            next_finalizer_registration: 0,
            next_claim: 0,
            claim_queue: VecDeque::new(),
            queued: BTreeSet::new(),
        };
        let error =
            flush_status_receiver(&state, &SessionId::new("session-after-quiesce").unwrap())
                .expect_err("quiesced sessions have been released");
        assert!(matches!(error, KernelError::Shutdown(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn submission_admission_wait_is_bounded() {
        let admission = Arc::new(SubmissionAdmission::new());
        let mut leases = Vec::with_capacity(MAXIMUM_ACTIVE_SESSIONS);
        for index in 0..MAXIMUM_ACTIVE_SESSIONS {
            leases.push(
                admission
                    .acquire(&SessionId::new(format!("session-{index}")).unwrap())
                    .await
                    .unwrap(),
            );
        }
        let waiter = tokio::spawn({
            let admission = Arc::clone(&admission);
            async move {
                admission
                    .acquire(&SessionId::new("session-over-capacity").unwrap())
                    .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(DURABILITY_WAIT_TIMEOUT).await;
        assert!(matches!(waiter.await.unwrap(), Err(TurnError::Capacity)));
        drop(leases);
    }

    #[tokio::test]
    async fn closing_submission_admission_releases_same_session_waiters() {
        let admission = Arc::new(SubmissionAdmission::new());
        let session = SessionId::new("session-serialized").unwrap();
        let lease = admission.acquire(&session).await.unwrap();
        let waiter = tokio::spawn({
            let admission = Arc::clone(&admission);
            async move { admission.acquire(&session).await }
        });
        tokio::task::yield_now().await;
        admission.close();
        assert!(matches!(
            waiter.await.unwrap(),
            Err(TurnError::ShuttingDown)
        ));
        assert!(admission.slots.is_closed());
        drop(lease);
    }

    #[tokio::test]
    async fn same_session_waiters_do_not_consume_unrelated_active_slots() {
        let admission = Arc::new(SubmissionAdmission::new());
        let session = SessionId::new("session-contended").unwrap();
        let lease = admission.acquire(&session).await.unwrap();
        let mut waiters = Vec::with_capacity(MAXIMUM_ACTIVE_SESSIONS - 1);
        for _ in 1..MAXIMUM_ACTIVE_SESSIONS {
            waiters.push(tokio::spawn({
                let admission = Arc::clone(&admission);
                let session = session.clone();
                async move { admission.acquire(&session).await }
            }));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let queued = admission
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&session)
                    .map_or(0, Weak::strong_count);
                if queued == MAXIMUM_ACTIVE_SESSIONS {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("same-Session waiters did not all reach keyed admission");

        let unrelated = tokio::time::timeout(
            Duration::from_millis(100),
            admission.acquire(&SessionId::new("session-unrelated").unwrap()),
        )
        .await
        .expect("same-Session waiters consumed every unrelated active slot")
        .expect("unrelated Session admission");
        drop(unrelated);

        admission.close();
        drop(lease);
        for waiter in waiters {
            assert!(matches!(
                waiter.await.unwrap(),
                Err(TurnError::ShuttingDown)
            ));
        }
    }

    #[test]
    fn write_behind_deadline_rebases_after_a_slow_or_early_scan() {
        let origin = Instant::now();
        let scheduled = origin + WRITE_BEHIND_INTERVAL;
        let slow_completion = origin + WRITE_BEHIND_INTERVAL * 3;
        assert_eq!(
            rebase_write_behind_tick(scheduled, slow_completion),
            slow_completion + WRITE_BEHIND_INTERVAL
        );

        let early_notification = origin + WRITE_BEHIND_INTERVAL / 2;
        assert_eq!(
            rebase_write_behind_tick(scheduled, early_notification),
            early_notification + WRITE_BEHIND_INTERVAL
        );
    }
}
