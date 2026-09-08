use crate::{SessionApi, SessionClient};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClientContract, ApiRegistrarContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_session_protocol::{SessionContract, SessionIngressContract};
use std::sync::Arc;

fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() && !config.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(MetaError::InvalidInput(
            "Session API configuration must be null or empty".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Ordinary endpoint plugin consuming the shared Session service and trusted ingress.
#[derive(Clone, Debug, Default)]
pub struct SessionApiFactory;
#[async_trait]
impl PluginFactory for SessionApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?
            .requiring_local::<SessionContract>()
            .requiring_local::<SessionIngressContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let api = SessionApi::register(
            registrar.as_ref(),
            plan.local::<SessionContract>()?,
            plan.local::<SessionIngressContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Session API",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// Ordinary client plugin publishing Session without server ingress authority.
#[derive(Clone, Debug, Default)]
pub struct SessionClientFactory;
#[async_trait]
impl PluginFactory for SessionClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = SessionClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<SessionContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Session client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
