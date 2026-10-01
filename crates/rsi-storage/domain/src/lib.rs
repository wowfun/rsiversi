//! Bounded domain form above the exact-name storage hub.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, LocalContract, PluginFactory, PreparedActivation};
pub use rsi_storage::StorageError;
use rsi_storage::{
    KvBackend, MAXIMUM_STORAGE_DOMAIN_BYTES, MAXIMUM_STORAGE_RECORDS, StorageHub,
    StorageHubContract, validate_identifier, validate_value,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, OwnedMutexGuard};

mod size;
pub use size::{RecordObjectSize, encoded_entry_bytes};

/// Immutable declaration for one domain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainSpec {
    /// Exact domain identity.
    pub id: String,
    /// Exact registered backend name.
    pub backend: String,
    /// Consumer-owned schema version.
    pub version: u32,
    /// Maximum retained records.
    pub maximum_records: usize,
    /// Maximum compact JSON bytes in the complete record object.
    pub maximum_bytes: usize,
}

impl DomainSpec {
    fn validate(&self) -> Result<(), StorageError> {
        validate_identifier("domain", &self.id)?;
        validate_identifier("backend", &self.backend)?;
        if self.version == 0 {
            return Err(StorageError::InvalidInput(
                "domain version must be nonzero".into(),
            ));
        }
        if self.maximum_records == 0 || self.maximum_records > MAXIMUM_STORAGE_RECORDS {
            return Err(StorageError::InvalidInput(format!(
                "maximum_records must be within 1..={MAXIMUM_STORAGE_RECORDS}"
            )));
        }
        if self.maximum_bytes < 2 || self.maximum_bytes > MAXIMUM_STORAGE_DOMAIN_BYTES {
            return Err(StorageError::InvalidInput(format!(
                "maximum_bytes must be within 2..={MAXIMUM_STORAGE_DOMAIN_BYTES}"
            )));
        }
        Ok(())
    }
}

/// Open authoritative view of one JSON record domain.
#[async_trait]
pub trait Domain: fmt::Debug + Send + Sync + 'static {
    /// Returns the immutable declaration used to open this domain.
    fn spec(&self) -> &DomainSpec;
    /// Returns the current committed record snapshot.
    async fn snapshot(&self) -> Result<BTreeMap<String, Value>, StorageError>;
    /// Checks the health of the retained backend generation without I/O.
    fn ensure_available(&self) -> Result<(), StorageError>;
    /// Durably publishes one complete JSON value.
    async fn put(&self, key: &str, value: Value) -> Result<(), StorageError>;
    /// Durably deletes one value and reports whether it existed.
    async fn delete(&self, key: &str) -> Result<bool, StorageError>;
}

/// Facility that opens exact routed domains.
#[async_trait]
pub trait DomainFacility: fmt::Debug + Send + Sync + 'static {
    /// Opens or reuses one domain with the exact same specification.
    async fn open(&self, spec: DomainSpec) -> Result<Arc<dyn Domain>, StorageError>;
}

/// Nominal Local contract for [`DomainFacility`].
#[derive(Debug)]
pub struct DomainFacilityContract;

impl LocalContract for DomainFacilityContract {
    const KEY: &'static str = "rsi.storage.domain";
    type Service = dyn DomainFacility;
}

#[derive(Debug)]
struct Facility {
    hub: Arc<dyn StorageHub>,
    registry: Arc<Registry>,
}

#[derive(Debug, Default)]
struct Registry {
    domains: Mutex<HashMap<String, Weak<Authority>>>,
}

#[derive(Debug)]
struct Authority {
    spec: DomainSpec,
    loaded: OnceCell<LoadedDomain>,
    registry: Weak<Registry>,
    recovery_required: AtomicBool,
}

impl Drop for Authority {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut domains = registry
                .domains
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if domains
                .get(&self.spec.id)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                domains.remove(&self.spec.id);
            }
        }
    }
}

#[derive(Debug)]
struct LoadedDomain {
    backend: Arc<dyn KvBackend>,
    records: Arc<AsyncMutex<DomainRecords>>,
}

#[derive(Debug)]
struct DomainHandle {
    authority: Arc<Authority>,
}

impl DomainHandle {
    fn loaded(&self) -> &LoadedDomain {
        self.authority
            .loaded
            .get()
            .expect("initialized domain authority")
    }

