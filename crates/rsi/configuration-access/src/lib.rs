//! Durable Local-owned configuration grants and Settings mutation admission.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_protocol::{
    ApiError, CallOrigin, DeviceAdministration, DeviceAdministrationContract, DeviceId, Result,
};
use rsi_configuration_api::GrantSnapshot;
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use rsi_settings_api::{
    SettingsMutationLease, SettingsMutationPolicy, SettingsMutationPolicyContract,
};
use rsi_settings_protocol::{SettingsAccess, SettingsAccessContract};
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::task::{TaskTracker, task_tracker::TaskTrackerToken};

mod credentials;
mod endpoint;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    devices: BTreeSet<DeviceId>,
}
#[derive(Debug, Default)]
struct Gate {
    state: Mutex<(bool, TaskTracker)>,
}
impl Gate {
    fn open() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((true, TaskTracker::new())),
        })
    }
    fn admit(&self) -> Result<TaskTrackerToken> {
        let state = self.state.lock().expect("configuration gate poisoned");
        if !state.0 {
            return Err(ApiError::Unauthorized);
        }
        Ok(state.1.token())
    }
    fn close(&self) -> TaskTracker {
        let mut state = self.state.lock().expect("configuration gate poisoned");
        state.0 = false;
        state.1.close();
        state.1.clone()
    }
}
#[derive(Debug)]
struct Admission {
    closed: bool,
    local: Arc<Gate>,
    devices: BTreeMap<DeviceId, Arc<Gate>>,
}

/// Authority retained until an admitted configuration operation has completed.
#[derive(Debug)]
pub struct ConfigurationLease {
    _token: TaskTrackerToken,
    _capacity: OwnedSemaphorePermit,
}

/// Ordinary Host-owned configuration authority; it cannot resolve credentials.
#[derive(Debug)]
pub struct ConfigurationAccess {
    domain: Arc<dyn Domain>,
    administration: Arc<dyn DeviceAdministration>,
    settings: Arc<dyn SettingsAccess>,
    admission: Mutex<Admission>,
    slots: Arc<Semaphore>,
    writer: Arc<Semaphore>,
    tasks: TaskTracker,
    execution: Execution,
}
impl ConfigurationAccess {
    async fn open(
        execution: Execution,
        domain: Arc<dyn Domain>,
        administration: Arc<dyn DeviceAdministration>,
        settings: Arc<dyn SettingsAccess>,
    ) -> Result<Arc<Self>> {
        let owner = Arc::new(Self {
            domain,
            administration,
            settings,
            admission: Mutex::new(Admission {
                closed: false,
                local: Gate::open(),
                devices: BTreeMap::new(),
            }),
            slots: Arc::new(Semaphore::new(8)),
            writer: Arc::new(Semaphore::new(1)),
            tasks: TaskTracker::new(),
            execution,
        });
        let document = owner.document().await?;
        owner
            .admission
            .lock()
            .expect("configuration admission poisoned")
            .devices = document
            .devices
            .into_iter()
            .map(|id| (id, Gate::open()))
            .collect();
        Ok(owner)
    }

    /// Admits one bounded mutation using trusted connection identity.
    ///
    /// # Panics
    /// Panics if an earlier owner panic poisoned admission state.
    pub fn admit(&self, origin: &CallOrigin) -> Result<ConfigurationLease> {
        let admission = self
            .admission
            .lock()
            .expect("configuration admission poisoned");
        if admission.closed {
            return Err(ApiError::ShuttingDown);
        }
        let gate = match origin {
            CallOrigin::Local => &admission.local,
            CallOrigin::Device(device) => {
                if device.revoked.is_cancelled() {
                    return Err(ApiError::Unauthorized);
                }
                admission
                    .devices
                    .get(&device.id)
                    .ok_or(ApiError::Unauthorized)?
            }
        };
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        Ok(ConfigurationLease {
            _token: gate.admit()?,
            _capacity: permit,
        })
    }

