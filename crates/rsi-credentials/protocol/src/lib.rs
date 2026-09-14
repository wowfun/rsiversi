//! Runtime-independent credential contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::{LocalContract, PluginId};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use zeroize::Zeroizing;

/// Maximum owner or slot identifier bytes.
pub const MAXIMUM_CREDENTIAL_IDENTIFIER_BYTES: usize = 256;
/// Maximum accepted secret UTF-8 bytes.
pub const MAXIMUM_SECRET_BYTES: usize = 64 * 1024;

/// Stable, non-secret credential address.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
    /// Stable owner plugin identity.
    pub owner: PluginId,
    /// Owner-local slot.
    pub slot: String,
}

impl<'de> Deserialize<'de> for CredentialRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireCredentialRef {
            owner: PluginId,
            slot: String,
        }

        let wire = WireCredentialRef::deserialize(deserializer)?;
        Self::new(wire.owner, wire.slot).map_err(serde::de::Error::custom)
    }
}

impl CredentialRef {
    /// Creates and validates an exact owner/slot address.
    pub fn new(owner: impl Into<PluginId>, slot: impl Into<String>) -> Result<Self> {
        let reference = Self {
            owner: owner.into(),
            slot: slot.into(),
        };
        reference.validate()?;
        Ok(reference)
    }

    /// Validates this deserialized address.
    pub fn validate(&self) -> Result<()> {
        validate_segment("credential owner", self.owner.as_str())?;
        validate_segment("credential slot", &self.slot)
    }

    /// Returns the stable non-secret address string.
    pub fn account(&self) -> String {
        format!("{}/{}", self.owner, self.slot)
    }
}

/// Owned zeroizing UTF-8 secret.
#[derive(Clone)]
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    /// Creates a bounded secret without exposing it to formatting traits.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = Zeroizing::new(value.into());
        if value.is_empty() || value.len() > MAXIMUM_SECRET_BYTES {
            return Err(CredentialsError::InvalidInput(format!(
                "secret length must be within 1..={MAXIMUM_SECRET_BYTES} bytes"
            )));
        }
        Ok(Self(value))
    }

    /// Borrows the secret text for immediate use.
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue(<redacted>)")
    }
}

/// Non-secret provenance of one resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialSource {
    /// Historical OS keyring provenance in durable call facts.
    Keyring,
    /// Private credential file entry.
    File,
    /// Explicitly captured startup environment variable.
    Environment {
        /// Non-secret environment variable name.
        variable: String,
    },
}

impl CredentialSource {
    /// Revalidates non-secret provenance decoded from durable facts.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Keyring | Self::File => Ok(()),
            Self::Environment { variable } => validate_environment_name(variable),
        }
    }
}

impl<'de> Deserialize<'de> for CredentialSource {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum WireCredentialSource {
            Keyring,
            File,
            Environment { variable: String },
        }

        let source = match WireCredentialSource::deserialize(deserializer)? {
            WireCredentialSource::Keyring => Self::Keyring,
            WireCredentialSource::File => Self::File,
            WireCredentialSource::Environment { variable } => Self::Environment { variable },
        };
        source
            .validate()
            .map(|()| source)
            .map_err(serde::de::Error::custom)
    }
}

/// Resolved secret and redacted provenance.
#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    /// Owned secret value.
    pub secret: SecretValue,
    /// Non-secret source fact.
    pub source: CredentialSource,
}

/// Closed credential failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CredentialsError {
    /// Malformed or out-of-bounds caller input.
    #[error("invalid credential input: {0}")]
    InvalidInput(String),
    /// No configured source resolved this reference.
    #[error("credential is not configured: {0}")]
    NotConfigured(String),
    /// A definite failure before mutation publication.
    #[error("credential store failed: {0}")]
    Store(CredentialStoreFailure),
    /// Publication may have occurred; callers must reconcile, never replay.
    #[error("credential write outcome is unknown; refresh before taking further action")]
    OutcomeUnknown,
    /// This waiter exceeded the configured resolution deadline.
    #[error("credential resolution timed out: {0}")]
    Timeout(String),
}

