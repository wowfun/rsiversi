//! Credential resolution and secret storage for standalone `rsi-ai` callers.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Credential failures carry stable machine-readable codes.

use std::{collections::BTreeMap, fmt, sync::Arc};

use rsi_ai_protocol::{MAX_ID_BYTES, validate_identifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

const MAX_SECRET_BYTES: usize = 64 * 1024;

/// A validated credential lookup key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialId(String);

impl CredentialId {
    /// Creates a bounded printable credential identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialError> {
        let value = value.into();
        validate_ascii_id("credential id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidates an identifier decoded from an untrusted snapshot.
    pub fn validate(&self) -> Result<(), CredentialError> {
        validate_ascii_id("credential id", &self.0)
    }
}

/// Secret text whose formatting is redacted and owned buffer is zeroed on drop.
#[derive(Clone)]
pub struct SecretValue(Arc<SecretInner>);

struct SecretInner {
    value: Zeroizing<String>,
}

impl SecretValue {
    /// Captures a bounded safe UTF-8 secret in zeroizing owned storage.
    pub fn new(value: impl Into<String>) -> Result<Self, CredentialError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_SECRET_BYTES
            || value
                .chars()
                .any(|character| character == '\0' || character == '\u{7f}')
        {
            return Err(CredentialError::new(
                "credential.invalid_secret",
                format!("secret must contain 1..={MAX_SECRET_BYTES} safe UTF-8 bytes"),
            ));
        }
        Ok(Self(Arc::new(SecretInner {
            value: Zeroizing::new(value),
        })))
    }

    /// Exposes the secret only to the provider/auth seam.
    pub fn expose(&self) -> &str {
        self.0.value.as_str()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Credential source recorded in a redacted prepared-call snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// A per-registry explicit override.
    Explicit,
    /// A nonpersistent in-memory credential.
    Memory,
    /// The configured persistent credential store.
    Store,
    /// A process environment variable captured during builder configuration.
    Environment,
}

/// Serializable source facts that never contain the credential value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSourceSnapshot {
    pub id: CredentialId,
    /// Winning source in the deterministic precedence order.
    pub source: CredentialSource,
    /// Captured variable name only when `source` is [`CredentialSource::Environment`].
    pub environment_variable: Option<String>,
}

impl CredentialSourceSnapshot {
    /// Revalidates facts decoded from a durable prepared-call snapshot.
    pub fn validate(&self) -> Result<(), CredentialError> {
        self.id.validate()?;
        match (self.source, self.environment_variable.as_deref()) {
            (CredentialSource::Environment, Some(variable)) => validate_environment_name(variable),
            (CredentialSource::Environment, None) => Err(CredentialError::new(
                "credential.invalid_source",
                "environment credential source has no variable name",
            )),
            (_, None) => Ok(()),
            (_, Some(_)) => Err(CredentialError::new(
                "credential.invalid_source",
                "non-environment credential source has an environment variable",
            )),
        }
    }
}

/// A resolved secret plus its persistable redacted source facts.
#[derive(Clone)]
pub struct ResolvedCredential {
    secret: SecretValue,
    source: CredentialSourceSnapshot,
}

impl ResolvedCredential {
    /// Exposes secret text only at the provider/auth seam.
    pub fn expose_secret(&self) -> &str {
        self.secret.expose()
    }

    pub fn secret(&self) -> &SecretValue {
        &self.secret
    }

    /// Returns persistable source facts containing no secret value.
    pub const fn source(&self) -> &CredentialSourceSnapshot {
        &self.source
    }
}

impl fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedCredential")
            .field("secret", &"[REDACTED]")
            .field("source", &self.source)
            .finish()
    }
}

/// One provider credential requirement and its captured-environment fallbacks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialRequirement {
    id: CredentialId,
    environment_variables: Vec<String>,
}

impl CredentialRequirement {
    /// Creates a logical credential requirement with ordered environment fallbacks.
    pub fn new<I, S>(
        id: impl Into<String>,
        environment_variables: I,
    ) -> Result<Self, CredentialError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let id = CredentialId::new(id)?;
        let environment_variables = environment_variables
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        for variable in &environment_variables {
            validate_environment_name(variable)?;
        }
        Ok(Self {
            id,
            environment_variables,
        })
    }

    pub const fn id(&self) -> &CredentialId {
        &self.id
    }
}

/// Pluggable persistent credential seam. Production uses the OS keyring.
pub trait CredentialStore: fmt::Debug + Send + Sync {
    /// Loads a credential or reports that no stored value exists.
    fn get(&self, id: &CredentialId) -> Result<Option<SecretValue>, StoreError>;
    /// Replaces the stored value for an identifier.
    fn set(&self, id: &CredentialId, secret: &SecretValue) -> Result<(), StoreError>;
    /// Deletes an identifier; absence must be treated as success.
    fn delete(&self, id: &CredentialId) -> Result<(), StoreError>;
}

