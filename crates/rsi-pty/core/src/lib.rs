//! Generation-owned bounded live terminal scopes and attachment authority.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::{ManagedPtyProcess, PtyProcess, PtyProcessContract, PtyProcessSpec};
use rsi_pty_protocol::{
    Attachment, InputReceipt, InputState, MAXIMUM_ATTACHMENTS, MAXIMUM_FOLLOWER_BYTES,
    MAXIMUM_OUTPUT_PAGE_BYTES, MAXIMUM_TERMINALS, Operation, OutputPage, Phase, PtyError,
    PtyProvider, PtyProviderContract, PtyScope, Reply, Result, Size, Terminal,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
mod filter;
mod operations;
mod output;
const SCREEN_BYTES: usize = 256 * 1024 * 1024;
const SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const QUEUE_BYTES: usize = 64 * 1024 * 1024;
const RECEIPTS: usize = 32;
const MAXIMUM_SCOPES: usize = 256;
const MAXIMUM_RETAINED_TERMINALS: usize = 256;
const FOLLOWER_IDLE: Duration = Duration::from_mins(1);

#[derive(Debug)]
struct Provider {
    shared: Arc<Shared>,
}
struct Shared {
    processes: Arc<dyn PtyProcess>,
    generation: String,
    next: AtomicU64,
    stopped: AtomicBool,
    creating: Creations,
    scopes: Mutex<Vec<Weak<Scope>>>,
    terminals: Mutex<Vec<Weak<Term>>>,
    slots: Arc<Semaphore>,
    screens: Arc<Semaphore>,
    snapshots: Arc<Semaphore>,
    queues: Arc<Semaphore>,
}
impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyProvider")
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}
impl Shared {
    fn id(&self, prefix: &str) -> Result<String> {
        let next = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
            .map_err(|_| PtyError::Capacity)?;
        Ok(format!("{prefix}-{}-{next:x}", self.generation))
    }
    fn snapshot(&self, parser: &vt100::Parser, size: Size) -> Result<Arc<Snapshot>> {
        let maximum = snapshot_bytes(size)?;
        let permit = reserve(&self.snapshots, maximum)?;
        snapshot(parser, maximum, permit)
    }
    fn reclaim_followers(&self, now: std::time::Instant) {
        let terminals: Vec<_> = lock(&self.terminals)
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for term in terminals {
            lock(&term.inner).reclaim_followers(now);
        }
    }
    fn follower_resources(
        &self,
        parser: &vt100::Parser,
        size: Size,
    ) -> Result<(OwnedSemaphorePermit, Arc<Snapshot>)> {
        let queue = reserve(&self.queues, MAXIMUM_FOLLOWER_BYTES)?;
        Ok((queue, self.snapshot(parser, size)?))
    }
    async fn retire(&self) -> Result<()> {
        {
            let _registry = lock(&self.terminals);
            self.stopped.store(true, Ordering::Release);
        }
        self.creating.wait().await;
        let terminals = lock(&self.terminals)
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for terminal in &terminals {
            terminal.process.terminate();
        }
        let mut failure = None;
        for result in
            futures_util::future::join_all(terminals.iter().map(|terminal| terminal.closed())).await
        {
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
#[derive(Debug, Default)]
struct Creations {
    active: AtomicUsize,
    changed: Notify,
}
impl Creations {
    fn admit(&self) -> Creating<'_> {
        self.active.fetch_add(1, Ordering::AcqRel);
        Creating(self)
    }
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }
}
struct Creating<'a>(&'a Creations);
impl Drop for Creating<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
#[derive(Debug)]
struct Scope {
    closing: tokio::sync::Mutex<()>,
    shared: Arc<Shared>,
    inner: Mutex<ScopeState>,
    creating: Creations,
}
#[derive(Debug, Default)]
struct ScopeState {
    retired: bool,
    terminals: BTreeMap<String, Arc<Term>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        for term in lock(&self.inner).terminals.values() {
            term.process.terminate();
        }
    }
}
impl PtyProvider for Provider {
    fn scope(&self) -> Result<Arc<dyn PtyScope>> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let mut scopes = lock(&self.shared.scopes);
        scopes.retain(|scope| scope.strong_count() > 0);
        if scopes.len() >= MAXIMUM_SCOPES {
            return Err(PtyError::Capacity);
        }
        let scope = Arc::new(Scope {
            closing: tokio::sync::Mutex::new(()),
            shared: self.shared.clone(),
            inner: Mutex::new(ScopeState::default()),
            creating: Creations::default(),
        });
        scopes.push(Arc::downgrade(&scope));
        Ok(scope)
    }
}
struct Term {
    shared: Arc<Shared>,
    process: ManagedPtyProcess,
    inner: Mutex<TermState>,
    changed: Notify,
    reader_done: AtomicBool,
    _slot: OwnedSemaphorePermit,
}
impl fmt::Debug for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("id", &lock(&self.inner).status.id)
            .finish_non_exhaustive()
    }
}
struct TermState {
    screen_size: Size,
    status: Terminal,
    parser: vt100::Parser,
    filter: filter::Filter,
    screen: OwnedSemaphorePermit,
    followers: BTreeMap<String, Follower>,
    next_input: u64,
    inflight: bool,
    receipts: VecDeque<Record>,
}
impl TermState {
    fn reclaim_followers(&mut self, now: std::time::Instant) {
        self.followers.retain(|_, follower| {
            now.saturating_duration_since(follower.last_read) < FOLLOWER_IDLE
        });
        if self
            .status
            .controller
            .as_ref()
            .is_some_and(|id| !self.followers.contains_key(id))
        {
            self.status.controller = None;
            // Saturation revokes authority permanently; takeover cannot increment it.
            self.status.controller_epoch = self.status.controller_epoch.saturating_add(1);
            self.next_input = 1;
        }
    }
}
struct Snapshot {
    text: String,
    permit: OwnedSemaphorePermit,
}
struct Follower {
    last_read: std::time::Instant,
    snapshot: Arc<Snapshot>,
    stream_epoch: u64,
    chunks: VecDeque<Chunk>,
    oldest: u64,
    end: u64,
    bytes: usize,
    reading: Arc<Semaphore>,
    _queue: OwnedSemaphorePermit,
}
struct Chunk {
    start: u64,
    text: Arc<str>,
}
struct Record {
    epoch: u64,
    sequence: u64,
    digest: [u8; 32],
    receipt: InputState,
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn unavailable() -> PtyError {
    PtyError::Unavailable("live terminal or attachment is absent".into())
}
fn native(error: rsi_process::ProcessError) -> PtyError {
    match error {
        rsi_process::ProcessError::Capacity => PtyError::Capacity,
        rsi_process::ProcessError::InvalidInput(message) => {
            PtyError::Invalid(message.chars().take(256).collect())
        }
        rsi_process::ProcessError::Unsupported | rsi_process::ProcessError::ShuttingDown => {
            unavailable()
        }
        error => PtyError::Io(error.to_string().chars().take(256).collect()),
    }
}
fn reserve(budget: &Arc<Semaphore>, bytes: usize) -> Result<OwnedSemaphorePermit> {
    budget
        .clone()
        .try_acquire_many_owned(u32::try_from(bytes).map_err(|_| PtyError::Capacity)?)
        .map_err(|_| PtyError::Capacity)
}
fn screen_bytes(size: Size) -> Result<usize> {
    size.validate()?;
    Ok((usize::from(size.rows) * 2 + 1000) * usize::from(size.columns) * 64 + 64 * 1024)
}
fn snapshot_bytes(size: Size) -> Result<usize> {
    size.validate()?;
    Ok(usize::from(size.rows) * usize::from(size.columns) * 160 + 16 * 1024)
}
fn snapshot(
    parser: &vt100::Parser,
    maximum: usize,
    permit: OwnedSemaphorePermit,
) -> Result<Arc<Snapshot>> {
    let bytes = parser.screen().state_formatted();
    if bytes.len() > maximum {
        return Err(PtyError::Capacity);
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| PtyError::Io("terminal snapshot was not UTF-8".into()))?;
    Ok(Arc::new(Snapshot { text, permit }))
}
impl Scope {
    fn term(&self, id: &str) -> Result<Arc<Term>> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let state = lock(&self.inner);
        if state.retired {
            return Err(unavailable());
        }
        state.terminals.get(id).cloned().ok_or_else(unavailable)
    }
    async fn close_all(&self, retire: bool) -> Result<()> {
        let _closing = self.closing.lock().await;
        {
            let mut state = lock(&self.inner);
            state.retired |= retire;
        }
        self.creating.wait().await;
        let terminals = lock(&self.inner).terminals.clone();
        for term in terminals.values() {
            term.process.terminate();
        }
        let mut error = None;
        for result in
            futures_util::future::join_all(terminals.values().map(|term| term.closed())).await
        {
            if let Err(failure) = result {
                error.get_or_insert(failure);
            }
        }
        let mut state = lock(&self.inner);
        for id in terminals.keys() {
            state.terminals.remove(id);
        }
        error.map_or(Ok(()), Err)
    }
}
#[async_trait]
impl PtyScope for Scope {
    fn is_empty(&self) -> bool {
        let state = lock(&self.inner);
        state.terminals.is_empty() && self.creating.active.load(Ordering::Acquire) == 0
    }
    fn create(&self, spec: PtyProcessSpec) -> Result<Attachment> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PtyError::Unavailable("Tokio runtime is unavailable".into()))?;
        spec.validate().map_err(native)?;
        let terminals = lock(&self.shared.terminals);
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let state = lock(&self.inner);
        if state.retired {
            return Err(unavailable());
        }
        if state.terminals.len() + self.creating.active.load(Ordering::Acquire) >= MAXIMUM_TERMINALS
        {
            return Err(PtyError::Capacity);
        }
        let slot = reserve(&self.shared.slots, 1)?;
        let _global_creation = self.shared.creating.admit();
        let _scope_creation = self.creating.admit();
        drop(state);
        drop(terminals);
        let size = Size {
            rows: spec.size.rows,
            columns: spec.size.columns,
        };
        let screen = reserve(&self.shared.screens, screen_bytes(size)?)?;
        let parser = vt100::Parser::new(size.rows, size.columns, 1000);
        let (queue, snapshot) = match self.shared.follower_resources(&parser, size) {
            Err(PtyError::Capacity) => {
                self.shared.reclaim_followers(std::time::Instant::now());
                self.shared.follower_resources(&parser, size)?
            }
            result => result?,
        };
        let id = self.shared.id("pty")?;
        let attachment = self.shared.id("view")?;
        let status = Terminal {
            id: id.clone(),
            size,
            phase: Phase::Running,
            controller: Some(attachment.clone()),
            controller_epoch: 1,
        };
        let process = self.shared.processes.spawn(spec).map_err(native)?;
        let term = Arc::new(Term {
            shared: self.shared.clone(),
            process,
            inner: Mutex::new(TermState {
                screen_size: size,
                status: status.clone(),
                parser,
                filter: filter::Filter::default(),
                screen,
                followers: BTreeMap::from([(
                    attachment.clone(),
                    Follower::new(snapshot, queue, 1),
                )]),
                next_input: 1,
                inflight: false,
                receipts: VecDeque::new(),
            }),
            changed: Notify::new(),
            reader_done: AtomicBool::new(false),
            _slot: slot,
        });
        let mut terminals = lock(&self.shared.terminals);
        let mut state = lock(&self.inner);
        let retired = self.shared.stopped.load(Ordering::Acquire) || state.retired;
        // Even a spawn racing retirement must remain owned until its reader/reaper settles.
        if retired {
            term.process.terminate();
        }
        terminals.retain(|value| value.strong_count() > 0);
        terminals.push(Arc::downgrade(&term));
        drop(terminals);
        state.terminals.insert(id, term.clone());
        drop(state);
        let guard = ReaderGuard(term.clone());
        runtime.spawn(async move {
            term.drain().await;
            drop(guard);
        });
        if retired {
            return Err(unavailable());
        }
        Ok(Attachment {
            terminal: status,
            id: attachment,
            stream_epoch: 1,
        })
    }
    async fn execute(&self, operation: Operation) -> Result<Reply> {
        self.dispatch(operation).await
    }
    async fn retire(&self) -> Result<()> {
        self.close_all(true).await
    }
}
struct ReaderGuard(Arc<Term>);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.process.terminate();
        let mut state = lock(&self.0.inner);
        if matches!(state.status.phase, Phase::Running) {
            state.status.phase = Phase::Failed;
        }
        drop(state);
        self.0.reader_done.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}
