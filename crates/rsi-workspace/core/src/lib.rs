//! Durable host-local Workspace registry plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_storage::StorageError;
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

const DOMAIN_ID: &str = "rsi.workspace";
const DOMAIN_VERSION: u32 = 2;
const MAXIMUM_WORKSPACES: usize = 16_384;
const MAXIMUM_WORKSPACE_DOMAIN_BYTES: usize = 128 * 1024 * 1024;

/// Stable host-local workspace identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

impl WorkspaceId {
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

/// Durable workspace registration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecord {
    /// Stable identity derived from canonical path.
    pub id: WorkspaceId,
    /// Canonical physical absolute directory.
    pub path: PathBuf,
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
    /// Returns registrations in stable user order.
    async fn list(&self) -> Vec<WorkspaceRecord>;
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

/// Configuration accepted by [`WorkspaceFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Exact non-session storage backend route.
    pub backend: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableWorkspaceRecord {
    order: u64,
    record: WorkspaceRecord,
}

#[derive(Debug, Default)]
struct RegistryData {
    next_order: u64,
    order: BTreeMap<u64, WorkspaceId>,
    records: BTreeMap<WorkspaceId, (u64, WorkspaceRecord)>,
}

#[derive(Debug)]
struct Service {
    domain: Arc<dyn Domain>,
    state: Arc<RwLock<RegistryData>>,
    commit: Arc<Mutex<()>>,
}

#[async_trait]
impl WorkspaceRegistry for Service {
    async fn list(&self) -> Vec<WorkspaceRecord> {
        let state = self.state.read().await;
        state
            .order
            .values()
            .filter_map(|id| state.records.get(id).map(|(_, record)| record.clone()))
            .collect()
    }

    async fn get_or_create(&self, path: &Path) -> Result<WorkspaceRecord> {
        let canonical = canonical_directory(path).await?;
        let id = workspace_id(&canonical)?;
        if let Some((_, existing)) = self.state.read().await.records.get(&id) {
            return Ok(existing.clone());
        }
        let _commit = Arc::clone(&self.commit).lock_owned().await;
        let state = self.state.read().await;
        if let Some((_, existing)) = state.records.get(&id) {
            return Ok(existing.clone());
        }
        if state.records.len() == MAXIMUM_WORKSPACES {
            return Err(WorkspaceError::InvalidInput(
                "workspace registry reached its capacity".into(),
            ));
        }
        if state
            .records
            .values()
            .any(|(_, record)| record.path == canonical)
        {
            return Err(WorkspaceError::Corrupt(
                "canonical path has a conflicting identity".into(),
            ));
        }
        let record = WorkspaceRecord {
            id: id.clone(),
            path: canonical,
        };
        let order = state
            .next_order
            .checked_add(1)
            .ok_or_else(|| WorkspaceError::InvalidInput("workspace order exhausted".into()))?;
        drop(state);
        let durable = DurableWorkspaceRecord {
            order,
            record: record.clone(),
        };
        let value = serde_json::to_value(durable)
            .map_err(|error| WorkspaceError::Storage(error.to_string()))?;
        let domain = Arc::clone(&self.domain);
        let state = Arc::clone(&self.state);
        let durable_key = id.as_str().to_owned();
        tokio::spawn(async move {
            let _commit = _commit;
            domain
                .put(&durable_key, value)
                .await
                .map_err(|error| storage_error(&error))?;
            let mut state = state.write().await;
            state.next_order = order;
            state.order.insert(order, id.clone());
            state.records.insert(id, (order, record.clone()));
            Ok(record)
        })
        .await
        .map_err(|error| {
            WorkspaceError::Storage(format!("workspace commit task failed: {error}"))
        })?
    }

    async fn status(&self, id: &WorkspaceId) -> Result<WorkspaceStatus> {
        let path = self
            .state
            .read()
            .await
            .records
            .get(id)
            .map(|(_, record)| record.path.clone())
            .ok_or_else(|| WorkspaceError::Unknown(id.clone()))?;
        Ok(match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_dir() => WorkspaceStatus::Ok,
            _ => WorkspaceStatus::MissingDirectory,
        })
    }

