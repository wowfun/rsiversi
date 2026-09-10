use crate::{SessionFilesApi, SessionFilesClient, SessionFilesContract};
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClientContract, ApiDispatchContract, ApiRegistrarContract, ConnectionDescriptionContract,
};
use rsi_files_protocol::FilesContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_session_protocol::SessionReadContract;
use std::sync::Arc;
fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() {
        return Err(MetaError::InvalidInput(
            "Session Files configuration must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Ordinary authenticated endpoint plugin with exact Session/reader dependencies.
#[derive(Clone, Debug, Default)]
pub struct SessionFilesApiFactory;
#[async_trait]
impl PluginFactory for SessionFilesApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<ApiDispatchContract>()
            .requiring_local::<ConnectionDescriptionContract>()
            .requiring_local::<SessionReadContract>()
            .requiring_local::<FilesContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let api = SessionFilesApi::register(
            registrar.as_ref(),
            plan.local::<SessionReadContract>()?,
            plan.local::<FilesContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Session Files API",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )?;
        let client = crate::local::client(
            plan.local::<ApiDispatchContract>()?,
            plan.local::<ConnectionDescriptionContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<SessionFilesContract>(Arc::new(client))?;
        plan.defer(
            "withdraw local Session Files client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
/// Ordinary shared client plugin; it receives no native filesystem authority.
#[derive(Clone, Debug, Default)]
pub struct SessionFilesClientFactory;
#[async_trait]
impl PluginFactory for SessionFilesClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = SessionFilesClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<SessionFilesContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Session Files client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
