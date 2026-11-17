//! One bounded worker for completed output and scope cleanup.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_spill::{
    CaptureSink, MAXIMUM_SPILL_BYTES, MAXIMUM_SPILL_READ_BYTES, Result, SpillCapture, SpillError,
    SpillLocator, SpillPage, SpillReader, SpillReaderContract, SpillRef, SpillScope, SpillStore,
    SpillStoreContract,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Component, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};

mod native;
mod worker;

const QUEUED_BYTES: usize = 8 * 1024 * 1024;
const COMMAND_CAPACITY: usize = 1024;
const READ_WAIT: Duration = Duration::from_secs(5);

/// Explicit local cache settings; product composition selects authority and paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpillLocalConfig {
    /// Private absolute writer partition.
    pub directory: PathBuf,
    /// Complete-object ceiling, at most 64 MiB.
    #[serde(default = "maximum_bytes")]
    pub maximum_object_bytes: u64,
    /// Best-effort free-space floor, including pending writes.
    #[serde(default = "minimum_free")]
    pub minimum_free_bytes: u64,
    /// Strict complete-object age limit; zero disables age expiry.
    #[serde(default = "cleanup_days")]
    pub cleanup_period_days: u32,
}
const fn maximum_bytes() -> u64 {
    MAXIMUM_SPILL_BYTES
}
const fn minimum_free() -> u64 {
    256 * 1024 * 1024
}
const fn cleanup_days() -> u32 {
    30
}
impl SpillLocalConfig {
    /// Selects the standard limits for an explicit private directory.
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            maximum_object_bytes: maximum_bytes(),
            minimum_free_bytes: minimum_free(),
            cleanup_period_days: cleanup_days(),
        }
    }
    fn validate(&self) -> Result<()> {
        if !self.directory.is_absolute()
            || self
                .directory
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
            || self.maximum_object_bytes == 0
            || self.maximum_object_bytes > MAXIMUM_SPILL_BYTES
        {
            return Err(SpillError::Invalid(
                "absolute directory and a 1..=64 MiB object bound are required".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct State {
    closed: AtomicBool,
    fenced: Mutex<BTreeSet<String>>,
    bytes: Arc<Semaphore>,
}
impl State {
    fn admits(&self, scope: &str) -> bool {
        !self.closed.load(Ordering::Acquire) && !self.fenced.lock().unwrap().contains(scope)
    }
}
#[derive(Debug)]
struct Shared {
    state: Arc<State>,
    sender: mpsc::SyncSender<Command>,
}
impl Shared {
    fn send(&self, command: Command) -> Result<()> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(SpillError::Closed);
        }
        self.sender.try_send(command).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => SpillError::Capacity,
            mpsc::TrySendError::Disconnected(_) => SpillError::Closed,
        })
    }
}

/// Exclusive native output cache owner. Escaped sinks do not extend write authority.
#[derive(Debug)]
pub struct LocalSpillStore {
    shared: Arc<Shared>,
    generation: SpillScope,
    stopped: watch::Receiver<bool>,
}
impl LocalSpillStore {
    /// Starts one native worker and waits for its private directory and lease.
    pub async fn open(config: SpillLocalConfig) -> Result<Arc<Self>> {
        config.validate()?;
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|e| SpillError::Unavailable(e.to_string()))?;
        let generation = SpillScope::new("generation", &format!("{entropy:02x?}"));
        let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let state = Arc::new(State {
            closed: AtomicBool::new(false),
            fenced: Mutex::default(),
            bytes: Arc::new(Semaphore::new(QUEUED_BYTES)),
        });
        let (ready, initialized) = oneshot::channel();
        let (completed, stopped) = watch::channel(false);
        let worker_state = state.clone();
        std::thread::Builder::new()
            .name("rsi-spill".into())
            .spawn(move || {
                match worker::Worker::open(config, worker_state) {
                    Ok(worker) => {
                        if ready.send(Ok(())).is_ok() {
                            worker.run(&receiver);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(io_error(error)));
                    }
                }
                drop(receiver);
                // Queued replies, the worker and its lease settle before acknowledgement.
                completed.send_replace(true);
            })
            .map_err(io_error)?;
        initialized
            .await
            .map_err(|_| SpillError::Unavailable("worker initialization was lost".into()))??;
        Ok(Arc::new(Self {
            shared: Arc::new(Shared { state, sender }),
            generation,
            stopped,
        }))
    }
    /// Closes admission and waits for the actual worker and native lease to settle.
    pub async fn shutdown(&self) {
        self.stop();
        let mut stopped = self.stopped.clone();
        let _ = stopped.wait_for(|done| *done).await;
    }
    fn stop(&self) {
        self.shared.state.closed.store(true, Ordering::Release);
        self.shared.state.bytes.close();
        let _ = self.shared.sender.try_send(Command::Stop);
    }
}
impl Drop for LocalSpillStore {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
struct ScopeSink {
    scope: SpillScope,
    shared: Arc<Shared>,
}
impl CaptureSink for ScopeSink {
    fn capture(&self) -> Option<Arc<dyn SpillCapture>> {
        if !self.shared.state.admits(self.scope.as_str()) {
            return None;
        }
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).ok()?;
        let locator = Arc::new(SpillLocator::new(&self.scope, entropy));
        let status = Arc::new(Mutex::new(CaptureStatus::Pending));
        self.shared
            .send(Command::Begin {
                locator: locator.clone(),
                status: status.clone(),
            })
            .ok()?;
        Some(Arc::new(Capture {
            shared: self.shared.clone(),
            locator,
            status,
        }))
    }
}

