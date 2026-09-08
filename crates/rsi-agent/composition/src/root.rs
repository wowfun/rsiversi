use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use std::sync::Arc;

/// Explicit service-local ancestry for generation builders; never exported to remote clients.
#[derive(Debug)]
pub struct AgentGenerationRootContract;
impl LocalContract for AgentGenerationRootContract {
    const KEY: &'static str = "rsi.agent.generation-root";
    type Service = Context;
}

/// Ordinary owner whose lifetime contains composition providers and their pinned generations.
#[derive(Clone, Debug, Default)]
pub struct AgentGenerationRootFactory;
#[async_trait]
impl PluginFactory for AgentGenerationRootFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Agent generation root configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<AgentGenerationRootContract>(Arc::new(plan.context().clone()))?;
        plan.defer(
            "withdraw Agent generation root",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