/// OS-native keyring adapter.
#[derive(Clone, Debug)]
pub struct OsKeyringStore {
    service: String,
}

impl OsKeyringStore {
    /// Creates an OS-keyring adapter under one bounded service name.
    pub fn new(service: impl Into<String>) -> Result<Self, CredentialError> {
        let service = service.into();
        validate_ascii_id("keyring service", &service)?;
        Ok(Self { service })
    }

    fn entry(&self, id: &CredentialId) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, id.as_str())
            .map_err(|error| StoreError::new("credential.store", error.to_string()))
    }
}

impl CredentialStore for OsKeyringStore {
    fn get(&self, id: &CredentialId) -> Result<Option<SecretValue>, StoreError> {
        match self.entry(id)?.get_password() {
            Ok(value) => SecretValue::new(value)
                .map(Some)
                .map_err(|error| StoreError::new("credential.store_value", error.to_string())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(StoreError::new("credential.store", error.to_string())),
        }
    }

    fn set(&self, id: &CredentialId, secret: &SecretValue) -> Result<(), StoreError> {
        self.entry(id)?
            .set_password(secret.expose())
            .map_err(|error| StoreError::new("credential.store", error.to_string()))
    }

    fn delete(&self, id: &CredentialId) -> Result<(), StoreError> {
        match self.entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(StoreError::new("credential.store", error.to_string())),
        }
    }
}

/// Immutable credential resolver used by a standalone Registry.
#[derive(Clone)]
pub struct CredentialManager {
    explicit: Arc<BTreeMap<CredentialId, SecretValue>>,
    memory: Arc<BTreeMap<CredentialId, SecretValue>>,
    store: Option<Arc<dyn CredentialStore>>,
    environment: Arc<BTreeMap<String, SecretValue>>,
}

impl CredentialManager {
    /// Starts a builder whose mutable inputs are frozen by [`CredentialManagerBuilder::build`].
    #[must_use]
    pub fn builder() -> CredentialManagerBuilder {
        CredentialManagerBuilder::default()
    }

    /// Resolves explicit, memory, store, then captured-environment sources in that order.
    pub fn resolve(
        &self,
        requirement: &CredentialRequirement,
    ) -> Result<ResolvedCredential, CredentialError> {
        if let Some(resolved) = self.try_resolve_in_memory(requirement) {
            return resolved;
        }
        let Some(store) = self.store.as_ref() else {
            return resolve_environment(requirement, &self.environment);
        };
        if let Some(secret) = store
            .get(requirement.id())
            .map_err(CredentialError::store)?
        {
            return Ok(resolved(
                requirement.id.clone(),
                secret,
                CredentialSource::Store,
                None,
            ));
        }
        resolve_environment(requirement, &self.environment)
    }

    /// Resolves sources that cannot call a persistent credential backend.
    ///
    /// `None` means a configured persistent store must be queried before
    /// environment fallback can be considered.
    pub fn try_resolve_in_memory(
        &self,
        requirement: &CredentialRequirement,
    ) -> Option<Result<ResolvedCredential, CredentialError>> {
        if let Some(secret) = self.explicit.get(requirement.id()) {
            return Some(Ok(resolved(
                requirement.id.clone(),
                secret.clone(),
                CredentialSource::Explicit,
                None,
            )));
        }
        if let Some(secret) = self.memory.get(requirement.id()) {
            return Some(Ok(resolved(
                requirement.id.clone(),
                secret.clone(),
                CredentialSource::Memory,
                None,
            )));
        }
        if self.store.is_some() {
            return None;
        }
        Some(resolve_environment(requirement, &self.environment))
    }

    /// Persists a credential through the configured store without changing this manager.
    pub fn persist(&self, id: &CredentialId, secret: &SecretValue) -> Result<(), CredentialError> {
        self.store
            .as_ref()
            .ok_or_else(|| {
                CredentialError::new(
                    "credential.no_store",
                    "no persistent credential store is configured",
                )
            })?
            .set(id, secret)
            .map_err(CredentialError::store)
    }
}

fn resolve_environment(
    requirement: &CredentialRequirement,
    environment: &BTreeMap<String, SecretValue>,
) -> Result<ResolvedCredential, CredentialError> {
    for variable in &requirement.environment_variables {
        if let Some(secret) = environment.get(variable) {
            return Ok(resolved(
                requirement.id.clone(),
                secret.clone(),
                CredentialSource::Environment,
                Some(variable.clone()),
            ));
        }
    }
    Err(CredentialError::new(
        "credential.missing",
        format!("credential `{}` is unavailable", requirement.id.as_str()),
    ))
}

