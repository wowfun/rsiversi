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
    ExecutionCoordinates, ExecutionLocation, MAXIMUM_REGISTERED_WORKSPACES,
    MAXIMUM_WORKSPACES_PER_PAGE, Result, WorkspaceCursor, WorkspaceDisplayOrder, WorkspaceError,
    WorkspaceId, WorkspaceKey, WorkspacePage, WorkspaceRecord, WorkspaceRegistry,
    WorkspaceRegistryContract, WorkspaceStatus,
};
use rsi_workspace_protocol::{WorkspaceIngress, WorkspaceIngressContract};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::{Mutex, RwLock};

const DOMAIN_ID: &str = "rsi.workspace";
const DOMAIN_VERSION: u32 = 4;
const ALLOCATION_KEY: &str = "allocation";
const MAXIMUM_WORKSPACES: usize = MAXIMUM_REGISTERED_WORKSPACES;
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    display_order: Vec<u64>,
}

#[derive(Debug, Default)]
struct RegistryData {
    next_order: u64,
    order: BTreeMap<u64, WorkspaceId>,
    records: BTreeMap<WorkspaceId, (u64, WorkspaceRecord)>,
    coordinates: BTreeMap<WorkspaceKey, WorkspaceId>,
    display_order: Vec<u64>,
}

#[derive(Clone, Debug)]
struct Service {
    resolver: Arc<dyn ExecutionResolver>,
    origin: CallOrigin,
    domain: Arc<dyn Domain>,
    state: Arc<RwLock<RegistryData>>,
    revision: Arc<AtomicU64>,
    commit: Arc<Mutex<()>>,
    execution: Execution,
    closed: Arc<AtomicBool>,
}

#[async_trait]
impl WorkspaceRegistry for Service {
    async fn display_order(&self) -> Result<WorkspaceDisplayOrder> {
        let state = self.state.read().await;
        self.ensure_available()?;
        visible_order(&state, &state.display_order, &mut Permissions::new(self))
    }

    async fn insert_before(
        &self,
        id: &WorkspaceId,
        before: Option<&WorkspaceId>,
    ) -> Result<WorkspaceDisplayOrder> {
        let commit = Arc::clone(&self.commit).lock_owned().await;
        let current = self.state.read().await;
        self.ensure_available()?;
        let mut permissions = Permissions::new(self);
        let mut position = |id: &WorkspaceId| -> Result<u64> {
            let (position, record) = current
                .records
                .get(id)
                .ok_or_else(|| WorkspaceError::Unknown(id.clone()))?;
            if !permissions.permits(record.coordinates.location())? {
                return Err(WorkspaceError::Api(ApiError::Unauthorized));
            }
            Ok(*position)
        };
        let moved = position(id)?;
        let anchor = before.map(&mut position).transpose()?;
        let mut next = current.display_order.clone();
        if anchor != Some(moved) {
            next.retain(|position| *position != moved);
            let index = anchor.map_or(next.len(), |anchor| {
                next.iter()
                    .position(|position| *position == anchor)
                    .expect("registered anchor has a display position")
            });
            next.insert(index, moved);
        }
        let result = visible_order(&current, &next, &mut permissions)?;
        if next == current.display_order {
            return Ok(result);
        }
        let value = serde_json::to_value(Allocation {
            high_water: current.next_order,
            display_order: next.clone(),
        })
        .map_err(|error| WorkspaceError::Storage(error.to_string()))?;
        let permits = permissions.locations;
        drop(current);
        let domain = self.domain.clone();
        let state = self.state.clone();
        let revision = self.revision.clone();
        self.execution
            .spawn(async move {
                let (_commit, _permits) = (commit, permits);
                domain
                    .put(ALLOCATION_KEY, value)
                    .await
                    .map_err(storage_error)?;
                state.write().await.display_order = next;
                revision.fetch_add(1, Ordering::Release);
                Ok(result)
            })
            .await
            .map_err(|error| {
                WorkspaceError::Storage(format!("workspace commit task failed: {error}"))
            })?
    }

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