#[derive(Debug)]
enum CaptureStatus {
    Pending,
    Complete(SpillRef),
    Unavailable,
}
#[derive(Debug)]
struct Capture {
    shared: Arc<Shared>,
    locator: Arc<SpillLocator>,
    status: Arc<Mutex<CaptureStatus>>,
}
impl Capture {
    fn pending(&self) -> bool {
        self.shared.state.admits(self.locator.scope_name())
            && matches!(*self.status.lock().unwrap(), CaptureStatus::Pending)
    }
    fn queue(&self, bytes: &[u8], permit: OwnedSemaphorePermit) -> bool {
        if !self.pending() {
            return false;
        }
        if self
            .shared
            .send(Command::Write {
                locator: self.locator.clone(),
                bytes: bytes.to_vec(),
                _permit: permit,
            })
            .is_err()
        {
            self.abandon();
            return false;
        }
        true
    }
    fn chunk(&self, bytes: &[u8]) -> Option<u32> {
        if bytes.len() > MAXIMUM_SPILL_READ_BYTES || !self.pending() {
            self.abandon();
            return None;
        }
        Some(u32::try_from(bytes.len()).expect("admitted chunk is at most 64 KiB"))
    }
}
#[async_trait]
impl SpillCapture for Capture {
    fn push(&self, bytes: &[u8]) -> bool {
        let Some(length) = self.chunk(bytes) else {
            return false;
        };
        let Ok(permit) = self
            .shared
            .state
            .bytes
            .clone()
            .try_acquire_many_owned(length)
        else {
            self.abandon();
            return false;
        };
        self.queue(bytes, permit)
    }
    async fn write(&self, bytes: &[u8]) -> bool {
        let Some(length) = self.chunk(bytes) else {
            return false;
        };
        let Ok(permit) = self
            .shared
            .state
            .bytes
            .clone()
            .acquire_many_owned(length)
            .await
        else {
            return false;
        };
        self.queue(bytes, permit)
    }
    async fn finish(&self) -> Option<SpillRef> {
        if !self.pending() {
            return self.reference();
        }
        let (done, receiver) = oneshot::channel();
        if self
            .shared
            .send(Command::Finish {
                locator: self.locator.clone(),
                done,
            })
            .is_err()
            || !matches!(
                tokio::time::timeout(rsi_spill::PUBLICATION_WAIT, receiver).await,
                Ok(Ok(()))
            )
        {
            self.abandon();
        }
        self.reference()
    }
    fn reference(&self) -> Option<SpillRef> {
        match &*self.status.lock().unwrap() {
            CaptureStatus::Complete(reference) => Some(reference.clone()),
            _ => None,
        }
    }
    fn abandon(&self) {
        let mut status = self.status.lock().unwrap();
        if matches!(*status, CaptureStatus::Pending) {
            *status = CaptureStatus::Unavailable;
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.abandon();
    }
}

#[async_trait]
impl SpillReader for LocalSpillStore {
    async fn read(&self, locator: &SpillLocator, offset: u64, limit: usize) -> Result<SpillPage> {
        rsi_spill::validate_read_limit(limit)?;
        let (done, receiver) = oneshot::channel();
        self.shared.send(Command::Read {
            locator: locator.clone(),
            offset,
            limit,
            done,
        })?;
        tokio::time::timeout(READ_WAIT, receiver)
            .await
            .map_err(|_| SpillError::Unavailable("spill read timed out".into()))?
            .map_err(|_| SpillError::Closed)?
    }
}
#[async_trait]
impl SpillStore for LocalSpillStore {
    fn scope(&self, scope: SpillScope) -> Arc<dyn CaptureSink> {
        Arc::new(ScopeSink {
            scope,
            shared: self.shared.clone(),
        })
    }
    fn generation_scope(&self) -> SpillScope {
        self.generation.clone()
    }
    fn fence(&self, scope: &SpillScope) {
        self.shared
            .state
            .fenced
            .lock()
            .unwrap()
            .insert(scope.as_str().into());
    }
    async fn cleanup(&self, scope: &SpillScope) -> Result<()> {
        self.fence(scope);
        let (done, receiver) = oneshot::channel();
        self.shared.send(Command::Cleanup {
            scope: scope.clone(),
            remove: true,
            done,
        })?;
        receiver.await.map_err(|_| SpillError::Closed)??;
        Ok(())
    }
    async fn is_clean(&self, scope: &SpillScope) -> Result<bool> {
        let (done, receiver) = oneshot::channel();
        self.shared.send(Command::Cleanup {
            scope: scope.clone(),
            remove: false,
            done,
        })?;
        tokio::time::timeout(READ_WAIT, receiver)
            .await
            .map_err(|_| SpillError::Unavailable("spill status timed out".into()))?
            .map_err(|_| SpillError::Closed)?
    }
}

#[derive(Debug)]
enum Command {
    Stop,
    #[cfg(test)]
    Block {
        entered: oneshot::Sender<()>,
        release: mpsc::Receiver<()>,
    },
    Begin {
        locator: Arc<SpillLocator>,
        status: Arc<Mutex<CaptureStatus>>,
    },
    Write {
        locator: Arc<SpillLocator>,
        bytes: Vec<u8>,
        _permit: OwnedSemaphorePermit,
    },
    Finish {
        locator: Arc<SpillLocator>,
        done: oneshot::Sender<()>,
    },
    Read {
        locator: SpillLocator,
        offset: u64,
        limit: usize,
        done: oneshot::Sender<Result<SpillPage>>,
    },
    Cleanup {
        scope: SpillScope,
        remove: bool,
        done: oneshot::Sender<Result<bool>>,
    },
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "Consumes native I/O errors directly as a Result::map_err adapter."
)]
fn io_error(error: std::io::Error) -> SpillError {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        SpillError::Invalid(error.to_string())
    } else {
        SpillError::Unavailable(error.to_string())
    }
}

