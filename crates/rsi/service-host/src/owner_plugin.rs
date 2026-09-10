use crate::{HostOwnerLease, ServiceHostPaths};
use async_trait::async_trait;
use rsi_api_protocol::{HostEpoch, HostGenerationContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use std::sync::Arc;

/// Local native ownership authority; never exported to remote clients.
#[derive(Debug)]
pub struct ServiceOwnerContract;
impl LocalContract for ServiceOwnerContract {
    const KEY: &'static str = "rsi.service.owner";
    type Service = HostOwnerLease;
}

/// Ordinary native ownership publisher, activated before durable service providers.
#[derive(Clone, Debug)]
pub struct ServiceOwnerFactory(Ownership);
#[derive(Clone, Debug)]
enum Ownership {
    Existing(Arc<HostOwnerLease>, HostEpoch),
    Acquire(ServiceHostPaths),
}
impl ServiceOwnerFactory {
    /// Pins the exact generation already selected by native process startup.
    pub fn new(owner: Arc<HostOwnerLease>, epoch: HostEpoch) -> Self {
        Self(Ownership::Existing(owner, epoch))
    }
    /// Defers acquisition of explicit paths until activation; preparation is inert.
    pub fn acquiring(paths: ServiceHostPaths) -> Self {
        Self(Ownership::Acquire(paths))
    }
}
#[async_trait]
impl PluginFactory for ServiceOwnerFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "service owner configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (lease, epoch) = match &self.0 {
            Ownership::Existing(owner, epoch) => (owner.clone(), epoch.clone()),
            Ownership::Acquire(paths) => {
                let owner = HostOwnerLease::try_acquire(paths.clone())
                    .map_err(|error| MetaError::Activation(error.to_string()))?;
                let epoch = HostEpoch::generate()
                    .map_err(|error| MetaError::Activation(error.to_string()))?;
                (Arc::new(owner), epoch)
            }
        };
        let generation = plan
            .context()
            .provide_local::<HostGenerationContract>(Arc::new(epoch))?;
        let owner = plan
            .context()
            .provide_local::<ServiceOwnerContract>(lease)?;
        plan.defer(
            "release service owner",
            Box::new(move || {
                Box::pin(async move {
                    drop((generation, owner));
                    Ok(())
                })
            }),
        )
    }
}