    async fn find(&self, key: &WorkspaceKey) -> Result<Option<WorkspaceRecord>> {
        let state = self.state.read().await;
        self.ensure_available()?;
        let Some(id) = state.coordinates.get(key) else {
            return Ok(None);
        };
        let record = &state.records[id].1;
        let _permit = self.admit(record.coordinates.location())?;
        Ok(Some(record.clone()))
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
        self.register(location, path)
            .await
            .map(|(record, _)| record)
    }

    async fn rename(
        &self,
        id: &WorkspaceId,
        name: rsi_workspace_protocol::WorkspaceName,
    ) -> Result<WorkspaceRecord> {
        let commit = Arc::clone(&self.commit).lock_owned().await;
        let state = self.state.read().await;
        self.ensure_available()?;
        let (order, current) = state
            .records
            .get(id)
            .ok_or_else(|| WorkspaceError::Unknown(id.clone()))?;
        let permit = self.admit(current.coordinates.location())?;
        let order = *order;
        let mut record = current.clone();
        record.name = Some(name);
        if &record == current {
            return Ok(record);
        }
        drop(state);
        let value = serde_json::to_value(DurableWorkspaceRecord {
            order,
            record: record.clone(),
        })
        .map_err(|error| WorkspaceError::Storage(error.to_string()))?;
        let domain = self.domain.clone();
        let state = self.state.clone();
        let revision = self.revision.clone();
        self.execution
            .spawn(async move {
                let (_commit, _permit) = (commit, permit);
                domain
                    .put(record.id.as_str(), value)
                    .await
                    .map_err(storage_error)?;
                state
                    .write()
                    .await
                    .records
                    .insert(record.id.clone(), (order, record.clone()));
                revision.fetch_add(1, Ordering::Release);
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
        let coordinates = WorkspaceKey::from_coordinates(&record.coordinates);
        drop(state);
        let domain = Arc::clone(&self.domain);
        let state = Arc::clone(&self.state);
        let id = id.clone();
        let revision = self.revision.clone();
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
                state.coordinates.remove(&coordinates);
                state.order.remove(&order);
                state.display_order.retain(|position| *position != order);
                revision.fetch_add(1, Ordering::Release);
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
            revision: Arc::default(),
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
    let canonical = rsi_workspace_protocol::canonical_native_path(canonical);
    rsi_workspace_protocol::validate_workspace_path(&canonical)?;
    Ok(canonical)
}

fn load_registry(mut snapshot: BTreeMap<String, ConfigValue>) -> Result<RegistryData> {
    let allocation = snapshot.remove(ALLOCATION_KEY);
    let Allocation {
        high_water,
        display_order,
    } = match allocation {
        Some(value) => serde_json::from_value::<Allocation>(value)
            .map_err(|error| WorkspaceError::Corrupt(error.to_string()))?,
        None if snapshot.is_empty() => Allocation {
            high_water: 0,
            display_order: Vec::new(),
        },
        None => {
            return Err(WorkspaceError::Corrupt(
                "workspace allocation is missing".into(),
            ));
        }
    };
    if display_order.len() > MAXIMUM_WORKSPACES
        || display_order
            .iter()
            .any(|position| *position == 0 || *position > high_water)
    {
        return Err(WorkspaceError::Corrupt(
            "invalid Workspace display positions".into(),
        ));
    }
    let known = display_order.iter().copied().collect::<HashSet<_>>();
    if known.len() != display_order.len() {
        return Err(WorkspaceError::Corrupt(
            "duplicated Workspace display positions".into(),
        ));
    }
    if snapshot.len() > MAXIMUM_WORKSPACES {
        return Err(WorkspaceError::Corrupt(
            "workspace record bound is exceeded".into(),
        ));
    }
    let mut state = RegistryData {
        next_order: high_water,
        ..RegistryData::default()
    };
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
            || state
                .coordinates
                .insert(
                    WorkspaceKey::from_coordinates(&record.coordinates),
                    record.id.clone(),
                )
                .is_some()
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
    state.display_order = state
        .order
        .keys()
        .rev()
        .copied()
        .filter(|position| !known.contains(position))
        .chain(
            display_order
                .into_iter()
                .filter(|position| state.order.contains_key(position)),
        )
        .collect();
    Ok(state)
}

fn storage_error(error: StorageError) -> WorkspaceError {
    WorkspaceError::Api(rsi_storage_domain::storage_error(error))
}

fn storage_meta(error: &StorageError) -> MetaError {
    MetaError::Activation(error.to_string())
}

impl Service {
    async fn register(
        &self,
        location: &ExecutionLocation,
        path: &Path,
    ) -> Result<(WorkspaceRecord, bool)> {
        let permit = self.admit(location)?;
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
        let key = WorkspaceKey::from_coordinates(&coordinates);
        let current = self.state.read().await;
        self.ensure_available()?;
        if let Some(id) = current.coordinates.get(&key) {
            return Ok((current.records[id].1.clone(), false));
        }
        drop(current);
        let commit = Arc::clone(&self.commit).lock_owned().await;
        let state = self.state.read().await;
        self.ensure_available()?;
        if let Some(id) = state.coordinates.get(&key) {
            return Ok((state.records[id].1.clone(), false));
        }
        if state.records.len() == MAXIMUM_WORKSPACES {
            return Err(WorkspaceError::InvalidInput(
                "workspace registry reached its capacity".into(),
            ));
        }
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| WorkspaceError::Storage(error.to_string()))?;
        let id = WorkspaceId::from_bytes(bytes);
        let mut record = WorkspaceRecord::new(id.clone(), coordinates);
        record.created_at_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| WorkspaceError::Storage("system clock precedes Unix epoch".into()))?
                .as_millis()
                .try_into()
                .map_err(|_| WorkspaceError::Storage("system clock is out of range".into()))?,
        );
        record.validate()?;
        let order = state
            .next_order
            .checked_add(1)
            .ok_or_else(|| WorkspaceError::InvalidInput("workspace order exhausted".into()))?;
        let allocation = serde_json::to_value(Allocation {
            high_water: order,
            display_order: state.display_order.clone(),
        })
        .map_err(|error| WorkspaceError::Storage(error.to_string()))?;
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
        let revision = self.revision.clone();
        self.execution
            .spawn(async move {
                let _commit = commit;
                let _permit = permit;
                // Reserve even an uncertain allocation locally; an error cannot prove
                // that the durable write did not happen. No registration is published
                // until its reservation has been acknowledged.
                state.write().await.next_order = order;
                domain
                    .put(ALLOCATION_KEY, allocation)
                    .await
                    .map_err(storage_error)?;
                domain
                    .put(&durable_key, value)
                    .await
                    .map_err(storage_error)?;
                let mut state = state.write().await;
                state.order.insert(order, id.clone());
                state.coordinates.insert(key, id.clone());
                state.records.insert(id, (order, record.clone()));
                state.display_order.insert(0, order);
                revision.fetch_add(1, Ordering::Release);
                Ok((record, true))
            })
            .await
            .map_err(|error| {
                WorkspaceError::Storage(format!("workspace commit task failed: {error}"))
            })?
    }
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
#[async_trait]
impl WorkspaceIngress for Service {
    fn directory_revision(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Acquire))
    }
    async fn register_local(
        &self,
        origin: CallOrigin,
        path: &Path,
    ) -> Result<(WorkspaceRecord, bool)> {
        Self {
            origin,
            ..self.clone()
        }
        .register(&ExecutionLocation::Local, path)
        .await
    }

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
fn visible_order(
    state: &RegistryData,
    positions: &[u64],
    permissions: &mut Permissions<'_>,
) -> Result<WorkspaceDisplayOrder> {
    let mut ids = Vec::with_capacity(positions.len());
    for position in positions {
        let id = &state.order[position];
        let record = &state.records[id].1;
        if permissions.permits(record.coordinates.location())? {
            ids.push(id.clone());
        }
    }
    Ok(WorkspaceDisplayOrder { ids })
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
