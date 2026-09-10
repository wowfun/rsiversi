//! Workspace identities and transport-independent registry contract.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::LocalContract;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Stable host-local workspace identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Validates the canonical lowercase SHA-256 identity at an external boundary.
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(WorkspaceError::InvalidInput(
                "workspace identity must be 64 lowercase hex digits".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Borrows the exact identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Largest page accepted by a Workspace provider.
pub const MAXIMUM_WORKSPACES_PER_PAGE: usize = 256;

/// Maximum UTF-8 bytes in a host workspace path.
pub const MAXIMUM_WORKSPACE_PATH_BYTES: usize = rsi_workspace_path::MAXIMUM_HOST_PATH_BYTES;

/// Bounds a path before native access or retaining an external record.
pub fn validate_workspace_path(path: &Path) -> Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| WorkspaceError::InvalidInput("workspace path is not UTF-8".into()))?;
    if text.is_empty() || text.len() > MAXIMUM_WORKSPACE_PATH_BYTES || text.contains('\0') {
        return Err(WorkspaceError::InvalidInput(
            "workspace path is empty, contains NUL or exceeds 16 KiB".into(),
        ));
    }
    Ok(text)
}

/// Exclusive durable insertion-order position, usable after record deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCursor {
    /// Last returned record's immutable order in this registry.
    pub after_order: u64,
}

/// Bounded live registry page in ascending insertion order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePage {
    /// At most the requested number of registrations.
    pub records: Vec<WorkspaceRecord>,
    /// Continuation when another record existed at the page read.
    pub next: Option<WorkspaceCursor>,
}

/// Durable workspace registration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecord {
    /// Stable identity derived from canonical path.
    pub id: WorkspaceId,
    /// Canonical physical absolute directory.
    pub path: PathBuf,
}

impl WorkspaceRecord {
    /// Validates a remote record without interpreting the host's path on this device.
    pub fn validate(&self) -> Result<()> {
        use sha2::Digest as _;
        let path = validate_workspace_path(&self.path)?;
        if !rsi_workspace_path::is_normalized_absolute(path) {
            return Err(WorkspaceError::Corrupt(
                "workspace path is not a normalized absolute host path".into(),
            ));
        }
        if hex::encode(sha2::Sha256::digest(path.as_bytes())) != self.id.as_str() {
            return Err(WorkspaceError::Corrupt(
                "workspace identity does not match its path".into(),
            ));
        }
        Ok(())
    }
}

impl WorkspacePage {
    /// Validates a remote page's count, identities and exclusive continuation.
    pub fn validate(&self, after: Option<WorkspaceCursor>, limit: usize) -> Result<()> {
        if !(1..=MAXIMUM_WORKSPACES_PER_PAGE).contains(&limit) || self.records.len() > limit {
            return Err(WorkspaceError::InvalidInput(
                "workspace page limit must be 1..=256 and bound the records".into(),
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for record in &self.records {
            record.validate()?;
            if !ids.insert(&record.id) {
                return Err(WorkspaceError::Corrupt(
                    "workspace page repeats an identity".into(),
                ));
            }
        }
        if self.next.is_some_and(|cursor| {
            self.records.is_empty()
                || cursor.after_order <= after.map_or(0, |cursor| cursor.after_order)
        }) {
            return Err(WorkspaceError::Corrupt(
                "workspace page continuation does not advance".into(),
            ));
        }
        Ok(())
    }
}

/// Current uncached directory status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    /// Canonical directory still exists.
    Ok,
    /// Registered directory is missing or no longer a directory.
    MissingDirectory,
}

/// Closed Workspace failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkspaceError {
    /// Connection failure preserved by a remote Workspace proxy.
    #[error(transparent)]
    Api(rsi_api_protocol::ApiError),
    /// Provider or adapter admission is full.
    #[error("workspace capacity is exhausted")]
    Capacity,
    /// The service generation is stopping.
    #[error("workspace service is shutting down")]
    ShuttingDown,
    /// Invalid or unavailable path/identity.
    #[error("invalid workspace: {0}")]
    InvalidInput(String),
    /// Unknown registry identity.
    #[error("workspace `{0}` is not registered")]
    Unknown(WorkspaceId),
    /// Durable state is malformed or inconsistent.
    #[error("workspace registry is corrupt: {0}")]
    Corrupt(String),
    /// Storage-domain operation failed.
    #[error("workspace storage failed: {0}")]
    Storage(String),
}

/// Workspace result.
pub type Result<T> = std::result::Result<T, WorkspaceError>;

/// Durable host-local Workspace registry.
#[async_trait]
pub trait WorkspaceRegistry: fmt::Debug + Send + Sync + 'static {
    /// Reads an exact registration without probing or mutating the filesystem.
    async fn get(&self, id: &WorkspaceId) -> Result<WorkspaceRecord>;
    /// Returns at most `limit` records in stable insertion order.
    /// The limit must be in `1..=MAXIMUM_WORKSPACES_PER_PAGE`.
    async fn list(&self, after: Option<WorkspaceCursor>, limit: usize) -> Result<WorkspacePage>;
    /// Finds or durably creates the canonical directory registration.
    async fn get_or_create(&self, path: &Path) -> Result<WorkspaceRecord>;
    /// Returns current filesystem status without mutating state.
    async fn status(&self, id: &WorkspaceId) -> Result<WorkspaceStatus>;
    /// Deletes only the registration and returns whether it existed.
    async fn delete_registration(&self, id: &WorkspaceId) -> Result<bool>;
}

/// Nominal Local contract for [`WorkspaceRegistry`].
#[derive(Debug)]
pub struct WorkspaceRegistryContract;

impl LocalContract for WorkspaceRegistryContract {
    const KEY: &'static str = "rsi.workspace";
    type Service = dyn WorkspaceRegistry;
}
