use crate::{
    ActivationPlan, ConfigValue, Context, EventHandler, EventOutcome, FactoryIdentity,
    InvocationContext, PluginFactory, PreparedActivation, Result,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(super) struct ContextFactory {
    pub(super) identity: FactoryIdentity,
    pub(super) context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct CountHandler(pub(super) Arc<AtomicUsize>);

#[async_trait]
impl EventHandler for CountHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}
