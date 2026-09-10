use crate::{MediaApi, MediaClient};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClientContract, ApiRegistrarContract};
use rsi_media_protocol::MediaContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;

fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() && !config.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(MetaError::InvalidInput(
            "Media API configuration must be null or empty".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Ordinary endpoint plugin over independent Media and API registrar capabilities.
#[derive(Clone, Debug, Default)]
pub struct MediaApiFactory;
#[async_trait]
impl PluginFactory for MediaApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?
            .requiring_local::<MediaContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let api = MediaApi::register(registrar.as_ref(), plan.local::<MediaContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Media API",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// Ordinary client plugin publishing canonical Media without backend authority.
#[derive(Clone, Debug, Default)]
pub struct MediaClientFactory;
#[async_trait]
impl PluginFactory for MediaClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = MediaClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<MediaContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Media client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