/// Ordinary provider; valid but unavailable storage does not remove Tool capability.
#[derive(Clone, Debug, Default)]
pub struct SpillLocalFactory;
#[async_trait]
impl PluginFactory for SpillLocalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: SpillLocalConfig = serde_json::from_value(desired.clone())
            .map_err(|e| MetaError::InvalidInput(e.to_string()))?;
        config
            .validate()
            .map_err(|e| MetaError::InvalidInput(e.to_string()))?;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            std::mem::size_of::<SpillLocalConfig>(),
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<SpillLocalConfig>()?;
        activate_local_store(plan, config).await
    }
}

/// Publishes one explicit native store and owns its retirement. Product adapters
/// may resolve their cache partition after acquiring a separate home authority.
pub async fn activate_local_store(
    plan: ActivationPlan,
    config: SpillLocalConfig,
) -> rsi_meta::Result<()> {
    let (store, live): (Arc<dyn SpillStore>, _) = match LocalSpillStore::open(config).await {
        Ok(store) => (store.clone(), Some(store)),
        Err(error) => (Arc::new(Unavailable(error)), None),
    };
    let reader: Arc<dyn SpillReader> = store.clone();
    let reader = plan
        .context()
        .provide_local::<SpillReaderContract>(reader)?;
    let store = plan.context().provide_local::<SpillStoreContract>(store)?;
    plan.defer(
        "close spill writer and reader",
        Box::new(move || {
            Box::pin(async move {
                if let Some(live) = live {
                    live.shutdown().await;
                }
                drop(store);
                drop(reader);
                Ok(())
            })
        }),
    )
}

#[derive(Debug)]
struct Unavailable(SpillError);
impl CaptureSink for Unavailable {
    fn capture(&self) -> Option<Arc<dyn SpillCapture>> {
        None
    }
}
#[async_trait]
impl SpillReader for Unavailable {
    async fn read(&self, _: &SpillLocator, _: u64, _: usize) -> Result<SpillPage> {
        Err(self.0.clone())
    }
}
#[async_trait]
impl SpillStore for Unavailable {
    fn scope(&self, _: SpillScope) -> Arc<dyn CaptureSink> {
        Arc::new(Self(self.0.clone()))
    }
    fn generation_scope(&self) -> SpillScope {
        SpillScope::new("unavailable", "")
    }
    fn fence(&self, _: &SpillScope) {}
    async fn cleanup(&self, _: &SpillScope) -> Result<()> {
        Err(self.0.clone())
    }
    async fn is_clean(&self, _: &SpillScope) -> Result<bool> {
        Err(self.0.clone())
    }
}

#[cfg(test)]
mod tests;