impl fmt::Debug for CredentialManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialManager")
            .field("explicit_ids", &self.explicit.keys().collect::<Vec<_>>())
            .field("memory_ids", &self.memory.keys().collect::<Vec<_>>())
            .field("store", &self.store)
            .field(
                "environment_names",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Builder that captures mutable secret sources before Registry construction.
#[derive(Debug, Default)]
pub struct CredentialManagerBuilder {
    explicit: BTreeMap<CredentialId, SecretValue>,
    memory: BTreeMap<CredentialId, SecretValue>,
    store: Option<Arc<dyn CredentialStore>>,
    environment: BTreeMap<String, SecretValue>,
}

impl CredentialManagerBuilder {
    /// Adds the highest-precedence secret for one logical identifier.
    pub fn with_explicit(
        mut self,
        id: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        insert_secret(&mut self.explicit, id, secret)?;
        Ok(self)
    }

    /// Adds a nonpersistent secret below explicit overrides in precedence.
    pub fn with_memory(
        mut self,
        id: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        insert_secret(&mut self.memory, id, secret)?;
        Ok(self)
    }

    #[must_use]
    /// Selects the persistent credential adapter queried before environment fallback.
    pub fn with_store(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Selects the production OS-keyring adapter under one service name.
    pub fn with_os_keyring(self, service: impl Into<String>) -> Result<Self, CredentialError> {
        Ok(self.with_store(Arc::new(OsKeyringStore::new(service)?)))
    }

    /// Replaces captured environment fallbacks with supplied deterministic values.
    pub fn with_captured_environment(
        mut self,
        environment: BTreeMap<String, String>,
    ) -> Result<Self, CredentialError> {
        let mut captured = BTreeMap::new();
        for (name, value) in environment {
            validate_environment_name(&name)?;
            captured.insert(name, SecretValue::new(value)?);
        }
        self.environment = captured;
        Ok(self)
    }

    /// Captures selected process variables once; later process changes are ignored.
    pub fn capture_process_environment<I, S>(mut self, names: I) -> Result<Self, CredentialError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut captured = BTreeMap::new();
        for name in names.into_iter().map(Into::into) {
            validate_environment_name(&name)?;
            if let Ok(value) = std::env::var(&name) {
                captured.insert(name, SecretValue::new(value)?);
            }
        }
        self.environment = captured;
        Ok(self)
    }

    #[must_use]
    /// Freezes all configured sources into an immutable resolver.
    pub fn build(self) -> CredentialManager {
        CredentialManager {
            explicit: Arc::new(self.explicit),
            memory: Arc::new(self.memory),
            store: self.store,
            environment: Arc::new(self.environment),
        }
    }
}

fn resolved(
    id: CredentialId,
    secret: SecretValue,
    source: CredentialSource,
    environment_variable: Option<String>,
) -> ResolvedCredential {
    ResolvedCredential {
        secret,
        source: CredentialSourceSnapshot {
            id,
            source,
            environment_variable,
        },
    }
}

fn insert_secret(
    target: &mut BTreeMap<CredentialId, SecretValue>,
    id: impl Into<String>,
    secret: impl Into<String>,
) -> Result<(), CredentialError> {
    let id = CredentialId::new(id)?;
    if target.contains_key(&id) {
        return Err(CredentialError::new(
            "credential.duplicate",
            format!("credential `{}` was configured more than once", id.as_str()),
        ));
    }
    target.insert(id, SecretValue::new(secret)?);
    Ok(())
}

fn validate_ascii_id(field: &str, value: &str) -> Result<(), CredentialError> {
    validate_identifier(field, value)
        .map_err(|message| CredentialError::new("credential.invalid_id", message))
}

fn validate_environment_name(value: &str) -> Result<(), CredentialError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
    {
        return Err(CredentialError::new(
            "credential.invalid_environment",
            "environment variable name is invalid",
        ));
    }
    Ok(())
}

/// Credential resolution failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct CredentialError {
    code: &'static str,
    message: String,
}

impl CredentialError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn store(error: StoreError) -> Self {
        Self::new(error.code, error.message)
    }

    /// Returns the stable machine-readable credential failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

/// Persistent credential adapter failure with no secret payload.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct StoreError {
    code: &'static str,
    message: String,
}

impl StoreError {
    /// Creates a safe persistent-adapter failure without secret payloads.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the adapter-supplied machine-readable failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}
