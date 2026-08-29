//! Runtime-independent user Settings contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Maximum namespace identifier bytes.
pub const MAXIMUM_SETTINGS_NAMESPACE_BYTES: usize = 256;
/// Maximum encoded bytes in one raw namespace section.
pub const MAXIMUM_SETTINGS_SECTION_BYTES: usize = 4 * 1024 * 1024;

/// Complete raw user document indexed by namespace.
pub type SettingsDocument = BTreeMap<String, Value>;

/// Closed Settings failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SettingsError {
    /// A caller supplied malformed or out-of-bounds data.
    #[error("invalid settings input: {0}")]
    InvalidInput(String),
    /// A namespace is already registered by another active owner.
    #[error("settings namespace `{0}` is already registered")]
    DuplicateNamespace(String),
    /// A scope no longer belongs to the active registration.
    #[error("settings namespace `{0}` registration is stale")]
    StaleRegistration(String),
    /// A write used an obsolete namespace revision.
    #[error("settings conflict: expected revision {expected}, actual revision {actual}")]
    Conflict {
        /// Revision supplied by the writer.
        expected: u64,
        /// Current committed namespace revision.
        actual: u64,
    },
    /// The provider observed a raw section changed outside this service view.
    #[error("settings document changed concurrently")]
    ConcurrentDocumentChange,
    /// The configured provider is read-only.
    #[error("settings provider is read-only")]
    ReadOnly,
    /// Durable state is malformed.
    #[error("settings document is corrupt: {0}")]
    Corrupt(String),
    /// Provider I/O failed.
    #[error("settings I/O failed: {0}")]
    Io(String),
}

/// Settings result.
pub type Result<T> = std::result::Result<T, SettingsError>;

/// Raw-document persistence seam supplied by one provider plugin.
#[async_trait]
pub trait SettingsProvider: fmt::Debug + Send + Sync + 'static {
    /// Whether this provider accepts writes.
    fn writable(&self) -> bool;
    /// Loads and validates the complete raw document.
    async fn load(&self) -> Result<SettingsDocument>;
    /// Atomically compares and replaces one raw namespace section.
    async fn compare_and_set(
        &self,
        namespace: &str,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> Result<Option<Value>>;
}

/// Nominal Local contract for [`SettingsProvider`].
#[derive(Debug)]
pub struct SettingsProviderContract;

impl LocalContract for SettingsProviderContract {
    const KEY: &'static str = "rsi.settings.provider";
    type Service = dyn SettingsProvider;
}

/// Pure validation function owned by a namespace consumer.
pub trait SettingsValidator: fmt::Debug + Send + Sync + 'static {
    /// Validates one fully resolved namespace value.
    fn validate(&self, value: &Value) -> Result<()>;
}

/// Function adapter for [`SettingsValidator`].
pub struct ValidateWith<F>(pub F);

impl<F> fmt::Debug for ValidateWith<F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValidateWith(..)")
    }
}

impl<F> SettingsValidator for ValidateWith<F>
where
    F: Fn(&Value) -> Result<()> + Send + Sync + 'static,
{
    fn validate(&self, value: &Value) -> Result<()> {
        (self.0)(value)
    }
}

/// Immutable namespace registration declaration.
#[derive(Clone, Debug)]
pub struct SettingsSpec {
    /// Exact namespace identity.
    pub namespace: String,
    /// Schema-owned defaults.
    pub defaults: Value,
    /// Composition-owned base value.
    pub base: Value,
    /// Pure validator for the fully merged value.
    pub validator: Arc<dyn SettingsValidator>,
}

/// Current resolved namespace value and CAS revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSnapshot {
    /// Monotonic revision of this namespace's raw user section.
    pub revision: u64,
    /// Frozen resolved value.
    pub value: Value,
}

/// One active namespace scope.
#[async_trait]
pub trait SettingsScope: fmt::Debug + Send + Sync + 'static {
    /// Returns the current resolved value.
    fn get(&self) -> Result<SettingsSnapshot>;
    /// Replaces the raw user section under revision CAS.
    async fn replace(&self, expected_revision: u64, value: Value) -> Result<SettingsSnapshot>;
    /// Clears the raw user section under revision CAS.
    async fn clear(&self, expected_revision: u64) -> Result<SettingsSnapshot>;
}

/// Active namespace registration and its generation-owned lease.
pub struct SettingsRegistration {
    /// Consumer interface for this namespace.
    pub scope: Arc<dyn SettingsScope>,
    /// Lease whose drop unregisters the namespace.
    pub lease: SettingsLease,
}

impl fmt::Debug for SettingsRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SettingsRegistration")
            .field("scope", &self.scope)
            .field("lease", &self.lease)
            .finish()
    }
}

/// Opaque namespace ownership lease.
pub struct SettingsLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl SettingsLease {
    /// Creates an effect lease from an unregister action.
    pub fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl fmt::Debug for SettingsLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SettingsLease(..)")
    }
}

impl Drop for SettingsLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

/// Settings namespace registry and resolver.
pub trait Settings: fmt::Debug + Send + Sync + 'static {
    /// Registers one exact namespace until the returned lease is dropped.
    fn register(&self, spec: SettingsSpec) -> Result<SettingsRegistration>;
}

/// Nominal Local contract for [`Settings`].
#[derive(Debug)]
pub struct SettingsContract;

impl LocalContract for SettingsContract {
    const KEY: &'static str = "rsi.settings";
    type Service = dyn Settings;
}

/// Validates one exact Settings namespace.
pub fn validate_namespace(namespace: &str) -> Result<()> {
    if namespace.is_empty()
        || namespace.len() > MAXIMUM_SETTINGS_NAMESPACE_BYTES
        || !namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(SettingsError::InvalidInput(
            "namespace must be a nonempty bounded ASCII identifier".into(),
        ));
    }
    Ok(())
}

/// Validates and returns the encoded size of one raw section.
pub fn validate_section(value: &Value) -> Result<usize> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| SettingsError::InvalidInput(error.to_string()))?;
    if bytes.len() > MAXIMUM_SETTINGS_SECTION_BYTES {
        return Err(SettingsError::InvalidInput(format!(
            "settings section exceeds {MAXIMUM_SETTINGS_SECTION_BYTES} bytes"
        )));
    }
    Ok(bytes.len())
}
