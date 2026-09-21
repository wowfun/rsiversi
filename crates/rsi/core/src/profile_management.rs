//! Product-owned reviewed source changes; Settings grants do not authorize them.
mod api;
mod grants;
mod source;
mod tools;
pub(crate) use tools::Factory as ToolsFactory;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_protocol::{ApiError, CallOrigin, HostEpoch};
use rsi_configuration_access::{ConfigurationAccess, ConfigurationAccessContract};
use rsi_configuration_api::leaf::{self as wire, Failure, Grant, Principal, Reply};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::task::{TaskTracker, task_tracker::TaskTrackerToken};

pub(crate) fn register(
    builder: &mut crate::StandardAddonBuilder,
    composition: crate::StandardComposition,
    catalog: crate::ProfileCatalog,
    local_api: Option<String>,
) -> rsi_host::Result<()> {
    builder.register_local_contract::<Contract>()?;
    builder.register_linked(
        "rsi.profile-leaves",
        env!("CARGO_PKG_VERSION"),
        rsi_meta::UpdateMode::RestartRequired,
        Arc::new(Factory {
            source: Arc::new(source::Source {
                composition,
                catalog,
                local_api,
            }),
        }),
    )?;
    builder.register_fragment(rsi_host::ProfileFragment::new(
        "rsi.standard.profile-leaves",
        [rsi_host::ProfileEntry::new(
            "rsi.profile-leaves",
            "rsi.profile-leaves",
            ConfigValue::Null,
        )],
    ))?;
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Contract;
impl LocalContract for Contract {
    const KEY: &'static str = "rsi.profile-leaves";
    type Service = Manager;
}
#[derive(Debug)]
struct Factory {
    source: Arc<source::Source>,
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Profile leaf owner requires null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<ConfigurationAccessContract>()
            .requiring_local::<rsi_api_protocol::DeviceAdministrationContract>()
            .requiring_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .requiring_local::<rsi_api_protocol::ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.profile-leaves".into(),
                backend: "base".into(),
                version: 1,
                maximum_records: 1,
                maximum_bytes: 256 * 1024,
            })
            .await
            .map_err(|_| activation())?;
        let mut records = domain.snapshot().await;
        let document = match records.remove("grants") {
            Some(value) => {
                serde_json::from_value::<wire::Grants>(value).map_err(|_| activation())?
            }
            None => wire::Grants {
                revision: "0".into(),
                scopes: vec![],
            },
        };
        document.validate().map_err(|_| activation())?;
        if !records.is_empty() {
            return Err(activation());
        }
        let gates = document
            .scopes
            .iter()
            .cloned()
            .map(|scope| (scope, Gate::open()))
            .collect();
        let owner = Arc::new(Manager {
            source: self.source.clone(),
            context: plan.context().clone(),
            configuration: plan.local::<ConfigurationAccessContract>()?,
            administration: plan.local::<rsi_api_protocol::DeviceAdministrationContract>()?,
            epoch: plan
                .local::<rsi_api_protocol::ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            domain,
            state: Mutex::new(State {
                closed: false,
                uncertain: false,
                document,
                gates,
                previews: BTreeMap::new(),
                receipts: BTreeMap::new(),
            }),
            slots: Arc::new(Semaphore::new(2)),
            writer: Arc::new(Semaphore::new(1)),
            grant_changes: Arc::new(Semaphore::new(4)),
            previews: Arc::new(Semaphore::new(4)),
            tasks: TaskTracker::new(),
        });
        let registrations = api::register(
            plan.local::<rsi_api_protocol::ApiRegistrarContract>()?
                .as_ref(),
            owner.clone(),
        )
        .map_err(|_| activation())?;
        let supply = plan.context().provide_local::<Contract>(owner.clone())?;
        plan.defer(
            "drain Profile leaf management",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    owner.close().await;
                    for registration in registrations {
                        registration.close().await;
                    }
                    Ok(())
                })
            }),
        )
    }
}
fn activation() -> MetaError {
    MetaError::Activation("Profile leaf management unavailable".into())
}

struct Gate {
    open: bool,
    tasks: TaskTracker,
}
impl Gate {
    fn open() -> Self {
        Self {
            open: true,
            tasks: TaskTracker::new(),
        }
    }
    fn close(&mut self) -> TaskTracker {
        self.open = false;
        self.tasks.close();
        self.tasks.clone()
    }
}
struct State {
    closed: bool,
    uncertain: bool,
    document: wire::Grants,
    gates: BTreeMap<Grant, Gate>,
    previews: BTreeMap<String, Proposal>,
    receipts: BTreeMap<String, Saved>,
}
struct Proposal {
    principal: Principal,
    preview: wire::Preview,
    previous_digest: String,
    host: Arc<rsi_host::Host>,
    bytes: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}
struct Saved {
    principal: Principal,
    receipt: wire::Receipt,
    previous_digest: String,
}

pub(crate) struct Manager {
    source: Arc<source::Source>,
    context: Context,
    configuration: Arc<ConfigurationAccess>,
    administration: Arc<dyn rsi_api_protocol::DeviceAdministration>,
    epoch: HostEpoch,
    domain: Arc<dyn Domain>,
    state: Mutex<State>,
    slots: Arc<Semaphore>,
    writer: Arc<Semaphore>,
    grant_changes: Arc<Semaphore>,
    previews: Arc<Semaphore>,
    tasks: TaskTracker,
}
impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileLeafManager")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}
impl Manager {
    fn run<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Reply<T>>,
    ) -> rsi_api_protocol::Result<BoxFuture<'static, Reply<T>>> {
        self.run_with(&self.slots, work)
    }
    fn run_grant<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Reply<T>>,
    ) -> rsi_api_protocol::Result<BoxFuture<'static, Reply<T>>> {
        self.run_with(&self.grant_changes, work)
    }
    fn run_with<T: Send + 'static>(
        self: &Arc<Self>,
        slots: &Arc<Semaphore>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Reply<T>>,
    ) -> rsi_api_protocol::Result<BoxFuture<'static, Reply<T>>> {
        let state = self.state.lock().expect("Profile leaf state");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if state.uncertain {
            return Err(ApiError::OutcomeUnknown);
        }
        let permit = slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let future = work(self.clone());
        let task = self
            .context
            .runtime()
            .execution()
            .spawn(self.tasks.track_future(async move {
                let _permit = permit;
                future.await
            }));
        drop(state);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }
    fn human(
        &self,
        origin: &CallOrigin,
    ) -> rsi_api_protocol::Result<(Principal, rsi_configuration_access::ConfigurationLease)> {
        let lease = self.configuration.admit(origin)?;
        let principal = match origin {
            CallOrigin::Local => Principal::Local,
            CallOrigin::Device(device) => Principal::Device(device.id.clone()),
        };
        Ok((principal, lease))
    }
    fn scope(
        &self,
        principal: &Principal,
        target: &wire::Target,
        operation: wire::ChangeKind,
    ) -> Result<TaskTrackerToken, Failure> {
        let state = self.state.lock().expect("Profile leaf grants");
        let grant = Grant {
            principal: principal.clone(),
            target: target.clone(),
            operation,
        };
        if state.closed || state.uncertain {
            return Err(Failure::Unauthorized);
        }
        let gate = state
            .gates
            .get(&grant)
            .filter(|gate| gate.open)
            .ok_or(Failure::Unauthorized)?;
        Ok(gate.tasks.token())
    }
    async fn close(&self) {
        {
            let mut state = self.state.lock().expect("Profile leaf retirement");
            state.closed = true;
            for gate in state.gates.values_mut() {
                gate.close();
            }
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
