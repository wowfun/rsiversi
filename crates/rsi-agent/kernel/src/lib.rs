//! Durable Agent turn scheduler and write-behind ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt as _};
use rsi_agent_session_protocol::{
    EffectId, EffectKind, FrozenAgentProfile, MAXIMUM_FACTS_PER_READ, SessionFact, SessionFactBody,
    SessionHeader, SessionId, TurnId, TurnOutcome, validate_identifier,
};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, MAXIMUM_SESSIONS_PER_READ, MAXIMUM_STORE_BATCH_BYTES,
    MAXIMUM_STORE_BATCH_FACTS, SessionStore, SessionStoreContract, StoreError,
};
use rsi_agent_turn_protocol::{
    CancelResult, ClaimFactPage, ExecutorLease, Result as TurnResult, SubmitImage, SubmitSession,
    SubmitTurn, SubmittedTurn, TurnClaim, TurnError, TurnExecution, TurnExecutionContract,
    TurnFinalization, TurnFinalizationContract, TurnFinalizationError, TurnFinalizer,
    TurnFinalizerLease, TurnObservation, TurnService, TurnServiceContract, TurnUpdate,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{Notify, broadcast, watch};
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
const OBSERVATION_CAPACITY: usize = 1_024;
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Identifier source injected into deterministic tests.
pub trait IdSource: fmt::Debug + Send + Sync + 'static {
    /// Produces one valid unique identity for the requested prefix.
    fn next_id(&self, prefix: &str) -> Result<String>;
}

/// Cross-process identifier source using OS entropy plus a local sequence.
#[derive(Debug, Default)]
pub struct SystemIds {
    sequence: std::sync::atomic::AtomicU64,
}

impl IdSource for SystemIds {
    fn next_id(&self, prefix: &str) -> Result<String> {
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|error| KernelError::Identity(error.to_string()))?;
        let entropy = u128::from_le_bytes(entropy);
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let value = format!("{prefix}-{entropy:032x}-{sequence:x}");
        validate_identifier(prefix, &value)
            .map_err(|error| KernelError::Identity(error.to_string()))?;
        Ok(value)
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
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdSource>,
    state: Mutex<KernelState>,
    claim_changed: Notify,
    flush_requested: Notify,
    stop_worker: CancellationToken,
}

struct KernelState {
    accepting: bool,
    sessions: BTreeMap<SessionId, SessionRuntime>,
    loading_sessions: BTreeMap<SessionId, Arc<SessionLoad>>,
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
        *self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.completed.notify_waiters();
    }
}

#[derive(Clone)]
struct FinalizerEntry {
    name: String,
    finalizer: Arc<dyn TurnFinalizer>,
}

