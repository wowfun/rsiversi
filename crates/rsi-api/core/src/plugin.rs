use crate::ApiRegistry;
use async_trait::async_trait;
use rsi_api_protocol::{ApiDispatchContract, ApiRegistrarContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;

/// Ordinary owner of a domain-independent API registry generation.
#[derive(Clone, Debug, Default)]
pub struct ApiFactory;
#[async_trait]
impl PluginFactory for ApiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "API registry configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registry = Arc::new(ApiRegistry::new(
            plan.context().runtime().execution().clone(),
        ));
        let cleanup = registry.clone();
        plan.defer(
            "drain API registry",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let registrar = plan
            .context()
            .provide_local::<ApiRegistrarContract>(registry.clone())?;
        let dispatch = plan
            .context()
            .provide_local::<ApiDispatchContract>(registry)?;
        plan.defer(
            "withdraw API registry",
            Box::new(move || {
                Box::pin(async move {
                    drop(dispatch);
                    drop(registrar);
                    Ok(())
                })
            }),
        )
    }
}
