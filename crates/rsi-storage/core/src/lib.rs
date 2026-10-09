//! Process-local hub for non-session storage backends.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(test)]
mod retirement_tests;

mod directories;
pub use directories::create_private_directories;
mod size;
pub use size::{RecordObjectSize, encoded_entry_bytes};

mod operations;
pub use operations::{BackendOperations, BackendRegistration};

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
/// Maximum JSON depth, counting the root value as one.
pub const MAXIMUM_STORAGE_VALUE_DEPTH: usize = 64;
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
    /// Commit may have become visible; its owning generation is now fenced.
    #[error("storage commit outcome unknown: {0}")]
    OutcomeUnknown(String),
    /// The backend must be reopened with fresh consumers before further use.
    #[error("storage recovery required")]
    RecoveryRequired,
    /// The durable medium rejected an operation before commit.
    #[error("storage I/O failed: {0}")]
    Io(String),
}

impl StorageError {
    /// Whether this failure requires permanent isolation of the owning generation.
    pub const fn requires_recovery(&self) -> bool {
        match self {
            Self::OutcomeUnknown(_) | Self::RecoveryRequired => true,
            Self::InvalidInput(_)
            | Self::DuplicateBackend(_)
            | Self::BackendUnavailable(_)
            | Self::Corrupt(_)
            | Self::Io(_) => false,
        }
    }
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
    /// Checks local generation health without I/O.
    fn ensure_available(&self) -> Result<()>;

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
        let removed = {
            let mut state = hub
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .backends
                .get(&self.name)
                .is_some_and(|entry| entry.registration == self.registration)
            {
                state.backends.remove(&self.name)
            } else {
                None
            }
        };
        drop(removed);
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
    validate_depth(value)?;
    let mut writer = BoundedWriter::new(std::io::sink(), MAXIMUM_STORAGE_VALUE_BYTES);
    serde_json::to_writer(&mut writer, value)
        .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
    Ok(writer.written)
}

fn validate_depth(value: &Value) -> Result<()> {
    if !value.is_array() && !value.is_object() {
        return Ok(());
    }
    // Iterator frames retain only the current path, not every sibling.
    let mut frames = vec![ValueChildren::Root(Some(value))];
    while let Some(frame) = frames.last_mut() {
        let Some(value) = frame.next() else {
            frames.pop();
            continue;
        };
        if frames.len() > MAXIMUM_STORAGE_VALUE_DEPTH {
            return Err(StorageError::InvalidInput(format!(
                "storage value exceeds depth {MAXIMUM_STORAGE_VALUE_DEPTH}"
            )));
        }
        match value {
            Value::Array(values) => frames.push(ValueChildren::Array(values.iter())),
            Value::Object(values) => frames.push(ValueChildren::Object(values.values())),
            _ => {}
        }
    }
    Ok(())
}

enum ValueChildren<'a> {
    Root(Option<&'a Value>),
    Array(std::slice::Iter<'a, Value>),
    Object(serde_json::map::Values<'a>),
}
impl<'a> Iterator for ValueChildren<'a> {
    type Item = &'a Value;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Root(value) => value.take(),
            Self::Array(values) => values.next(),
            Self::Object(values) => values.next(),
        }
    }
}

/// A writer that rejects bytes exceeding its budget before forwarding them.
#[derive(Debug)]
pub struct BoundedWriter<W> {
    inner: W,
    maximum: usize,
    written: usize,
    limit_exceeded: bool,
}
impl<W> BoundedWriter<W> {
    /// Wraps a writer with an exact encoded-byte ceiling.
    pub const fn new(inner: W, maximum: usize) -> Self {
        Self {
            inner,
            maximum,
            written: 0,
            limit_exceeded: false,
        }
    }
    /// Whether this wrapper rejected a write for exceeding its byte ceiling.
    /// Underlying writer errors do not set this flag, regardless of their error kind.
    pub const fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }
    /// Returns the wrapped writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}
impl<W: std::io::Write> std::io::Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum - self.written {
            self.limit_exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "encoded storage data exceeds byte limit",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.written += written;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Validates and encodes one value without exceeding the value-byte ceiling.
pub fn encode_value(value: &Value) -> Result<Vec<u8>> {
    validate_depth(value)?;
    let mut writer = BoundedWriter::new(Vec::new(), MAXIMUM_STORAGE_VALUE_BYTES);
    serde_json::to_writer(&mut writer, value)
        .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
    Ok(writer.into_inner())
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

#[cfg(test)]
mod value_tests {
    use super::*;
    #[test]
    fn depth_and_encoding_limits_match_decoder_safe_values() {
        for objects in [false, true] {
            let mut value = Value::Null;
            for _ in 1..MAXIMUM_STORAGE_VALUE_DEPTH {
                value = if objects {
                    serde_json::json!({"child":value})
                } else {
                    Value::Array(vec![value])
                };
            }
            let encoded = encode_value(&value).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), value);
            assert!(matches!(
                validate_value(&Value::Array(vec![value])),
                Err(StorageError::InvalidInput(_))
            ));
        }
        let boundary = Value::String("a".repeat(MAXIMUM_STORAGE_VALUE_BYTES - 2));
        assert_eq!(
            validate_value(&boundary).unwrap(),
            MAXIMUM_STORAGE_VALUE_BYTES
        );
        let excessive = Value::String("a".repeat(MAXIMUM_STORAGE_VALUE_BYTES - 1));
        assert!(validate_value(&excessive).is_err());
        let mut writer = BoundedWriter::new(Vec::new(), 3);
        std::io::Write::write_all(&mut writer, b"abc").unwrap();
        assert!(std::io::Write::write_all(&mut writer, b"d").is_err());
        assert_eq!(writer.into_inner(), b"abc");
    }
}