    /// Reports effective current-generation authority without granting it.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn allowed(&self, origin: &CallOrigin) -> bool {
        let admission = self
            .admission
            .lock()
            .expect("configuration admission poisoned");
        if admission.closed {
            return false;
        }
        match origin {
            CallOrigin::Local => true,
            CallOrigin::Device(device) => {
                !device.revoked.is_cancelled()
                    && admission.devices.get(&device.id).is_some_and(|gate| {
                        gate.state.lock().expect("configuration gate poisoned").0
                    })
            }
        }
    }

    /// Reads the durable grant state; callers reconcile this after an uncertain write.
    pub async fn snapshot(&self) -> Result<GrantSnapshot> {
        let doc = self.document().await?;
        Ok(GrantSnapshot {
            revision: doc.revision.to_string(),
            devices: doc.devices.into_iter().collect(),
        })
    }
    async fn document(&self) -> Result<Document> {
        let mut records = self.domain.snapshot().await;
        if records.len() > 1 || records.keys().any(|key| key != "grants") {
            return Err(ApiError::Backend(
                "invalid configuration grant records".into(),
            ));
        }
        let doc: Document = records
            .remove("grants")
            .map_or_else(|| Ok(Document::default()), serde_json::from_value)
            .map_err(|_| ApiError::Backend("invalid configuration grant document".into()))?;
        if doc.devices.len() > 64 {
            return Err(ApiError::Backend(
                "configuration grant count exceeds 64".into(),
            ));
        }
        Ok(doc)
    }

    /// Starts one Local-only expected-revision grant change before returning its waiter.
    /// Dropping the waiter leaves the exact admitted operation owned by this plugin.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn set_grant(
        self: &Arc<Self>,
        origin: &CallOrigin,
        device: DeviceId,
        expected: &str,
        granted: bool,
    ) -> Result<BoxFuture<'static, Result<GrantSnapshot>>> {
        if !matches!(origin, CallOrigin::Local) {
            return Err(ApiError::Unauthorized);
        }
        let revision = expected
            .parse::<u64>()
            .ok()
            .filter(|revision| revision.to_string() == expected)
            .ok_or_else(|| ApiError::Invalid("invalid configuration revision".into()))?;
        let admission = self
            .admission
            .lock()
            .expect("configuration admission poisoned");
        if admission.closed {
            return Err(ApiError::ShuttingDown);
        }
        let permit = self
            .writer
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let owner = self.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            owner.change_grant(device, revision, granted).await
        }));
        drop(admission);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }
    async fn change_grant(
        &self,
        device: DeviceId,
        expected: u64,
        granted: bool,
    ) -> Result<GrantSnapshot> {
        let mut document = self.document().await?;
        if document.revision != expected {
            return Err(ApiError::Invalid(
                "configuration revision conflict; refresh grants".into(),
            ));
        }
        if granted
            && !self
                .administration
                .list()?
                .iter()
                .any(|record| record.id == device)
        {
            return Err(ApiError::Invalid(
                "configuration grants require a registered device".into(),
            ));
        }
        if granted {
            if !document.devices.contains(&device) && document.devices.len() == 64 {
                return Err(ApiError::Capacity);
            }
            document.devices.insert(device.clone());
        } else {
            let gate = self
                .admission
                .lock()
                .expect("configuration admission poisoned")
                .devices
                .get(&device)
                .cloned();
            if let Some(gate) = gate {
                gate.close().wait().await;
            }
            document.devices.remove(&device);
        }
        document.revision = document
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("configuration revision exhausted".into()))?;
        self.domain
            .put(
                "grants",
                serde_json::to_value(&document).map_err(|_| {
                    ApiError::Invalid("configuration document encoding failed".into())
                })?,
            )
            .await
            .map_err(|_| ApiError::OutcomeUnknown)?;
        let mut admission = self
            .admission
            .lock()
            .expect("configuration admission poisoned");
        if granted && !admission.closed {
            // A failed revocation may have left the prior generation closed. Explicit
            // grant publication is the only operation that may replace that gate.
            admission
                .devices
                .entry(device)
                .and_modify(|gate| {
                    let closed = !gate.state.lock().expect("configuration gate poisoned").0;
                    if closed {
                        *gate = Gate::open();
                    }
                })
                .or_insert_with(Gate::open);
        } else {
            admission.devices.remove(&device);
        }
        drop(admission);
        Ok(GrantSnapshot {
            revision: document.revision.to_string(),
            devices: document.devices.into_iter().collect(),
        })
    }
    async fn close(&self) {
        let gates = {
            let mut admission = self
                .admission
                .lock()
                .expect("configuration admission poisoned");
            admission.closed = true;
            self.writer.close();
            self.slots.close();
            self.tasks.close();
            std::iter::once(admission.local.close())
                .chain(admission.devices.values().map(|gate| gate.close()))
                .collect::<Vec<_>>()
        };
        futures_util::future::join_all(gates.iter().map(TaskTracker::wait)).await;
        self.tasks.wait().await;
    }
}
#[async_trait]
impl SettingsMutationPolicy for ConfigurationAccess {
    async fn admit(
        &self,
        origin: &CallOrigin,
        namespace: &str,
        replacement: Option<&Value>,
    ) -> Result<Box<dyn SettingsMutationLease>> {
        let lease = self.admit(origin)?;
        if matches!(origin, CallOrigin::Device(_)) {
            match namespace {
                "rsi.agent" | "rsi.client" => {}
                "rsi.agent-presets" => {
                    let value = replacement.ok_or(ApiError::Unauthorized)?;
                    let current = self
                        .settings
                        .read(namespace)
                        .await
                        .map_err(|_| ApiError::Unavailable)?;
                    let fields = value.as_object().ok_or(ApiError::Unauthorized)?;
                    if fields.keys().any(|key| key != "default" && key != "roots")
                        || value.get("roots") != current.value.get("roots")
                    {
                        return Err(ApiError::Unauthorized);
                    }
                }
                _ => return Err(ApiError::Unauthorized),
            }
        }
        Ok(Box::new(lease))
    }
}

