//! Durable host-local Workspace registry plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_api_protocol::{ApiError, CallOrigin};
use rsi_execution::{ExecutionOperation, ExecutionResolver, ExecutionResolverContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, MetaError, PluginFactory, PreparedActivation,
};
use rsi_storage::StorageError;
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use rsi_workspace_protocol::{
    ExecutionCoordinates, ExecutionLocation, MAXIMUM_WORKSPACES_PER_PAGE, Result, WorkspaceCursor,
    WorkspaceError, WorkspaceId, WorkspacePage, WorkspaceRecord, WorkspaceRegistry,
    WorkspaceRegistryContract, WorkspaceStatus,
};
use rsi_workspace_protocol::{WorkspaceIngress, WorkspaceIngressContract};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, RwLock};

const DOMAIN_ID: &str = "rsi.workspace";
const DOMAIN_VERSION: u32 = 4;
const ALLOCATION_KEY: &str = "allocation";
const MAXIMUM_WORKSPACES: usize = 16_384;
const MAXIMUM_WORKSPACE_DOMAIN_BYTES: usize = 128 * 1024 * 1024;

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Allocation {
    high_water: u64,
}

#[derive(Debug, Default)]
struct RegistryData {
    next_order: u64,
    order: BTreeMap<u64, WorkspaceId>,
    records: BTreeMap<WorkspaceId, (u64, WorkspaceRecord)>,
}

#[derive(Clone, Debug)]
struct Service {
    resolver: Arc<dyn ExecutionResolver>,
    origin: CallOrigin,
    domain: Arc<dyn Domain>,
    state: Arc<RwLock<RegistryData>>,
    commit: Arc<Mutex<()>>,
    execution: Execution,
    closed: Arc<AtomicBool>,
}

#[async_trait]
impl WorkspaceRegistry for Service {
    async fn order_seed(&self) -> Result<rsi_workspace_protocol::WorkspaceOrderSeed> {
        let state = self.state.read().await;
        self.ensure_available()?;
        let mut permissions = Permissions::new(self);
        let mut error = None;
        let mut records = state.records.values();
        let visible = std::iter::from_fn(|| {
            loop {
                let (_, record) = records.next()?;
                match permissions.permits(record.coordinates.location()) {
                    Ok(true) => return Some(record),
                    Ok(false) => {}
                    Err(failure) => {
                        error = Some(failure);
                        return None;
                    }
                }
            }
        });
        let seed = rsi_workspace_protocol::WorkspaceOrderSeed::from_records(visible);
        error.map_or(Ok(seed), Err)
    }
    async fn get(&self, id: &WorkspaceId) -> Result<WorkspaceRecord> {
        let state = self.state.read().await;
        self.ensure_available()?;
        let record = state
            .records
            .get(id)
            .map(|(_, record)| record)
            .ok_or_else(|| WorkspaceError::Unknown(id.clone()))?;
        let _permit = self.admit(record.coordinates.location())?;
        Ok(record.clone())
    }

    async fn list(&self, after: Option<WorkspaceCursor>, limit: usize) -> Result<WorkspacePage> {
        if limit == 0 || limit > MAXIMUM_WORKSPACES_PER_PAGE {
            return Err(WorkspaceError::InvalidInput(format!(
                "workspace page limit must be in 1..={MAXIMUM_WORKSPACES_PER_PAGE}"
            )));
        }
        let state = self.state.read().await;
        self.ensure_available()?;
        let start = after.map_or(0, |cursor| cursor.after_order);
        let mut permissions = Permissions::new(self);
        let mut records = Vec::with_capacity(limit);
        let mut last = start;
        let mut next = None;
        for (order, id) in state
            .order
            .range((std::ops::Bound::Excluded(start), std::ops::Bound::Unbounded))
        {
            let record = &state
                .records
                .get(id)
                .expect("registry order references an existing record")
                .1;
            if !permissions.permits(record.coordinates.location())? {
                continue;
            }
            if records.len() == limit {
                next = Some(WorkspaceCursor { after_order: last });
                break;
            }
            records.push(record.clone());
            last = *order;
        }
        Ok(WorkspacePage { records, next })
    }