impl Term {
    async fn drain(&self) {
        loop {
            match self.process.read().await {
                Ok(chunk) if chunk.eof => break,
                Ok(chunk) => {
                    if self.feed(&chunk.bytes).is_err() {
                        self.process.terminate();
                        lock(&self.inner).status.phase = Phase::Failed;
                        break;
                    }
                }
                Err(_) => {
                    self.process.terminate();
                    lock(&self.inner).status.phase = Phase::Failed;
                    break;
                }
            }
        }
        let outcome = self.process.wait().await;
        let mut state = lock(&self.inner);
        if matches!(state.status.phase, Phase::Running) {
            state.status.phase = match outcome {
                Ok(outcome) => Phase::Exited {
                    exit_code: outcome.exit_code,
                    signal: outcome.signal,
                },
                Err(_) => Phase::Failed,
            };
        }
    }
    async fn closed(&self) -> Result<()> {
        let outcome = self.process.wait().await.map_err(native);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = self.changed.notified();
                if self.reader_done.load(Ordering::Acquire) {
                    break;
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| PtyError::Io("terminal screen reader did not settle".into()))?;
        outcome.map(|_| ())
    }
}
/// Ordinary provider of live terminal scopes over the native Process contract.
#[derive(Clone, Debug, Default)]
pub struct PtyFactory;
#[async_trait]
impl PluginFactory for PtyFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "PTY configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<PtyProcessContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let mut generation = [0; 16];
        getrandom::fill(&mut generation).map_err(|e| MetaError::Activation(e.to_string()))?;
        let shared = Arc::new(Shared {
            processes: plan.local::<PtyProcessContract>()?,
            generation: hex::encode(generation),
            next: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            creating: Creations::default(),
            scopes: Mutex::new(vec![]),
            terminals: Mutex::new(vec![]),
            slots: Arc::new(Semaphore::new(MAXIMUM_RETAINED_TERMINALS)),
            screens: Arc::new(Semaphore::new(SCREEN_BYTES)),
            snapshots: Arc::new(Semaphore::new(SNAPSHOT_BYTES)),
            queues: Arc::new(Semaphore::new(QUEUE_BYTES)),
        });
        let supply = plan
            .context()
            .provide_local::<PtyProviderContract>(Arc::new(Provider {
                shared: shared.clone(),
            }))?;
        plan.defer(
            "retire live terminal scopes",
            Box::new(move || {
                Box::pin(async move {
                    let result = shared.retire().await.map_err(|e| e.to_string());
                    drop(supply);
                    result
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests;
