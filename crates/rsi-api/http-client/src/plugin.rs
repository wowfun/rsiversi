use crate::{HttpClient, HttpClientConfig};
use async_trait::async_trait;
use rsi_api_protocol::ApiClientContract;
use rsi_credentials_protocol::CredentialsResolveContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;

/// Ordinary Meta owner of one authenticated native API connection generation.
#[derive(Clone, Debug, Default)]
pub struct HttpClientFactory;
#[async_trait]
impl PluginFactory for HttpClientFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: HttpClientConfig = serde_json::from_value(desired.clone())
            .map_err(|_| MetaError::InvalidInput("invalid API client configuration".into()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = std::mem::size_of::<HttpClientConfig>()
            + config.origin.len()
            + config.credential.owner.as_str().len()
            + config.credential.slot.len()
            + config
                .tls_ca
                .as_ref()
                .map_or(0, |path| path.as_os_str().len());
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring_local::<CredentialsResolveContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<HttpClientConfig>()?;
        let credentials = plan.local::<CredentialsResolveContract>()?;
        let token = credentials.resolve(&config.credential).await.map_err(|_| {
            MetaError::Activation("API device credential could not be resolved".into())
        })?;
        let client = Arc::new(
            HttpClient::connect(
                plan.context().runtime().execution().clone(),
                config,
                token.secret,
            )
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let cleanup = client.clone();
        plan.defer(
            "close API client",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan.context().provide_local::<ApiClientContract>(client)?;
        plan.defer(
            "withdraw API client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