/// Nominal Local configuration authority shared by configuration endpoint owners.
#[derive(Debug)]
pub struct ConfigurationAccessContract;
impl LocalContract for ConfigurationAccessContract {
    const KEY: &'static str = "rsi.configuration.access";
    type Service = ConfigurationAccess;
}

/// Ordinary grant owner using one explicitly selected Storage backend.
#[derive(Clone, Debug, Default)]
pub struct ConfigurationAccessFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: String,
}
#[async_trait]
impl PluginFactory for ConfigurationAccessFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let input: Config = serde_json::from_value(config.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        if input.backend.is_empty() || input.backend.len() > 256 {
            return Err(MetaError::InvalidInput("invalid grant backend".into()));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<rsi_credentials_protocol::CredentialsStatusContract>()
            .requiring_local::<rsi_credentials_protocol::CredentialsAdminContract>()
            .requiring_local::<DeviceAdministrationContract>()
            .requiring_local::<SettingsAccessContract>()
            .requiring_local::<rsi_api_protocol::ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let input: Config = serde_json::from_value(plan.config().as_ref().clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.configuration.grants".into(),
                backend: input.backend,
                version: 1,
                maximum_records: 1,
                maximum_bytes: 64 * 1024,
            })
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let owner = ConfigurationAccess::open(
            plan.context().runtime().execution().clone(),
            domain,
            plan.local::<DeviceAdministrationContract>()?,
            plan.local::<SettingsAccessContract>()?,
        )
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let mut registrations = endpoint::register(
            plan.local::<rsi_api_protocol::ApiRegistrarContract>()?
                .as_ref(),
            owner.clone(),
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        registrations.extend(
            credentials::register(
                plan.local::<rsi_api_protocol::ApiRegistrarContract>()?
                    .as_ref(),
                owner.clone(),
                plan.local::<rsi_credentials_protocol::CredentialsStatusContract>()?,
                plan.local::<rsi_credentials_protocol::CredentialsAdminContract>()?,
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let supplies = vec![
            plan.context()
                .provide_local::<ConfigurationAccessContract>(owner.clone())?,
            plan.context()
                .provide_local::<SettingsMutationPolicyContract>(owner.clone())?,
        ];
        plan.defer(
            "drain configuration authority",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    // Configuration leases may belong to other endpoint generations.
                    let closing = owner.close();
                    let endpoints = futures_util::future::join_all(
                        registrations
                            .into_iter()
                            .map(rsi_api_protocol::ApiRegistration::close),
                    );
                    futures_util::join!(closing, endpoints);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests;
