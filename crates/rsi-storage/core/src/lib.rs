//! Process-local hub for non-session storage backends.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use thiserror::Error;

/// Maximum UTF-8 bytes in a backend, domain, or record identifier.
pub const MAXIMUM_STORAGE_IDENTIFIER_BYTES: usize = 256;
/// Maximum records returned by one domain load.
pub const MAXIMUM_STORAGE_RECORDS: usize = 65_536;
/// Maximum encoded JSON bytes in one stored value.
pub const MAXIMUM_STORAGE_VALUE_BYTES: usize = 16 * 1024 * 1024;
/// Absolute maximum compact JSON bytes retained by one open domain.
pub const MAXIMUM_STORAGE_DOMAIN_BYTES: usize = 256 * 1024 * 1024;

/// Failure returned by the non-session storage contracts.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StorageError {
    /// A caller supplied malformed or out-of-bounds input.
    #[error("invalid storage input: {0}")]
    InvalidInput(String),
    /// An exact backend name is already registered.
    #[error("storage backend `{0}` is already registered")]
    DuplicateBackend(String),
    /// No active backend has the requested exact name.
    #[error("storage backend `{0}` is unavailable")]
    BackendUnavailable(String),
    /// Durable state is corrupt or has an incompatible schema version.
    #[error("storage corruption: {0}")]
    Corrupt(String),
    /// The durable medium rejected an operation.
    #[error("storage I/O failed: {0}")]
    Io(String),
}

/// Result returned by storage services.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Complete bounded state loaded for one domain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDomain {
    /// Exact consumer-owned schema version.
    pub version: u32,
    /// Ordered record map.
    pub records: BTreeMap<String, Value>,
}

/// Durable JSON KV backend implemented by one ordinary backend plugin.
#[async_trait]
pub trait KvBackend: fmt::Debug + Send + Sync + 'static {
    /// Loads one complete domain, returning `None` when it has never existed.
    async fn load(&self, domain: &str) -> Result<Option<StoredDomain>>;

    /// Atomically publishes one record under the exact domain schema version.
    async fn put(&self, domain: &str, version: u32, key: &str, value: &Value) -> Result<()>;

    /// Atomically deletes one record under the exact domain schema version.
    async fn delete(&self, domain: &str, version: u32, key: &str) -> Result<()>;
}

/// Process-local exact-name backend registry.
pub trait StorageHub: fmt::Debug + Send + Sync + 'static {
    /// Registers one backend until the returned lease is dropped.
    fn register(&self, name: &str, backend: Arc<dyn KvBackend>) -> Result<BackendLease>;

    /// Resolves the currently active backend with this exact name.
    fn resolve(&self, name: &str) -> Result<Arc<dyn KvBackend>>;
}

/// Nominal Local contract for [`StorageHub`].
#[derive(Debug)]
pub struct StorageHubContract;

impl LocalContract for StorageHubContract {
    const KEY: &'static str = "rsi.storage.hub";
    type Service = dyn StorageHub;
}

/// Generation-owned backend registration.
pub struct BackendLease {
    name: String,
    registration: u64,
    hub: Weak<HubState>,
}

impl fmt::Debug for BackendLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendLease")
            .field("name", &self.name)
            .field("registration", &self.registration)
            .finish_non_exhaustive()
    }
}

impl Drop for BackendLease {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let mut state = hub
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .backends
            .get(&self.name)
            .is_some_and(|entry| entry.registration == self.registration)
        {
            state.backends.remove(&self.name);
        }
    }
}

#[derive(Debug)]
struct Hub {
    state: Arc<HubState>,
}

#[derive(Debug)]
struct HubState {
    inner: Mutex<HubInner>,
}

#[derive(Debug, Default)]
struct HubInner {
    next_registration: u64,
    backends: HashMap<String, BackendEntry>,
}

#[derive(Debug)]
struct BackendEntry {
    registration: u64,
    backend: Arc<dyn KvBackend>,
}

impl Hub {
    fn new() -> Self {
        Self {
            state: Arc::new(HubState {
                inner: Mutex::new(HubInner::default()),
            }),
        }
    }
}

impl StorageHub for Hub {
    fn register(&self, name: &str, backend: Arc<dyn KvBackend>) -> Result<BackendLease> {
        validate_identifier("backend", name)?;
        let mut state = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.backends.contains_key(name) {
            return Err(StorageError::DuplicateBackend(name.to_owned()));
        }
        state.next_registration = state
            .next_registration
            .checked_add(1)
            .ok_or_else(|| StorageError::Io("backend registration identity exhausted".into()))?;
        let registration = state.next_registration;
        state.backends.insert(
            name.to_owned(),
            BackendEntry {
                registration,
                backend,
            },
        );
        Ok(BackendLease {
            name: name.to_owned(),
            registration,
            hub: Arc::downgrade(&self.state),
        })
    }

    fn resolve(&self, name: &str) -> Result<Arc<dyn KvBackend>> {
        validate_identifier("backend", name)?;
        self.state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .backends
            .get(name)
            .map(|entry| Arc::clone(&entry.backend))
            .ok_or_else(|| StorageError::BackendUnavailable(name.to_owned()))
    }
}

/// Ordinary plugin factory that owns one backend hub generation.
#[derive(Clone, Debug, Default)]
pub struct StorageFactory;

#[async_trait]
impl PluginFactory for StorageFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        require_empty_config(desired, "storage hub")?;
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let hub: Arc<dyn StorageHub> = Arc::new(Hub::new());
        let supply = plan.context().provide_local::<StorageHubContract>(hub)?;
        plan.defer(
            "withdraw storage hub",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

/// Validates a bounded exact storage identifier.
pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_STORAGE_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StorageError::InvalidInput(format!(
            "{kind} must be a nonempty bounded ASCII identifier"
        )));
    }
    Ok(())
}

/// Validates one JSON value at the shared backend bound.
pub fn validate_value(value: &Value) -> Result<usize> {
    let encoded =
        serde_json::to_vec(value).map_err(|error| StorageError::InvalidInput(error.to_string()))?;
    if encoded.len() > MAXIMUM_STORAGE_VALUE_BYTES {
        return Err(StorageError::InvalidInput(format!(
            "storage value exceeds {MAXIMUM_STORAGE_VALUE_BYTES} bytes"
        )));
    }
    Ok(encoded.len())
}

fn require_empty_config(desired: &ConfigValue, owner: &str) -> rsi_meta::Result<()> {
    if desired.is_null() || desired.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(MetaError::InvalidInput(format!(
            "{owner} configuration must be null or empty"
        )))
    }
}