struct SessionRuntime {
    header: SessionHeader,
    durable_seq: u64,
    pending: VecDeque<SessionFact>,
    pending_bytes: usize,
    header_pending: bool,
    turns: BTreeMap<TurnId, TurnControl>,
    turn_order: Vec<TurnId>,
    updates: broadcast::Sender<TurnUpdate>,
    flush_status: watch::Sender<FlushStatus>,
    flush_inflight: bool,
    retry_failures: u32,
    retry_not_before: Option<Instant>,
    permanent_flush_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FlushStatus {
    durable_seq: u64,
    permanent_error: Option<String>,
}

struct TurnControl {
    terminal: Option<TurnOutcome>,
    terminal_seq: Option<u64>,
    cancel_requested: bool,
    cancellation: CancellationToken,
    claim: Option<ClaimOwner>,
    effect: Option<ActiveEffect>,
}

#[derive(Clone)]
struct ClaimOwner {
    executor: String,
    registration: u64,
    claim: u64,
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
    fn new() -> Self {
        Self {
            terminal: None,
            terminal_seq: None,
            cancel_requested: false,
            cancellation: CancellationToken::new(),
            claim: None,
            effect: None,
        }
    }
}

impl SessionRuntime {
    fn new(header: SessionHeader, durable_seq: u64, header_pending: bool) -> Self {
        let (updates, _) = broadcast::channel(OBSERVATION_CAPACITY);
        let (flush_status, _) = watch::channel(FlushStatus {
            durable_seq,
            permanent_error: None,
        });
        Self {
            header,
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

fn apply_committed_flush(session: &mut SessionRuntime, commit: AppendCommit) -> Vec<TurnId> {
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
    let committed_terminal = session
        .pending
        .iter()
        .take_while(|fact| fact.seq() <= commit.durable_seq)
        .find(|fact| is_terminal_fact(fact))
        .cloned();
    while session
        .pending
        .front()
        .is_some_and(|fact| fact.seq() <= commit.durable_seq)
    {
        let fact = session.pending.pop_front().expect("front existed");
        session.pending_bytes = session.pending_bytes.saturating_sub(fact.encoded_len());
    }
    session.durable_seq = commit.durable_seq;
    session.header_pending = false;
    session.retry_failures = 0;
    session.retry_not_before = None;
    let _previous = session.flush_status.send_replace(FlushStatus {
        durable_seq: commit.durable_seq,
        permanent_error: None,
    });
    if let Some(fact) = committed_terminal {
        let _receivers = session.updates.send(TurnUpdate::Fact {
            fact: Box::new(fact),
            durable_seq: commit.durable_seq,
        });
    } else {
        let _receivers = session.updates.send(TurnUpdate::Durable {
            durable_seq: commit.durable_seq,
        });
    }
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
    pub async fn recover(store: Arc<dyn SessionStore>) -> Result<Self> {
        Self::recover_with_sources(store, Arc::new(SystemClock), Arc::new(SystemIds::default()))
            .await
    }

    /// Recovers with deterministic timestamp and identity sources.
    pub async fn recover_with_sources(
        store: Arc<dyn SessionStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
    ) -> Result<Self> {
        let mut after = None;
        loop {
            let page = store
                .list_sessions(after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
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
                clock,
                ids,
                state: Mutex::new(KernelState {
                    accepting: true,
                    sessions: BTreeMap::new(),
                    loading_sessions: BTreeMap::new(),
                    executors: BTreeMap::new(),
                    next_executor_registration: 0,
                    finalizers: BTreeMap::new(),
                    finalizer_names: BTreeSet::new(),
                    next_finalizer_registration: 0,
                    next_claim: 0,
                    claim_queue: VecDeque::new(),
                    queued: BTreeSet::new(),
                }),
                claim_changed: Notify::new(),
                flush_requested: Notify::new(),
                stop_worker: CancellationToken::new(),
            }),
        })
    }

    /// Starts the sole background write-behind worker.
    pub fn start_write_behind(&self) -> JoinHandle<()> {
        let kernel = self.clone();
        let first_tick = Instant::now() + WRITE_BEHIND_INTERVAL;
        tokio::spawn(async move { kernel.flush_loop(first_tick).await })
    }

    /// Stops admission, durably drains pending Facts, and ends the worker.
    pub async fn shutdown(&self, mut worker: JoinHandle<()>) -> Result<()> {
        {
            let mut state = lock_state(&self.inner);
            state.accepting = false;
        }
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
        flush_result.and(worker_result)
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
            let Some(batch) = self.prepare_flush_batch(&session_id) else {
                continue;
            };
            let result = self.inner.store.append(batch).await;
            self.complete_flush(&session_id, result);
        }
    }

