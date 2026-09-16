use crate::{RetrievalConfig, RetrievalService, SETTINGS_NAMESPACE};
use async_trait::async_trait;
use rsi_credentials_protocol::CredentialsResolveContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_settings_protocol::{
    SettingsApply, SettingsContract, SettingsError, SettingsMetadata, SettingsSpec, ValidateWith,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Local retrieval service; arbitrary URLs never pass into generic Agent code.
#[derive(Debug)]
pub struct RetrievalContract;
impl LocalContract for RetrievalContract {
    const KEY: &'static str = "rsi.retrieval";
    type Service = RetrievalService;
}
/// Default-off Host retrieval owner with typed Settings and Credentials resolve.
#[derive(Debug, Default)]
pub struct RetrievalFactory;
#[async_trait]
impl PluginFactory for RetrievalFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Retrieval configuration belongs to rsi.retrieval Settings".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<SettingsContract>()
            .requiring_local::<CredentialsResolveContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registration = plan.local::<SettingsContract>()?.register(SettingsSpec {
            namespace: SETTINGS_NAMESPACE.into(),defaults:json!({"web_fetch":false,"web_search":false}),base:json!({}),
            metadata: SettingsMetadata { applies:SettingsApply::Live,description:"Enable public web Tools for new conversations. Existing conversations keep their saved definitions; disabling stops future calls immediately. Exa uses a separately stored credential. Saving settings does not submit any request.".into(),schema:json!({"type":"object","properties":{"web_fetch":{"type":"boolean","description":"Fetch public HTTP/S pages"},"web_search":{"type":"boolean","description":"Search Exa; requires its separate credential"}},"required":["web_fetch","web_search"],"additionalProperties":false}),sensitive_fields:vec![] },
            validator:Arc::new(ValidateWith(|value: &Value| serde_json::from_value::<RetrievalConfig>(value.clone()).map(|_|()).map_err(|_| SettingsError::InvalidInput("Retrieval settings require only web_fetch and web_search boolean flags".into())))),
        }).map_err(|error| MetaError::Activation(error.to_string()))?;
        let service = Arc::new(RetrievalService {
            dns: crate::network::system_resolver(),
            settings: registration.scope.clone(),
            credentials: plan.local::<CredentialsResolveContract>()?,
            permits: Arc::new(Semaphore::new(8)),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        let cleanup = service.clone();
        plan.defer(
            "retire retrieval work and settings",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.shutdown().await;
                    drop(registration);
                    Ok(())
                })
            }),
        )?;
        plan.context().provide_local::<RetrievalContract>(service)?;
        Ok(())
    }
}
