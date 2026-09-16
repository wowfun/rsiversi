use super::*;
use async_trait::async_trait;
use rsi_agent_store_protocol::SessionStoreContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};

/// Local finite reference owner, shared by human admission and model reads.
#[derive(Debug)]
pub struct ReferencesContract;
impl LocalContract for ReferencesContract {
    const KEY: &'static str = "rsi.agent.references";
    type Service = References;
}
/// Ordinary Store-consuming reference service generation.
#[derive(Clone, Debug, Default)]
pub struct ReferencesFactory;
#[async_trait]
impl PluginFactory for ReferencesFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "References configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<SessionStoreContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let references = Arc::new(References::new(
            plan.local::<SessionStoreContract>()?,
            plan.context().runtime().execution().clone(),
        ));
        let cleanup = references.clone();
        plan.defer(
            "drain reference reads",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<ReferencesContract>(references)?;
        plan.defer(
            "withdraw reference reader",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