    fn prepare_flush_batch(&self, session_id: &SessionId) -> Option<AppendBatch> {
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
            facts.push(fact.clone());
        }
        session.flush_inflight = true;
        Some(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: session.durable_seq,
            header: session.header_pending.then(|| session.header.clone()),
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
        {
            let mut state = lock_state(&self.inner);
            let Some(session) = state.sessions.get_mut(session_id) else {
                return;
            };
            session.flush_inflight = false;
            match result {
                Ok(commit) => {
                    pruned_turns = apply_committed_flush(session, commit);
                    enqueue_after_commit = true;
                    request_more = !session.pending.is_empty();
                    evict_session = session.turns.is_empty() && session.pending.is_empty();
                }
                Err(StoreError::Io(_)) => {
                    session.retry_failures = session.retry_failures.saturating_add(1);
                    if session.retry_failures >= MAXIMUM_CONSECUTIVE_FLUSH_FAILURES {
                        session.permanent_flush_error = Some(format!(
                            "Store append failed {MAXIMUM_CONSECUTIVE_FLUSH_FAILURES} consecutive times"
                        ));
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
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| KernelError::Invariant("session disappeared during flush".into()))?
                .flush_status
                .subscribe()
        };
        self.inner.flush_requested.notify_one();
        self.wait_on_flush_status(status, through_seq).await
    }

    async fn wait_on_flush_status(
        &self,
        mut status: watch::Receiver<FlushStatus>,
        through_seq: u64,
    ) -> Result<u64> {
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
            }
        }
    }

    fn validate_claim<'a>(
        state: &'a KernelState,
        claim: &TurnClaim,
    ) -> TurnResult<&'a TurnControl> {
        let registration_id = state
            .executors
            .get(&claim.executor_id)
            .copied()
            .ok_or(TurnError::StaleClaim)?;
        let session = state
            .sessions
            .get(&claim.session_id)
            .ok_or(TurnError::StaleClaim)?;
        let turn = session
            .turns
            .get(&claim.turn_id)
            .ok_or(TurnError::StaleClaim)?;
        match &turn.claim {
            Some(owner)
                if owner.executor == claim.executor_id
                    && owner.registration == registration_id
                    && owner.claim == claim.claim_id =>
            {
                Ok(turn)
            }
            _ => Err(TurnError::StaleClaim),
        }
    }

    async fn prepare_submission_profile(
        &self,
        selection: &SubmitSession,
    ) -> TurnResult<FrozenAgentProfile> {
        match selection {
            SubmitSession::Fresh(header) => {
                if lock_state(&self.inner)
                    .sessions
                    .contains_key(header.session_id())
                {
                    return Err(TurnError::Invalid(
                        "fresh submission selected an existing session".into(),
                    ));
                }
                match self.inner.store.header(header.session_id()).await {
                    Ok(_) => Err(TurnError::Invalid(
                        "fresh submission selected an existing session".into(),
                    )),
                    Err(StoreError::NotFound(_)) => Ok(header.profile().clone()),
                    Err(error) => Err(turn_store_error(error)),
                }
            }
            SubmitSession::Resume(session_id) => {
                if let Some(profile) = lock_state(&self.inner)
                    .sessions
                    .get(session_id)
                    .map(|session| session.header.profile().clone())
                {
                    return Ok(profile);
                }
                self.inner
                    .store
                    .header(session_id)
                    .await
                    .map(|header| header.profile().clone())
                    .map_err(turn_store_error)
            }
        }
    }

    async fn ensure_session_loaded(&self, session_id: &SessionId) -> TurnResult<()> {
        let (load, leader) = {
            let mut state = lock_state(&self.inner);
            if state.sessions.contains_key(session_id) {
                return Ok(());
            }
            if let Some(load) = state.loading_sessions.get(session_id) {
                (Arc::clone(load), false)
            } else {
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
        let header = self
            .inner
            .store
            .header(session_id)
            .await
            .map_err(turn_store_error);
        let result = match header {
            Ok(header) => load_control_state(&self.inner.store, session_id)
                .await
                .map_err(turn_kernel_error)
                .and_then(|(durable_seq, turns, turn_order)| {
                    let mut state = lock_state(&self.inner);
                    if state.sessions.contains_key(session_id) {
                        return Ok(());
                    }
                    if state.sessions.len() >= MAXIMUM_ACTIVE_SESSIONS {
                        return Err(TurnError::Capacity);
                    }
                    let mut session = SessionRuntime::new(header, durable_seq, false);
                    session.turns = turns;
                    session.turn_order = turn_order;
                    let queued = session.turn_order.clone();
                    state.sessions.insert(session_id.clone(), session);
                    for turn_id in queued {
                        enqueue(&mut state, session_id.clone(), turn_id);
                    }
                    Ok(())
                }),
            Err(error) => Err(error),
        };
        load.complete(result.clone());
        lock_state(&self.inner).loading_sessions.remove(session_id);
        if result.is_ok() {
            self.inner.claim_changed.notify_waiters();
        }
        result
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
            return Err(TurnError::ShuttingDown);
        }
        let inserted_fresh = matches!(&session_selection, SubmitSession::Fresh(_));
        match session_selection {
            SubmitSession::Fresh(header) => {
                header
                    .validate()
                    .map_err(|error| TurnError::Invalid(error.to_string()))?;
                if state.sessions.contains_key(&session_id) {
                    return Err(TurnError::Invalid(
                        "fresh submission selected an existing session".into(),
                    ));
                }
                if state.sessions.len() >= MAXIMUM_ACTIVE_SESSIONS {
                    return Err(TurnError::Capacity);
                }
                state
                    .sessions
                    .insert(session_id.clone(), SessionRuntime::new(header, 0, true));
            }
            SubmitSession::Resume(_) => {
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
                    "generated duplicate turn identity".into(),
                ));
            }
            let fact = next_fact(&self.inner, session, body).map_err(turn_kernel_error)?;
            push_pending(session, fact.clone()).map_err(turn_kernel_error)?;
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
        session.turns.insert(turn_id.clone(), TurnControl::new());
        session.turn_order.push(turn_id.clone());
        let _receivers = session.updates.send(TurnUpdate::Fact {
            fact: Box::new(fact),
            durable_seq: session.durable_seq,
        });
        enqueue(&mut state, session_id.clone(), turn_id.clone());
        drop(state);
        self.inner.claim_changed.notify_waiters();
        Ok(SubmittedTurn {
            session_id,
            turn_id,
            accepted_seq,
        })
    }
}

