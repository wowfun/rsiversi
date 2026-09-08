use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberHandle, PluginFactory, PreparedActivation,
    ResolvedFactory, UpdateMode,
};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct Capture(Arc<Mutex<Option<Context>>>);

#[async_trait]
impl PluginFactory for Capture {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        *self.0.lock().expect("capture lock") = Some(plan.context().clone());
        Ok(())
    }
}

/// Activates an ordinary child and returns its exact contribution context.
/// The caller owns explicit Fiber disposal and may select a parent position.
///
/// # Panics
/// Panics if the fixture capture mutex is poisoned or successful activation
/// did not execute its capture callback.
pub async fn activate_contribution_owner(
    parent: &Context,
) -> rsi_meta::Result<(FiberHandle, Context)> {
    let captured = Arc::new(Mutex::new(None));
    let fiber = parent
        .apply(
            ResolvedFactory::linked(
                "fixture.contribution",
                "1",
                UpdateMode::Replayable,
                Arc::new(Capture(captured.clone())),
            ),
            ConfigValue::Null,
        )
        .await?;
    let context = captured
        .lock()
        .expect("capture lock")
        .take()
        .expect("activation captured context");
    Ok((fiber, context))
}
