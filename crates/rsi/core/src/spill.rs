//! Standard cache partition selected from the acquired persistent-home authority.
use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) struct StandardSpillFactory(pub PathBuf);
#[async_trait]
impl PluginFactory for StandardSpillFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "standard Spill config must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<rsi_service_host::ServiceOwnerContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let authority = plan
            .local::<rsi_service_host::ServiceOwnerContract>()?
            .paths()
            .home_authority()
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        rsi_spill_local::activate_local_store(
            plan,
            rsi_spill_local::SpillLocalConfig::new(self.0.join("spill").join(authority)),
        )
        .await
    }
}