    async fn delete_registration(&self, id: &WorkspaceId) -> Result<bool> {
        let _commit = Arc::clone(&self.commit).lock_owned().await;
        let state = self.state.read().await;
        let Some((order, _)) = state.records.get(id) else {
            return Ok(false);
        };
        let order = *order;
        drop(state);
        let domain = Arc::clone(&self.domain);
        let state = Arc::clone(&self.state);
        let id = id.clone();
        tokio::spawn(async move {
            let _commit = _commit;
            domain
                .delete(id.as_str())
                .await
                .map_err(|error| storage_error(&error))?;
            let mut state = state.write().await;
            state.records.remove(&id);
            state.order.remove(&order);
            Ok(true)
        })
        .await
        .map_err(|error| {
            WorkspaceError::Storage(format!("workspace commit task failed: {error}"))
        })?
    }
}

/// Ordinary factory for one Workspace registry generation.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceFactory;

#[async_trait]
impl PluginFactory for WorkspaceFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: WorkspaceConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        rsi_storage::validate_identifier("workspace backend", &config.backend)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config.backend.len();
        Ok(
            PreparedActivation::with_state(desired.clone(), config, retained)
                .requiring_local::<DomainFacilityContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<WorkspaceConfig>()?;
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: DOMAIN_ID.into(),
                backend: config.backend,
                version: DOMAIN_VERSION,
                maximum_records: MAXIMUM_WORKSPACES,
                maximum_bytes: MAXIMUM_WORKSPACE_DOMAIN_BYTES,
            })
            .await
            .map_err(|error| storage_meta(&error))?;
        let snapshot = domain.snapshot().await;
        let state =
            load_registry(snapshot).map_err(|error| MetaError::Activation(error.to_string()))?;
        let service: Arc<dyn WorkspaceRegistry> = Arc::new(Service {
            domain,
            state: Arc::new(RwLock::new(state)),
            commit: Arc::new(Mutex::new(())),
        });
        let supply = plan
            .context()
            .provide_local::<WorkspaceRegistryContract>(service)?;
        plan.defer(
            "withdraw Workspace registry",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

async fn canonical_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(WorkspaceError::InvalidInput(
            "workspace path must be absolute".into(),
        ));
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| WorkspaceError::InvalidInput(error.to_string()))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|error| WorkspaceError::InvalidInput(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(WorkspaceError::InvalidInput(
            "workspace path must name a directory".into(),
        ));
    }
    Ok(canonical)
}

fn workspace_id(path: &Path) -> Result<WorkspaceId> {
    let path = path
        .to_str()
        .ok_or_else(|| WorkspaceError::InvalidInput("workspace path is not UTF-8".into()))?;
    Ok(WorkspaceId(hex::encode(Sha256::digest(path.as_bytes()))))
}

fn load_registry(snapshot: BTreeMap<String, ConfigValue>) -> Result<RegistryData> {
    if snapshot.len() > MAXIMUM_WORKSPACES {
        return Err(WorkspaceError::Corrupt(
            "workspace record bound is exceeded".into(),
        ));
    }
    let mut state = RegistryData::default();
    let mut paths = HashSet::new();
    for (key, value) in snapshot {
        let durable: DurableWorkspaceRecord = serde_json::from_value(value)
            .map_err(|error| WorkspaceError::Corrupt(error.to_string()))?;
        let record = durable.record;
        if durable.order == 0
            || key != record.id.as_str()
            || !record.path.is_absolute()
            || !paths.insert(record.path.clone())
        {
            return Err(WorkspaceError::Corrupt(
                "workspace record identity or path is inconsistent".into(),
            ));
        }
        if workspace_id(&record.path)? != record.id {
            return Err(WorkspaceError::Corrupt(
                "workspace identity does not match its physical path".into(),
            ));
        }
        let id = record.id.clone();
        if state.order.insert(durable.order, id.clone()).is_some()
            || state.records.insert(id, (durable.order, record)).is_some()
        {
            return Err(WorkspaceError::Corrupt(
                "workspace order or identity is duplicated".into(),
            ));
        }
        state.next_order = state.next_order.max(durable.order);
    }
    Ok(state)
}

fn storage_error(error: &StorageError) -> WorkspaceError {
    WorkspaceError::Storage(error.to_string())
}

fn storage_meta(error: &StorageError) -> MetaError {
    MetaError::Activation(error.to_string())
}