    async fn register_at(
        &self,
        location: &ExecutionLocation,
        path: &Path,
    ) -> Result<WorkspaceRecord> {
        let _permit = self.admit(location)?;
        let text = rsi_workspace_protocol::validate_registration_path(location, path)?;
        let coordinates = match location {
            ExecutionLocation::Local => {
                let canonical = canonical_directory(path).await?;
                ExecutionCoordinates::new(
                    location.clone(),
                    canonical.to_str().expect("validated UTF-8 path"),
                )
                .map_err(|error| WorkspaceError::InvalidInput(error.to_string()))?
            }
            ExecutionLocation::Ssh { .. } => self
                .resolver
                .lease(self.origin.clone(), location)
                .map_err(WorkspaceError::Api)?
                .canonicalize(text)
                .await
                .map_err(execution_error)?,
        };
        let id = WorkspaceId::from_coordinates(&coordinates);
        let current = self.state.read().await;
        self.ensure_available()?;
        if let Some((_, existing)) = current.records.get(&id) {
            return Ok(existing.clone());
        }
        drop(current);
        let _commit = Arc::clone(&self.commit).lock_owned().await;
        let state = self.state.read().await;
        self.ensure_available()?;
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
            .any(|(_, record)| record.coordinates == coordinates)
        {
            return Err(WorkspaceError::Corrupt(
                "canonical path has a conflicting identity".into(),
            ));
        }
        let record = WorkspaceRecord {
            id: id.clone(),
            coordinates,
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
        self.execution
            .spawn(async move {
                let _commit = _commit;
                let _permit = _permit;
                // Reserve even an uncertain allocation locally; an error cannot prove
                // that the durable write did not happen. No registration is published
                // until its reservation has been acknowledged.
                state.write().await.next_order = order;
                domain
                    .put(ALLOCATION_KEY, serde_json::json!({"high_water": order}))
                    .await
                    .map_err(storage_error)?;
                domain
                    .put(&durable_key, value)
                    .await
                    .map_err(storage_error)?;
                let mut state = state.write().await;
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
        let record = self.get(id).await?;
        let _permit = self.admit(record.coordinates.location())?;
        if record.coordinates.location() != &ExecutionLocation::Local {
            let actual = self
                .resolver
                .lease(self.origin.clone(), record.coordinates.location())
                .map_err(WorkspaceError::Api)?
                .canonicalize(record.coordinates.path())
                .await
                .map_err(execution_error)?;
            return Ok(if actual == record.coordinates {
                WorkspaceStatus::Ok
            } else {
                WorkspaceStatus::MissingDirectory
            });
        }
        Ok(
            match tokio::fs::symlink_metadata(record.coordinates.path()).await {
                Ok(metadata) if metadata.is_dir() => WorkspaceStatus::Ok,
                _ => WorkspaceStatus::MissingDirectory,
            },
        )
    }

    async fn delete_registration(&self, id: &WorkspaceId) -> Result<bool> {
        let _commit = Arc::clone(&self.commit).lock_owned().await;
        let state = self.state.read().await;
        self.ensure_available()?;
        let Some((order, record)) = state.records.get(id) else {
            return Ok(false);
        };
        let _permit = self.admit(record.coordinates.location())?;
        let order = *order;
        drop(state);
        let domain = Arc::clone(&self.domain);
        let state = Arc::clone(&self.state);
        let id = id.clone();
        self.execution
            .spawn(async move {
                let _commit = _commit;
                let _permit = _permit;
                if !domain.delete(id.as_str()).await.map_err(storage_error)? {
                    return Err(WorkspaceError::Corrupt(
                        "registered Workspace is absent from its durable domain".into(),
                    ));
                }
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
                .requiring_local::<DomainFacilityContract>()
                .requiring_local::<ExecutionResolverContract>(),
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
                maximum_records: MAXIMUM_WORKSPACES + 1,
                maximum_bytes: MAXIMUM_WORKSPACE_DOMAIN_BYTES,
            })
            .await
            .map_err(|error| storage_meta(&error))?;
        let snapshot = domain
            .snapshot()
            .await
            .map_err(|error| storage_meta(&error))?;
        let state =
            load_registry(snapshot).map_err(|error| MetaError::Activation(error.to_string()))?;
        let service = Arc::new(Service {
            resolver: plan.local::<ExecutionResolverContract>()?,
            origin: CallOrigin::Local,
            domain,
            state: Arc::new(RwLock::new(state)),
            commit: Arc::new(Mutex::new(())),
            execution: plan.context().runtime().execution().clone(),
            closed: Arc::new(AtomicBool::new(false)),
        });
        let supply = plan
            .context()
            .provide_local::<WorkspaceRegistryContract>(service.clone())?;
        let ingress = plan
            .context()
            .provide_local::<WorkspaceIngressContract>(service.clone())?;
        plan.defer(
            "withdraw Workspace registry",
            Box::new(move || {
                Box::pin(async move {
                    service.closed.store(true, Ordering::Release);
                    // Admission and its retained task hold this same slot; a
                    // dropped requester cannot let retirement miss the commit.
                    let _drained = service.commit.lock().await;
                    drop(supply);
                    drop(ingress);
                    Ok(())
                })
            }),
        )
    }
}

async fn canonical_directory(path: &Path) -> Result<PathBuf> {
    rsi_workspace_protocol::validate_workspace_path(path)?;
    if !path.is_absolute() {
        return Err(WorkspaceError::InvalidInput(
            "workspace path must be absolute".into(),
        ));
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| WorkspaceError::InvalidInput(error.to_string()))?;
    let metadata = tokio::fs::symlink_metadata(&canonical)
        .await
        .map_err(|error| WorkspaceError::InvalidInput(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(WorkspaceError::InvalidInput(
            "workspace path must name a directory".into(),
        ));
    }
    rsi_workspace_protocol::validate_workspace_path(&canonical)?;
    Ok(canonical)
}

fn load_registry(mut snapshot: BTreeMap<String, ConfigValue>) -> Result<RegistryData> {
    let allocation = snapshot.remove(ALLOCATION_KEY);
    let high_water = match allocation {
        Some(value) => {
            serde_json::from_value::<Allocation>(value)
                .map_err(|error| WorkspaceError::Corrupt(error.to_string()))?
                .high_water
        }
        None if snapshot.is_empty() => 0,
        None => {
            return Err(WorkspaceError::Corrupt(
                "workspace allocation is missing".into(),
            ));
        }
    };
    if snapshot.len() > MAXIMUM_WORKSPACES {
        return Err(WorkspaceError::Corrupt(
            "workspace record bound is exceeded".into(),
        ));
    }
    let mut state = RegistryData {
        next_order: high_water,
        ..RegistryData::default()
    };
    let mut paths = HashSet::new();
    for (key, value) in snapshot {
        let durable: DurableWorkspaceRecord = serde_json::from_value(value)
            .map_err(|error| WorkspaceError::Corrupt(error.to_string()))?;
        let record = durable.record;
        record
            .validate()
            .map_err(|error| WorkspaceError::Corrupt(error.to_string()))?;
        if durable.order == 0
            || durable.order > high_water
            || key != record.id.as_str()
            || !paths.insert(record.coordinates.clone())
        {
            return Err(WorkspaceError::Corrupt(
                "workspace record identity or path is inconsistent".into(),
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
    }
    Ok(state)
}

fn storage_error(error: StorageError) -> WorkspaceError {
    WorkspaceError::Api(rsi_storage_domain::storage_error(error))
}

fn storage_meta(error: &StorageError) -> MetaError {
    MetaError::Activation(error.to_string())
}

impl Service {
    fn ensure_available(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(WorkspaceError::Api(ApiError::ShuttingDown));
        }
        if let CallOrigin::Device(device) = &self.origin
            && device.revoked.is_cancelled()
        {
            return Err(WorkspaceError::Api(ApiError::Unauthorized));
        }
        self.domain.ensure_available().map_err(storage_error)
    }
    fn admit(&self, location: &ExecutionLocation) -> Result<ExecutionOperation> {
        self.ensure_available()?;
        self.resolver
            .admit(&self.origin, location)
            .map_err(WorkspaceError::Api)
    }
}
impl WorkspaceIngress for Service {
    fn scoped(&self, origin: CallOrigin) -> Arc<dyn WorkspaceRegistry> {
        Arc::new(Self {
            origin,
            ..self.clone()
        })
    }
}
// Keep each location's accepted operation alive for the entire bounded snapshot.
struct Permissions<'a> {
    service: &'a Service,
    locations: BTreeMap<ExecutionLocation, Option<ExecutionOperation>>,
}
impl<'a> Permissions<'a> {
    fn new(service: &'a Service) -> Self {
        Self {
            service,
            locations: BTreeMap::new(),
        }
    }
    fn permits(&mut self, location: &ExecutionLocation) -> Result<bool> {
        if let std::collections::btree_map::Entry::Vacant(entry) =
            self.locations.entry(location.clone())
        {
            let permit = match self.service.admit(location) {
                Ok(permit) => Some(permit),
                Err(WorkspaceError::Api(ApiError::Unauthorized)) => None,
                Err(error) => return Err(error),
            };
            entry.insert(permit);
        }
        Ok(self.locations[location].is_some())
    }
}
fn execution_error(error: rsi_process::ProcessError) -> WorkspaceError {
    WorkspaceError::Api(match error {
        rsi_process::ProcessError::Api(error) => error,
        rsi_process::ProcessError::Capacity => ApiError::Capacity,
        rsi_process::ProcessError::ShuttingDown => ApiError::ShuttingDown,
        rsi_process::ProcessError::OutcomeUnknown => ApiError::OutcomeUnknown,
        rsi_process::ProcessError::InvalidInput(message) => {
            return WorkspaceError::InvalidInput(message);
        }
        _ => ApiError::Unavailable,
    })
}
