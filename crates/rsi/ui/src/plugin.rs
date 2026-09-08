use crate::{TargetKind, Ui, UiContract, UiTargetContract};
use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::Arc;

/// Ordinary application-local UI registry factory.
#[derive(Clone, Debug, Default)]
pub struct UiFactory;
#[async_trait]
impl PluginFactory for UiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "UI registry configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let ui = Arc::new(Ui::new(plan.context()).map_err(crate::registry::meta)?);
        let drain = ui.clone();
        plan.defer(
            "drain UI registry",
            Box::new(move || {
                Box::pin(async move {
                    drain.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan.context().provide_local::<UiContract>(ui.clone())?;
        plan.defer(
            "withdraw UI registry",
            Box::new(move || {
                Box::pin(async move {
                    ui.retire();
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
/// Publishes the actual Context as a target inside its existing Local mappings.
#[derive(Clone, Debug, Default)]
pub struct UiTargetFactory;
#[async_trait]
impl PluginFactory for UiTargetFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let kind: TargetKind = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(
            PreparedActivation::with_state(
                desired.clone(),
                kind,
                std::mem::size_of::<TargetKind>(),
            )
            .requiring_local::<UiContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let kind = plan.take_state::<TargetKind>()?;
        let ui = plan.local::<UiContract>()?;
        let (target, lease) = ui
            .register_target(&plan, kind)
            .map_err(crate::registry::meta)?;
        let supply = plan.context().provide_local::<UiTargetContract>(target)?;
        plan.defer(
            "withdraw UI target capability",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("UI target registration cleanup failed".into())
                    }
                })
            }),
        )
    }
}
