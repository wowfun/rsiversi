use super::ConnectionFactory;
use async_trait::async_trait;
use rsi_api_protocol::ApiClientContract;
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct OperatorFactory(pub(super) Arc<ConnectionFactory>);
#[async_trait]
impl PluginFactory for OperatorFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(self
                .0
                .diagnosed("local operator connection configuration must be null"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.activate_inner(plan)
            .await
            .map_err(|error| self.0.diagnosed(error))
    }
}
impl OperatorFactory {
    async fn activate_inner(&self, plan: ActivationPlan) -> Result<(), MetaError> {
        let convert = |error: String| MetaError::Activation(error);
        let paths = rsi_service_host::ServiceHostPaths::from_host_paths(self.0.composition.paths())
            .map_err(|error| convert(error.to_string()))?;
        let owner = paths
            .read_metadata()
            .map_err(|error| convert(error.to_string()))?
            .ok_or_else(|| {
                convert("device administration requires a running local Service Host".into())
            })?;
        if !rsi_service_host::owner_process_is_current(&owner)
            .map_err(|error| convert(error.to_string()))?
        {
            return Err(convert(
                "the recorded Service Host is no longer running".into(),
            ));
        }
        let config = crate::client_composition::configuration(&owner)
            .map_err(|error| convert(error.to_string()))?;
        let mut builder = HostBuilder::without_paths("native");
        builder
            .register_local_contract::<ApiClientContract>()
            .map_err(|error| convert(error.to_string()))?;
        builder
            .register_linked(
                "rsi.connection.local",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(rsi_api_uds_client::UdsClientFactory),
            )
            .map_err(|error| convert(error.to_string()))?;
        let connection = crate::ProfileOwner::start_scoped(
            builder
                .build()
                .map_err(|error| convert(error.to_string()))?,
            self.0.composition.paths().clone(),
            plan.context(),
            ProfileProgram::from_profile(Profile::new(vec![ProfileEntry::new(
                "connection",
                "rsi.connection.local",
                serde_json::to_value(config).map_err(|error| convert(error.to_string()))?,
            )])),
        )
        .await
        .map_err(|error| convert(error.to_string()))?;
        let api = connection.lookup_local::<ApiClientContract>();
        plan.defer(
            "close operator connection",
            Box::new(move || {
                Box::pin(async move {
                    if connection.shutdown().await.is_clean() {
                        Ok(())
                    } else {
                        Err("operator connection cleanup failed".into())
                    }
                })
            }),
        )?;
        let api =
            api.ok_or_else(|| convert("operator Profile did not publish an API client".into()))?;
        let supply = plan.context().provide_local::<ApiClientContract>(api)?;
        plan.defer(
            "withdraw operator connection",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