#[async_trait]
impl TurnService for SessionKernel {
    async fn submit(&self, request: SubmitTurn) -> TurnResult<SubmittedTurn> {
        let profile = self.prepare_submission_profile(&request.session).await?;
        let sandbox = request.sandbox.unwrap_or(profile.sandbox());
        let require_approval =
            profile.require_approval() || sandbox == rsi_sandbox::SandboxMode::DangerFullAccess;
        let turn_id = TurnId::new(self.inner.ids.next_id("turn").map_err(turn_kernel_error)?)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let body = SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: request.text,
            model: request.model,
            sandbox,
            require_approval,
        };
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if let SubmitSession::Resume(session_id) = &request.session {
            self.ensure_session_loaded(session_id).await?;
        }
        self.accept_turn(request.session, turn_id, body)
    }

    async fn submit_image(&self, request: SubmitImage) -> TurnResult<SubmittedTurn> {
        let _profile = self.prepare_submission_profile(&request.session).await?;
        let turn_id = TurnId::new(self.inner.ids.next_id("turn").map_err(turn_kernel_error)?)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let body = SessionFactBody::ImageRequested {
            turn_id: turn_id.clone(),
            model: request.model,
            request: request.request,
        };
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if let SubmitSession::Resume(session_id) = &request.session {
            self.ensure_session_loaded(session_id).await?;
        }
        self.accept_turn(request.session, turn_id, body)
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
            if read_stored_outcome(&self.inner.store, session_id, turn_id)
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
            return match read_stored_outcome(&self.inner.store, session_id, turn_id).await? {
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
            push_pending(session, fact.clone()).map_err(turn_kernel_error)?;
            session
                .turns
                .get_mut(turn_id)
                .expect("validated turn exists")
                .cancel_requested = true;
            let _receivers = session.updates.send(TurnUpdate::Fact {
                fact: Box::new(fact),
                durable_seq: session.durable_seq,
            });
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
        let live_snapshot = {
            let state = lock_state(&self.inner);
            state.sessions.get(session_id).map(|session| {
                (
                    session.updates.subscribe(),
                    Some(session.flush_status.subscribe()),
                    session.durable_seq,
                    session.live_seq(),
                    session
                        .pending
                        .iter()
                        .filter(|fact| fact.seq() > after_seq && !is_terminal_fact(fact))
                        .cloned()
                        .collect::<VecDeque<_>>(),
                )
            })
        };
        let (receiver, flush_status, durable_target, live_seq, pending) = if let Some((
            receiver,
            flush_status,
            durable_target,
            live_seq,
            pending,
        )) = live_snapshot
        {
            (
                receiver,
                flush_status,
                durable_target,
                live_seq.map_err(turn_kernel_error)?,
                pending,
            )
        } else {
            let page = self
                .inner
                .store
                .read_facts(session_id, 0, 1)
                .await
                .map_err(turn_store_error)?;
            let state = lock_state(&self.inner);
            if let Some(session) = state.sessions.get(session_id) {
                (
                    session.updates.subscribe(),
                    Some(session.flush_status.subscribe()),
                    session.durable_seq,
                    session.live_seq().map_err(turn_kernel_error)?,
                    session
                        .pending
                        .iter()
                        .filter(|fact| fact.seq() > after_seq && !is_terminal_fact(fact))
                        .cloned()
                        .collect(),
                )
            } else {
                let (sender, receiver) = broadcast::channel(1);
                drop(sender);
                (
                    receiver,
                    None,
                    page.durable_seq,
                    page.durable_seq,
                    VecDeque::new(),
                )
            }
        };
        if after_seq > live_seq {
            return Err(TurnError::Invalid(
                "observation cursor exceeds the live tail".into(),
            ));
        }
        let state = ObservationState {
            store: Arc::clone(&self.inner.store),
            session_id: session_id.clone(),
            cursor: after_seq,
            durable_target,
            durable_buffer: VecDeque::new(),
            pending,
            receiver,
            flush_status,
            ended: false,
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
        read_stored_outcome(&self.inner.store, session_id, turn_id).await
    }

    async fn session_header(&self, session_id: &SessionId) -> TurnResult<SessionHeader> {
        if let Some(header) = lock_state(&self.inner)
            .sessions
            .get(session_id)
            .map(|session| session.header.clone())
        {
            return Ok(header);
        }
        self.inner
            .store
            .header(session_id)
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
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> rsi_agent_turn_protocol::FinalizationResult<()> {
        let finalizers = lock_state(&self.inner)
            .finalizers
            .values()
            .map(|entry| Arc::clone(&entry.finalizer))
            .collect::<Vec<_>>();
        for finalizer in finalizers {
            finalizer.finalize(session_id, turn_id).await?;
        }
        Ok(())
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
                    session
                        .turns
                        .get_mut(&turn_id)
                        .expect("claimable turn was observed")
                        .claim = Some(ClaimOwner {
                        executor: executor_id.into(),
                        registration: registration_id,
                        claim: claim_id,
                    });
                    return Ok(Some(TurnClaim {
                        executor_id: executor_id.into(),
                        claim_id,
                        session_id,
                        turn_id,
                        header: session.header.clone(),
                        live_seq: session.live_seq().map_err(turn_kernel_error)?,
                    }));
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
            Self::validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(&claim.session_id)
                .expect("validated claim session exists");
            let claimed_index = session
                .turn_order
                .iter()
                .position(|turn_id| turn_id == &claim.turn_id)
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
            let page = self
                .inner
                .store
                .read_facts(&claim.session_id, after_seq, limit)
                .await
                .map_err(turn_store_error)?;
            scanned = page.facts.len();
            through_seq = page
                .facts
                .last()
                .map_or(after_seq, SessionFact::seq)
                .min(live_seq);
            facts.extend(page.facts.into_iter().filter(|fact| {
                fact.seq() <= live_seq && !hidden_turns.contains(fact.body().turn_id())
            }));
            let current_durable_seq = {
                let state = lock_state(&self.inner);
                Self::validate_claim(&state, claim)?;
                state
                    .sessions
                    .get(&claim.session_id)
                    .expect("validated claim session exists")
                    .durable_seq
            };
            if through_seq < current_durable_seq || through_seq == live_seq || scanned == limit {
                return Ok(ClaimFactPage { facts, through_seq });
            }
        }
        if scanned < limit && through_seq < live_seq {
            let state = lock_state(&self.inner);
            Self::validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(&claim.session_id)
                .expect("validated claim session exists");
            let pending_after = through_seq;
            for fact in session
                .pending
                .iter()
                .filter(|fact| fact.seq() > pending_after)
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

    async fn publish(
        &self,
        claim: &TurnClaim,
        bodies: Vec<SessionFactBody>,
    ) -> TurnResult<Vec<SessionFact>> {
        if bodies.is_empty() || bodies.len() > MAXIMUM_STORE_BATCH_FACTS {
            return Err(TurnError::Invalid(
                "Fact publication batch is empty or too large".into(),
            ));
        }
        let mut state = lock_state(&self.inner);
        Self::validate_claim(&state, claim)?;
        let session = state
            .sessions
            .get_mut(&claim.session_id)
            .expect("validated claim session exists");
        let original = session
            .turns
            .get(&claim.turn_id)
            .expect("validated claim turn exists");
        let mut staged = clone_turn_control(original);
        let mut normalized = Vec::with_capacity(bodies.len());
        for body in bodies {
            if body.turn_id() != &claim.turn_id {
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
            let fact = SessionFact::new(next_seq, self.inner.clock.now_ms().max(1), body)
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
            added_bytes = added_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| TurnError::Invalid("Fact bytes overflowed".into()))?;
            facts.push(fact);
        }
        if session.pending_bytes.saturating_add(added_bytes) > MAXIMUM_PENDING_FACT_BYTES {
            return Err(TurnError::Flush(
                "speculative Fact buffer is full; flush before publishing more".into(),
            ));
        }
        let terminal = staged.terminal.is_some();
        if terminal {
            staged.terminal_seq = facts.last().map(SessionFact::seq);
        }
        *session
            .turns
            .get_mut(&claim.turn_id)
            .expect("validated claim turn exists") = staged;
        for fact in &facts {
            push_pending(session, fact.clone()).map_err(turn_kernel_error)?;
            if !is_terminal_fact(fact) {
                let _receivers = session.updates.send(TurnUpdate::Fact {
                    fact: Box::new(fact.clone()),
                    durable_seq: session.durable_seq,
                });
            }
        }
        drop(state);
        Ok(facts)
    }

    async fn flush(&self, claim: &TurnClaim, through_seq: u64) -> TurnResult<u64> {
        let status = {
            let state = lock_state(&self.inner);
            Self::validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(&claim.session_id)
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
        let turn = Self::validate_claim(&state, claim)?;
        Ok(turn.cancellation.clone())
    }

    fn release(&self, claim: &TurnClaim) -> TurnResult<()> {
        let mut state = lock_state(&self.inner);
        Self::validate_claim(&state, claim)?;
        let turn = state
            .sessions
            .get_mut(&claim.session_id)
            .expect("validated claim session exists")
            .turns
            .get_mut(&claim.turn_id)
            .expect("validated claim turn exists");
        if turn.terminal.is_none() {
            turn.claim = None;
            enqueue(&mut state, claim.session_id.clone(), claim.turn_id.clone());
        }
        drop(state);
        self.inner.claim_changed.notify_waiters();
        Ok(())
    }
}

struct ObservationState {
    store: Arc<dyn SessionStore>,
    session_id: SessionId,
    cursor: u64,
    durable_target: u64,
    durable_buffer: VecDeque<SessionFact>,
    pending: VecDeque<SessionFact>,
    receiver: broadcast::Receiver<TurnUpdate>,
    flush_status: Option<watch::Receiver<FlushStatus>>,
    ended: bool,
}

enum ObservationSignal {
    Update(std::result::Result<TurnUpdate, broadcast::error::RecvError>),
    Flush(std::result::Result<(), watch::error::RecvError>),
}

async fn observation_next(
    mut state: ObservationState,
) -> Option<(TurnResult<TurnUpdate>, ObservationState)> {
    if state.ended {
        return None;
    }
    loop {
        if let Some(fact) = state.durable_buffer.pop_front() {
            state.cursor = fact.seq();
            return Some((
                Ok(TurnUpdate::Fact {
                    fact: Box::new(fact),
                    durable_seq: state.durable_target,
                }),
                state,
            ));
        }
        if state.cursor < state.durable_target {
            match state
                .store
                .read_facts(&state.session_id, state.cursor, MAXIMUM_FACTS_PER_READ)
                .await
            {
                Ok(page) => {
                    state.durable_buffer.extend(
                        page.facts
                            .into_iter()
                            .filter(|fact| fact.seq() <= state.durable_target),
                    );
                    if state.durable_buffer.is_empty() {
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
        if let Some(fact) = state.pending.pop_front() {
            state.cursor = fact.seq();
            return Some((
                Ok(TurnUpdate::Fact {
                    fact: Box::new(fact),
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
                update = state.receiver.recv() => ObservationSignal::Update(update),
                changed = status.changed() => ObservationSignal::Flush(changed),
            }
        } else {
            ObservationSignal::Update(state.receiver.recv().await)
        };
        match signal {
            ObservationSignal::Flush(Ok(())) => {}
            ObservationSignal::Flush(Err(_)) => {
                state.flush_status = None;
            }
            ObservationSignal::Update(Ok(update)) => return Some((Ok(update), state)),
            ObservationSignal::Update(Err(broadcast::error::RecvError::Lagged(_))) => {
                state.ended = true;
                return Some((
                    Err(TurnError::Invariant(
                        "live observation lagged; caller must reattach from a Fact cursor".into(),
                    )),
                    state,
                ));
            }
            ObservationSignal::Update(Err(broadcast::error::RecvError::Closed)) => return None,
        }
    }
}

fn observation_flush_result(state: &mut ObservationState, error: String) -> TurnResult<TurnUpdate> {
    match state.receiver.try_recv() {
        Ok(update) => Ok(update),
        Err(broadcast::error::TryRecvError::Lagged(_)) => {
            state.ended = true;
            Err(TurnError::Invariant(
                "live observation lagged; caller must reattach from a Fact cursor".into(),
            ))
        }
        Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
            state.ended = true;
            Err(TurnError::Flush(error))
        }
    }
}

async fn repair_unfinished_session(
    store: &Arc<dyn SessionStore>,
    clock: &dyn Clock,
    session_id: &SessionId,
) -> Result<()> {
    let (durable_seq, turns, turn_order) = load_control_state(store, session_id).await?;
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
    session_id: &SessionId,
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
                let turn_page = store
                    .read_turn_facts(
                        session_id,
                        &open_turn.turn_id,
                        turn_cursor,
                        MAXIMUM_FACTS_PER_READ,
                    )
                    .await?;
                if turn_page.durable_seq != page.durable_seq {
                    return Err(KernelError::Invariant(
                        "Store durable watermark changed during per-turn load".into(),
                    ));
                }
                for fact in &turn_page.facts {
                    apply_recovered_fact(&mut turns, &mut order, fact)?;
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
    store: &Arc<dyn SessionStore>,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> TurnResult<Option<TurnOutcome>> {
    let mut cursor = 0_u64;
    let mut accepted = false;
    loop {
        let page = store
            .read_turn_facts(session_id, turn_id, cursor, MAXIMUM_FACTS_PER_READ)
            .await
            .map_err(turn_store_error)?;
        for fact in &page.facts {
            if fact.body().turn_id() != turn_id {
                cursor = fact.seq();
                continue;
            }
            match fact.body() {
                SessionFactBody::TurnAccepted { .. } | SessionFactBody::ImageRequested { .. } => {
                    if accepted {
                        return Err(TurnError::Invariant(
                            "durable turn was accepted more than once".into(),
                        ));
                    }
                    accepted = true;
                }
                SessionFactBody::TurnTerminal { outcome, .. } => {
                    if !accepted {
                        return Err(TurnError::Invariant(
                            "durable terminal precedes turn acceptance".into(),
                        ));
                    }
                    return Ok(Some(outcome.clone()));
                }
                _ if !accepted => {
                    return Err(TurnError::Invariant(
                        "durable turn Fact precedes acceptance".into(),
                    ));
                }
                _ => {}
            }
            cursor = fact.seq();
        }
        if !page.has_more {
            return if accepted {
                Ok(None)
            } else {
                Err(TurnError::Invariant(
                    "Store turn index omitted the acceptance Fact".into(),
                ))
            };
        }
        if page.facts.is_empty() {
            return Err(TurnError::Invariant(
                "historical outcome scan made no progress".into(),
            ));
        }
    }
}

fn apply_recovered_fact(
    turns: &mut BTreeMap<TurnId, TurnControl>,
    order: &mut Vec<TurnId>,
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
            if turns.insert(turn_id.clone(), TurnControl::new()).is_some() {
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
            apply_executor_body(turn, body)
                .map_err(|error| KernelError::Invariant(error.to_string()))?;
        }
    }
    Ok(())
}

fn apply_executor_body(turn: &mut TurnControl, body: &SessionFactBody) -> TurnResult<()> {
    if turn.terminal.is_some() {
        return Err(TurnError::Invalid("Fact follows a terminal turn".into()));
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
        SessionFactBody::TurnTerminal { outcome, .. } => {
            outcome
                .validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
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

fn clone_turn_control(turn: &TurnControl) -> TurnControl {
    TurnControl {
        terminal: turn.terminal.clone(),
        terminal_seq: turn.terminal_seq,
        cancel_requested: turn.cancel_requested,
        cancellation: turn.cancellation.clone(),
        claim: turn.claim.clone(),
        effect: turn.effect.clone(),
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
) -> Result<SessionFact> {
    let seq = session
        .live_seq()?
        .checked_add(1)
        .ok_or_else(|| KernelError::Invariant("Fact sequence exhausted".into()))?;
    SessionFact::new(seq, inner.clock.now_ms().max(1), body).map_err(KernelError::Session)
}

fn push_pending(session: &mut SessionRuntime, fact: SessionFact) -> Result<()> {
    let bytes = fact.encoded_len();
    let projected = session
        .pending_bytes
        .checked_add(bytes)
        .ok_or_else(|| KernelError::Invariant("pending Fact bytes overflowed".into()))?;
    if projected > MAXIMUM_PENDING_FACT_BYTES {
        return Err(KernelError::Flush(
            "speculative Fact buffer capacity is exhausted".into(),
        ));
    }
    session.pending_bytes = projected;
    session.pending.push_back(fact);
    Ok(())
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
        StoreError::NotFound(session) => TurnError::SessionNotFound(session),
        StoreError::TurnNotFound { session, turn } => TurnError::TurnNotFound { session, turn },
        other => TurnError::Flush(other.to_string()),
    }
}

fn turn_not_found(session_id: &SessionId, turn_id: &TurnId) -> TurnError {
    TurnError::TurnNotFound {
        session: session_id.to_string(),
        turn: turn_id.to_string(),
    }
}

fn turn_kernel_error(error: KernelError) -> TurnError {
    match error {
        KernelError::Flush(message) | KernelError::Shutdown(message) => TurnError::Flush(message),
        KernelError::Session(error) => TurnError::Invalid(error.to_string()),
        KernelError::Identity(message) => TurnError::Invalid(message),
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
    /// Identity generation failed.
    #[error("Agent identity generation failed: {0}")]
    Identity(String),
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

/// Ordinary Kernel factory requiring one exact Agent Store Local supply.
#[derive(Clone, Debug, Default)]
pub struct KernelFactory;

#[async_trait]
impl PluginFactory for KernelFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Agent Kernel configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<SessionStoreContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let kernel = SessionKernel::recover(plan.local::<SessionStoreContract>()?)
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
