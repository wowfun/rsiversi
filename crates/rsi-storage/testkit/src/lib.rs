//! Deterministic backend gates for tests of production typed Domains.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

use async_trait::async_trait;
use rsi_storage::{
    Address, Capabilities, EncodedValue, KvBackend, KvUnit, Layout, Mutation, OpenedUnit, Result,
    StorageError, UnitDescriptor, UnitLease, UnitNames, UnitSnapshot, admit_mutations,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

#[derive(Debug)]
/// In-memory backend with explicit fault injection and unit leases.
pub struct MemoryBackend {
    /// Shared durable fixture and fault controls.
    pub state: Arc<MemoryState>,
    names: Arc<UnitNames>,
}
impl Default for MemoryBackend {
    fn default() -> Self {
        Self {
            state: Arc::new(MemoryState {
                atomic: AtomicBool::new(true),
                ..MemoryState::default()
            }),
            names: Arc::default(),
        }
    }
}

#[derive(Debug, Default)]
/// Observable fixture state. Tests must release any gate they arm.
pub struct MemoryState {
    /// Raw durable snapshots, also used to seed malformed business records.
    pub units: Mutex<BTreeMap<String, UnitSnapshot>>,
    /// Number of attempted opens.
    pub loads: AtomicUsize,
    /// Number of commits that changed at least one address.
    pub writes: AtomicUsize,
    /// Exact encoded changes in the latest committed write, for write-amplification assertions.
    pub last_write: Mutex<Vec<Mutation>>,
    /// Pause the next open after taking its durable snapshot.
    pub pause_load: AtomicBool,
    /// Reject the next open before reading its snapshot.
    pub reject_load: AtomicBool,
    /// Pause the next write before commit.
    pub pause_write: AtomicBool,
    /// Reject writes before commit while this flag is set.
    pub reject_write: AtomicBool,
    /// Pause the next write after durable mutation but before return.
    pub pause_after_write: AtomicBool,
    /// Panic after the next commit to exercise Domain worker fencing.
    pub panic_after_write: AtomicBool,
    /// Return `OutcomeUnknown` and fence this backend after the next commit.
    pub unknown_after_write: AtomicBool,
    /// Fence reads and writes in every unit of this backend.
    pub unhealthy: AtomicBool,
    /// Whether the backend advertises arbitrary atomic batches.
    pub atomic: AtomicBool,
    /// Notification when an armed load or write gate has been reached.
    pub entered: Notify,
    /// Release one paused operation.
    pub release: Notify,
}

impl MemoryBackend {
    /// Inserts raw durable bytes without requiring a business codec.
    pub fn seed(&self, name: &str, version: u32, address: Address, value: &Value) {
        let mut units = self.state.units.lock().unwrap();
        let unit = units.entry(name.into()).or_default();
        unit.version = Some(version);
        unit.records
            .insert(address, EncodedValue::encode(value).unwrap());
    }
    /// Decodes fixture values, indexed by the canonical accounting address.
    pub fn values(&self, name: &str) -> BTreeMap<String, Value> {
        self.state
            .units
            .lock()
            .unwrap()
            .get(name)
            .map_or_else(BTreeMap::new, |unit| {
                unit.records
                    .iter()
                    .map(|(address, value)| (address.accounting_key(), value.decode().unwrap()))
                    .collect()
            })
    }
}

fn health(state: &MemoryState) -> Result<()> {
    if state.unhealthy.load(Ordering::Acquire) {
        Err(StorageError::RecoveryRequired)
    } else {
        Ok(())
    }
}
fn capabilities(state: &MemoryState) -> Capabilities {
    Capabilities {
        atomic_batch: state.atomic.load(Ordering::Acquire),
        backup_record: false,
    }
}

#[async_trait]
impl KvBackend for MemoryBackend {
    fn ensure_available(&self) -> Result<()> {
        health(&self.state)
    }
    fn capabilities(&self, _: Layout) -> Capabilities {
        capabilities(&self.state)
    }
    async fn open(&self, descriptor: UnitDescriptor) -> Result<OpenedUnit> {
        self.ensure_available()?;
        let lease = self.names.reserve(descriptor.name())?;
        self.state.loads.fetch_add(1, Ordering::SeqCst);
        if self.state.reject_load.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Io("injected load rejection".into()));
        }
        let snapshot = self
            .state
            .units
            .lock()
            .unwrap()
            .get(descriptor.name())
            .cloned()
            .unwrap_or_default();
        if self.state.pause_load.swap(false, Ordering::SeqCst) {
            self.state.entered.notify_one();
            self.state.release.notified().await;
        }
        if let Some(version) = snapshot.version {
            descriptor.admit_version(version)?;
        }
        Ok(OpenedUnit {
            snapshot,
            unit: Arc::new(Unit {
                state: Arc::clone(&self.state),
                descriptor,
                _lease: lease,
            }),
        })
    }
}