    async fn commit<T: Send + 'static, W: Send + 'static>(
        &self,
        records: OwnedMutexGuard<DomainRecords>,
        write: impl Future<Output = Result<W, StorageError>> + Send + 'static,
        publish: impl FnOnce(&mut DomainRecords, W) -> T + Send + 'static,
    ) -> Result<T, StorageError> {
        // The guard exists before spawning, including if an unpolled task is dropped.
        let commit = Commit {
            authority: Arc::clone(&self.authority),
            records,
            finished: false,
        };
        tokio::spawn(async move {
            let mut commit = commit;
            match write.await {
                Ok(written) => {
                    let result = publish(&mut commit.records, written);
                    commit.finished = true;
                    Ok(result)
                }
                Err(error) => {
                    commit.finished = !error.requires_recovery();
                    Err(error)
                }
            }
        })
        .await
        .map_err(|error| {
            StorageError::OutcomeUnknown(format!("domain commit task failed: {error}"))
        })?
    }
}

struct Commit {
    authority: Arc<Authority>,
    records: OwnedMutexGuard<DomainRecords>,
    finished: bool,
}
impl Drop for Commit {
    fn drop(&mut self) {
        if !self.finished {
            // Fence before the records guard releases queued readers and writers.
            self.authority
                .recovery_required
                .store(true, Ordering::Release);
        }
    }
}

#[derive(Debug)]
struct DomainRecords {
    values: BTreeMap<String, Record>,
    size: RecordObjectSize,
}

#[derive(Debug)]
struct Record {
    value: Value,
    encoded_bytes: usize,
}

impl DomainRecords {
    fn from_values(values: BTreeMap<String, Value>) -> Result<Self, StorageError> {
        let mut records = Self {
            values: BTreeMap::new(),
            size: RecordObjectSize::default(),
        };
        for (key, value) in values {
            validate_identifier("record key", &key)?;
            let encoded_bytes = encoded_entry_bytes(&key, validate_value(&value)?)?;
            records.size = records.size.with_entry(None, encoded_bytes)?;
            records.values.insert(
                key,
                Record {
                    value,
                    encoded_bytes,
                },
            );
        }
        Ok(records)
    }
}

#[async_trait]
impl DomainFacility for Facility {
    async fn open(&self, spec: DomainSpec) -> Result<Arc<dyn Domain>, StorageError> {
        spec.validate()?;
        let authority = {
            let mut domains = self
                .registry
                .domains
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = domains.get(&spec.id).and_then(Weak::upgrade) {
                existing
            } else {
                let authority = Arc::new(Authority {
                    spec: spec.clone(),
                    loaded: OnceCell::new(),
                    registry: Arc::downgrade(&self.registry),
                    recovery_required: AtomicBool::new(false),
                });
                domains.insert(spec.id.clone(), Arc::downgrade(&authority));
                authority
            }
        };
        // No strong authority is dropped while holding the registry mutex.
        if authority.spec != spec {
            return Err(StorageError::InvalidInput(format!(
                "domain `{}` is already open with a different specification",
                spec.id
            )));
        }
        authority
            .loaded
            .get_or_try_init(|| async {
                let backend = self.hub.resolve(&spec.backend)?;
                let loaded = backend.load(&spec.id).await?;
                let records = if let Some(loaded) = loaded {
                    if loaded.version != spec.version {
                        return Err(StorageError::Corrupt(format!(
                            "domain `{}` has version {}, expected {}",
                            spec.id, loaded.version, spec.version
                        )));
                    }
                    if loaded.records.len() > spec.maximum_records {
                        return Err(StorageError::Corrupt(format!(
                            "domain `{}` exceeds its record bound",
                            spec.id
                        )));
                    }
                    let records = DomainRecords::from_values(loaded.records)
                        .map_err(|error| StorageError::Corrupt(error.to_string()))?;
                    if records.size.bytes() > spec.maximum_bytes {
                        return Err(StorageError::Corrupt(format!(
                            "domain `{}` exceeds its aggregate byte bound",
                            spec.id
                        )));
                    }
                    records
                } else {
                    DomainRecords {
                        values: BTreeMap::new(),
                        size: RecordObjectSize::default(),
                    }
                };
                Ok::<_, StorageError>(LoadedDomain {
                    backend,
                    records: Arc::new(AsyncMutex::new(records)),
                })
            })
            .await?;
        let domain = DomainHandle { authority };
        domain.ensure_available()?;
        Ok(Arc::new(domain))
    }
}