/// Safe, closed storage failures; no backend diagnostic or secret text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStoreFailure {
    /// Ownership, mode or filesystem access is unsuitable.
    #[error("credential file permissions or ownership are invalid")]
    Permissions,
    /// A link, special file or invalid path was rejected.
    #[error("credential path must contain private directories and regular files without links")]
    UnsafePath,
    /// Invalid JSON, duplicate record, version or field.
    #[error("credential file is invalid; repair it before retrying")]
    Corrupt,
    /// Encoded document or record count exceeds its contract.
    #[error("credential file exceeds its size or record limit")]
    TooLarge,
    /// Another process retained the writer lock past the deadline.
    #[error("credential file is busy; retry after the other writer completes")]
    LockTimeout,
    /// Filesystem operation failed before publication.
    #[error("credential file could not be accessed; check the filesystem and retry")]
    Io,
    /// No equivalent private file implementation exists on this platform.
    #[error("private credential files are unsupported on this platform")]
    Unsupported,
    /// A read waiter exceeded its deadline.
    #[error("credential status timed out; retry")]
    Timeout,
}

/// Credential result.
pub type Result<T> = std::result::Result<T, CredentialsError>;

/// Per-operation credential resolution service.
#[async_trait]
pub trait CredentialsResolve: fmt::Debug + Send + Sync + 'static {
    /// Resolves one exact reference without caching across calls.
    async fn resolve(&self, reference: &CredentialRef) -> Result<ResolvedCredential>;
}

/// Privileged credential mutation service.
#[async_trait]
pub trait CredentialsAdmin: fmt::Debug + Send + Sync + 'static {
    /// Sets one exact stored entry, overriding any environment fallback.
    async fn set(&self, reference: &CredentialRef, secret: SecretValue) -> Result<()>;
    /// Deletes one stored entry; a captured environment fallback may become effective.
    async fn unset(&self, reference: &CredentialRef) -> Result<bool>;
}

/// Redacted effective credential availability, without store diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialAvailability {
    /// A source currently resolves this exact address.
    Configured {
        /// Effective non-secret provenance.
        source: CredentialSource,
    },
    /// All configured sources were read successfully and contain no value.
    Missing,
    /// The provider could not determine availability.
    Unavailable {
        /// Safe failure category, independent of backend diagnostic text.
        reason: CredentialStoreFailure,
    },
}

/// A safe status projection; this type cannot contain secret material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialStatus {
    /// Effective availability at the time of the read.
    pub availability: CredentialAvailability,
    /// Whether the current provider permits administrative mutation.
    pub editable: bool,
    /// Connected Host's credential file path, at most 2048 UTF-8 bytes.
    /// Absent for injected stores without a filesystem location.
    pub store_path: Option<String>,
}

impl<'de> Deserialize<'de> for CredentialStatus {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            availability: CredentialAvailability,
            editable: bool,
            store_path: Option<String>,
        }
        let wire = Wire::deserialize(decoder)?;
        if wire.store_path.as_ref().is_some_and(|path| {
            path.is_empty() || path.len() > 2048 || path.chars().any(char::is_control)
        }) || wire.editable
            && matches!(
                wire.availability,
                CredentialAvailability::Unavailable { .. }
            )
        {
            return Err(serde::de::Error::custom("invalid credential status"));
        }
        Ok(Self {
            availability: wire.availability,
            editable: wire.editable,
            store_path: wire.store_path,
        })
    }
}

/// Read-only credential setup information, independent of secret resolution authority.
#[async_trait]
pub trait CredentialsStatus: fmt::Debug + Send + Sync + 'static {
    /// Reads one bounded exact reference without returning any secret or store error text.
    async fn status(&self, reference: &CredentialRef) -> Result<CredentialStatus>;
}

/// Nominal Local contract for redacted setup information.
#[derive(Debug)]
pub struct CredentialsStatusContract;

impl LocalContract for CredentialsStatusContract {
    const KEY: &'static str = "rsi.credentials.status";
    type Service = dyn CredentialsStatus;
}

/// Nominal Local contract for provider consumers.
#[derive(Debug)]
pub struct CredentialsResolveContract;

impl LocalContract for CredentialsResolveContract {
    const KEY: &'static str = "rsi.credentials.resolve";
    type Service = dyn CredentialsResolve;
}

/// Nominal Local contract for privileged mutation.
#[derive(Debug)]
pub struct CredentialsAdminContract;

impl LocalContract for CredentialsAdminContract {
    const KEY: &'static str = "rsi.credentials.admin";
    type Service = dyn CredentialsAdmin;
}

/// Validates one bounded owner or slot segment.
pub fn validate_segment(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_CREDENTIAL_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CredentialsError::InvalidInput(format!(
            "{kind} must be a nonempty bounded ASCII identifier"
        )));
    }
    Ok(())
}

/// Validates one explicitly captured portable environment variable name.
pub fn validate_environment_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_CREDENTIAL_IDENTIFIER_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        })
    {
        return Err(CredentialsError::InvalidInput(
            "environment name must be a bounded portable identifier".into(),
        ));
    }
    Ok(())
}
