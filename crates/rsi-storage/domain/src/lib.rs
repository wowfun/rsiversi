//! Bounded domain form above the exact-name storage hub.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, LocalContract, PluginFactory, PreparedActivation};
use rsi_storage::{
    KvBackend, MAXIMUM_STORAGE_DOMAIN_BYTES, MAXIMUM_STORAGE_RECORDS, StorageError, StorageHub,
    StorageHubContract, validate_identifier, validate_value,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Mutex as AsyncMutex;

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
    async fn snapshot(&self) -> BTreeMap<String, Value>;
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
    domains: Mutex<HashMap<String, Weak<DomainState>>>,
}

#[derive(Debug)]
struct DomainState {
    spec: DomainSpec,
    backend: Arc<dyn KvBackend>,
    records: Arc<AsyncMutex<DomainRecords>>,
}

#[derive(Debug)]
struct DomainRecords {
    values: BTreeMap<String, Value>,
    encoded_bytes: usize,
}

#[async_trait]
impl DomainFacility for Facility {
    async fn open(&self, spec: DomainSpec) -> Result<Arc<dyn Domain>, StorageError> {
        spec.validate()?;
        if let Some(existing) = self
            .domains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&spec.id)
            .and_then(Weak::upgrade)
        {
            if existing.spec != spec {
                return Err(StorageError::InvalidInput(format!(
                    "domain `{}` is already open with a different specification",
                    spec.id
                )));
            }
            let domain: Arc<dyn Domain> = existing;
            return Ok(domain);
        }

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
            for (key, value) in &loaded.records {
                validate_identifier("record key", key)?;
                validate_value(value)?;
            }
            let encoded_bytes = encoded_records_bytes(&loaded.records).map_err(|_| {
                StorageError::Corrupt(format!(
                    "domain `{}` has an invalid aggregate byte size",
                    spec.id
                ))
            })?;
            if encoded_bytes > spec.maximum_bytes {
                return Err(StorageError::Corrupt(format!(
                    "domain `{}` exceeds its aggregate byte bound",
                    spec.id
                )));
            }
            DomainRecords {
                values: loaded.records,
                encoded_bytes,
            }
        } else {
            DomainRecords {
                values: BTreeMap::new(),
                encoded_bytes: 2,
            }
        };
        let state = Arc::new(DomainState {
            spec: spec.clone(),
            backend,
            records: Arc::new(AsyncMutex::new(records)),
        });
        let mut domains = self
            .domains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = domains.get(&spec.id).and_then(Weak::upgrade) {
            if existing.spec != spec {
                return Err(StorageError::InvalidInput(format!(
                    "domain `{}` concurrently opened with a different specification",
                    spec.id
                )));
            }
            let domain: Arc<dyn Domain> = existing;
            return Ok(domain);
        }
        domains.insert(spec.id.clone(), Arc::downgrade(&state));
        let domain: Arc<dyn Domain> = state;
        Ok(domain)
    }
}

#[async_trait]
impl Domain for DomainState {
    fn spec(&self) -> &DomainSpec {
        &self.spec
    }

    async fn snapshot(&self) -> BTreeMap<String, Value> {
        self.records.lock().await.values.clone()
    }

    async fn put(&self, key: &str, value: Value) -> Result<(), StorageError> {
        validate_identifier("record key", key)?;
        validate_value(&value)?;
        let mut records = Arc::clone(&self.records).lock_owned().await;
        if !records.values.contains_key(key) && records.values.len() == self.spec.maximum_records {
            return Err(StorageError::InvalidInput(format!(
                "domain `{}` reached its record bound",
                self.spec.id
            )));
        }
        let new_entry_bytes = encoded_entry_bytes(key, &value)?;
        let projected_bytes = if let Some(previous) = records.values.get(key) {
            records
                .encoded_bytes
                .checked_sub(encoded_entry_bytes(key, previous)?)
                .and_then(|bytes| bytes.checked_add(new_entry_bytes))
        } else {
            records
                .encoded_bytes
                .checked_add(usize::from(!records.values.is_empty()))
                .and_then(|bytes| bytes.checked_add(new_entry_bytes))
        }
        .ok_or_else(|| StorageError::InvalidInput("domain byte count overflowed".into()))?;
        if projected_bytes > self.spec.maximum_bytes {
            return Err(StorageError::InvalidInput(format!(
                "domain `{}` reached its aggregate byte bound",
                self.spec.id
            )));
        }
        let backend = Arc::clone(&self.backend);
        let domain = self.spec.id.clone();
        let version = self.spec.version;
        let key = key.to_owned();
        tokio::spawn(async move {
            backend.put(&domain, version, &key, &value).await?;
            records.values.insert(key, value);
            records.encoded_bytes = projected_bytes;
            Ok(())
        })
        .await
        .map_err(|error| StorageError::Io(format!("domain commit task failed: {error}")))?
    }

    async fn delete(&self, key: &str) -> Result<bool, StorageError> {
        validate_identifier("record key", key)?;
        let mut records = Arc::clone(&self.records).lock_owned().await;
        let Some(previous) = records.values.get(key) else {
            return Ok(false);
        };
        let entry_bytes = encoded_entry_bytes(key, previous)?;
        let projected_bytes = if records.values.len() == 1 {
            2
        } else {
            records
                .encoded_bytes
                .checked_sub(entry_bytes)
                .and_then(|bytes| bytes.checked_sub(1))
                .ok_or_else(|| StorageError::InvalidInput("domain byte count underflowed".into()))?
        };
        let backend = Arc::clone(&self.backend);
        let domain = self.spec.id.clone();
        let version = self.spec.version;
        let key = key.to_owned();
        tokio::spawn(async move {
            backend.delete(&domain, version, &key).await?;
            records.values.remove(&key);
            records.encoded_bytes = projected_bytes;
            Ok(true)
        })
        .await
        .map_err(|error| StorageError::Io(format!("domain commit task failed: {error}")))?
    }
}

fn encoded_records_bytes(records: &BTreeMap<String, Value>) -> Result<usize, StorageError> {
    let mut bytes = 2_usize;
    for (index, (key, value)) in records.iter().enumerate() {
        let entry_bytes = encoded_entry_bytes(key, value)?;
        bytes = bytes
            .checked_add(usize::from(index > 0))
            .and_then(|bytes| bytes.checked_add(entry_bytes))
            .ok_or_else(|| StorageError::InvalidInput("domain byte count overflowed".into()))?;
    }
    Ok(bytes)
}

fn encoded_entry_bytes(key: &str, value: &Value) -> Result<usize, StorageError> {
    let key_bytes = serde_json::to_vec(key)
        .map_err(|error| StorageError::InvalidInput(error.to_string()))?
        .len();
    let value_bytes = serde_json::to_vec(value)
        .map_err(|error| StorageError::InvalidInput(error.to_string()))?
        .len();
    key_bytes
        .checked_add(1)
        .and_then(|bytes| bytes.checked_add(value_bytes))
        .ok_or_else(|| StorageError::InvalidInput("domain byte count overflowed".into()))
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
            domains: Mutex::new(HashMap::new()),
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
