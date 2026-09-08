use crate::{OutputClient, register_output};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClientContract, ApiRegistrarContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::ProcessOutputCacheContract;
use std::sync::Arc;

fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() && !config.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(MetaError::InvalidInput(
            "Output API configuration must be null or empty".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Ordinary endpoint plugin consuming only the completed-output cache and registrar.
#[derive(Clone, Debug, Default)]
pub struct OutputApiFactory;
#[async_trait]
impl PluginFactory for OutputApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?
            .requiring_local::<ProcessOutputCacheContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let registration = register_output(
            registrar.as_ref(),
            plan.local::<ProcessOutputCacheContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Output API",
            Box::new(move || {
                Box::pin(async move {
                    registration.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// Ordinary client plugin publishing read-only output authority.
#[derive(Clone, Debug, Default)]
pub struct OutputClientFactory;
#[async_trait]
impl PluginFactory for OutputClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = OutputClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<ProcessOutputCacheContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Output client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