#[derive(Debug)]
struct Unit {
    state: Arc<MemoryState>,
    descriptor: UnitDescriptor,
    _lease: UnitLease,
}

#[async_trait]
impl KvUnit for Unit {
    fn descriptor(&self) -> &UnitDescriptor {
        &self.descriptor
    }
    fn ensure_available(&self) -> Result<()> {
        health(&self.state)
    }
    async fn write(&self, changes: Vec<Mutation>) -> Result<()> {
        self.ensure_available()?;
        admit_mutations(&self.descriptor, &changes, capabilities(&self.state))?;
        if self.state.pause_write.swap(false, Ordering::SeqCst) {
            self.state.entered.notify_one();
            self.state.release.notified().await;
        }
        if self.state.reject_write.load(Ordering::SeqCst) {
            return Err(StorageError::Io("injected pre-commit rejection".into()));
        }
        {
            let mut units = self.state.units.lock().unwrap();
            let unit = units.entry(self.descriptor.name().into()).or_default();
            if let Some(version) = unit.version {
                self.descriptor.admit_version(version)?;
            }
            let mut modified = false;
            self.state.last_write.lock().unwrap().clone_from(&changes);
            for change in changes {
                if let Some(value) = change.value {
                    unit.records.insert(change.address, value);
                    modified = true;
                } else {
                    modified |= unit.records.remove(&change.address).is_some();
                }
            }
            if modified {
                unit.version = Some(self.descriptor.version());
                self.state.writes.fetch_add(1, Ordering::SeqCst);
            }
        }
        if self.state.pause_after_write.swap(false, Ordering::SeqCst) {
            self.state.entered.notify_one();
            self.state.release.notified().await;
        }
        assert!(
            !self.state.panic_after_write.swap(false, Ordering::SeqCst),
            "injected post-commit panic"
        );
        if self.state.unknown_after_write.swap(false, Ordering::SeqCst) {
            self.state.unhealthy.store(true, Ordering::Release);
            return Err(StorageError::OutcomeUnknown(
                "injected lost commit acknowledgement".into(),
            ));
        }
        Ok(())
    }
}

/// Registers an explicitly supplied backend in an ordinary test runtime.
#[derive(Debug, Clone)]
pub struct BackendFactory {
    name: String,
    backend: Arc<dyn KvBackend>,
}
impl BackendFactory {
    /// Chooses the registration name and shared fixture backend.
    pub fn new(name: impl Into<String>, backend: Arc<dyn KvBackend>) -> Self {
        Self {
            name: name.into(),
            backend,
        }
    }
}
#[async_trait]
impl rsi_meta::PluginFactory for BackendFactory {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(config.clone())
            .requiring_local::<rsi_storage::StorageHubContract>())
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<rsi_storage::StorageHubContract>()?
            .register(&self.name, self.backend.clone())
            .map_err(|e| rsi_meta::MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw test storage backend",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}
