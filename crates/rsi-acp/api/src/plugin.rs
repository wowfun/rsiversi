use crate::{Client, Endpoint};
use async_trait::async_trait;
use rsi_acp_protocol::service::ExternalConversationsContract;
use rsi_api_protocol::{ApiClientContract, ApiRegistrarContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;
fn prepare(value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !value.is_null() {
        return Err(MetaError::InvalidInput(
            "external-conversation API configuration must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Ordinary authenticated API endpoint plugin over the Host-owned service.
#[derive(Debug, Default)]
pub struct EndpointFactory;
#[async_trait]
impl PluginFactory for EndpointFactory {
    fn prepare(&self, value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(value)?
            .requiring_local::<ExternalConversationsContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let endpoint = Endpoint::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            &plan.local::<ExternalConversationsContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw external-conversation API",
            Box::new(move || {
                Box::pin(async move {
                    endpoint.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// Ordinary domain proxy; it owns no subprocess, endpoint configuration or journal.
#[derive(Debug, Default)]
pub struct ClientFactory;
#[async_trait]
impl PluginFactory for ClientFactory {
    fn prepare(&self, value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(value)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = Client::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.context()
            .provide_local::<ExternalConversationsContract>(Arc::new(client))?;
        Ok(())
    }
}