#[async_trait]
impl Domain for DomainHandle {
    fn spec(&self) -> &DomainSpec {
        &self.authority.spec
    }

    fn ensure_available(&self) -> Result<(), StorageError> {
        if self.authority.recovery_required.load(Ordering::Acquire) {
            return Err(StorageError::RecoveryRequired);
        }
        self.loaded().backend.ensure_available()
    }

    async fn snapshot(&self) -> Result<BTreeMap<String, Value>, StorageError> {
        self.ensure_available()?;
        let records = self.loaded().records.lock().await;
        self.ensure_available()?;
        Ok(records
            .values
            .iter()
            .map(|(key, record)| (key.clone(), record.value.clone()))
            .collect())
    }

    async fn put(&self, key: &str, value: Value) -> Result<(), StorageError> {
        self.ensure_available()?;
        validate_identifier("record key", key)?;
        let value_bytes = validate_value(&value)?;
        let records = Arc::clone(&self.loaded().records).lock_owned().await;
        self.ensure_available()?;
        let previous = records.values.get(key);
        if previous.is_none() && records.values.len() == self.authority.spec.maximum_records {
            return Err(StorageError::InvalidInput(format!(
                "domain `{}` reached its record bound",
                self.authority.spec.id
            )));
        }
        let new_entry_bytes = encoded_entry_bytes(key, value_bytes)?;
        let projected = records
            .size
            .with_entry(previous.map(|record| record.encoded_bytes), new_entry_bytes)?;
        if projected.bytes() > self.authority.spec.maximum_bytes {
            return Err(StorageError::InvalidInput(format!(
                "domain `{}` reached its aggregate byte bound",
                self.authority.spec.id
            )));
        }
        let backend = Arc::clone(&self.loaded().backend);
        let domain = self.authority.spec.id.clone();
        let version = self.authority.spec.version;
        let key = key.to_owned();
        self.commit(
            records,
            async move {
                backend.put(&domain, version, &key, &value).await?;
                Ok((key, value))
            },
            move |records, (key, value)| {
                records.values.insert(
                    key,
                    Record {
                        value,
                        encoded_bytes: new_entry_bytes,
                    },
                );
                records.size = projected;
            },
        )
        .await
    }

    async fn delete(&self, key: &str) -> Result<bool, StorageError> {
        self.ensure_available()?;
        validate_identifier("record key", key)?;
        let records = Arc::clone(&self.loaded().records).lock_owned().await;
        self.ensure_available()?;
        let Some(previous) = records.values.get(key) else {
            return Ok(false);
        };
        let projected = records.size.without_entry(previous.encoded_bytes)?;
        let backend = Arc::clone(&self.loaded().backend);
        let domain = self.authority.spec.id.clone();
        let version = self.authority.spec.version;
        let key = key.to_owned();
        self.commit(
            records,
            async move {
                backend.delete(&domain, version, &key).await?;
                Ok(key)
            },
            move |records, key| {
                records.values.remove(&key);
                records.size = projected;
                true
            },
        )
        .await
    }
}

/// Ordinary plugin factory for the domain facility.
#[derive(Clone, Debug, Default)]
pub struct DomainFactory;

#[async_trait]
impl PluginFactory for DomainFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(rsi_meta::MetaError::InvalidInput(
                "storage domain configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<StorageHubContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let facility: Arc<dyn DomainFacility> = Arc::new(Facility {
            hub: plan.local::<StorageHubContract>()?,
            registry: Arc::new(Registry::default()),
        });
        let supply = plan
            .context()
            .provide_local::<DomainFacilityContract>(facility)?;
        plan.defer(
            "withdraw storage domain facility",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests;

/// Projects storage failures into the shared API taxonomy without losing commit certainty.
pub fn storage_error(error: StorageError) -> rsi_api_protocol::ApiError {
    use rsi_api_protocol::ApiError;
    match error {
        StorageError::OutcomeUnknown(_) => ApiError::OutcomeUnknown,
        StorageError::Io(_)
        | StorageError::RecoveryRequired
        | StorageError::BackendUnavailable(_) => ApiError::Unavailable,
        error @ (StorageError::InvalidInput(_)
        | StorageError::DuplicateBackend(_)
        | StorageError::Corrupt(_)) => ApiError::Backend(error.to_string()),
    }
}
