#![allow(dead_code)]

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, ContractId, ContractVersion, EventHandle, EventHandler,
    EventOptions, EventOutcome, FactoryIdentity, FiberHandle, InvocationContext, PluginFactory,
    PreparedActivation, ProviderChannel, Requirement, Result, ServiceEndpoint, ServiceKey,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub(crate) async fn wait_active(handle: &FiberHandle) {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handle.wait_active(&CancellationToken::new()),
    )
    .await
    .expect("fiber activation timed out")
    .expect("fiber should activate");
}

#[derive(Clone, Debug)]
pub(crate) struct FactorySpec {
    identity: FactoryIdentity,
    requirements: Vec<Requirement>,
}

impl FactorySpec {
    pub(crate) fn new(identity: FactoryIdentity) -> Self {
        Self {
            identity,
            requirements: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn requiring(mut self, requirement: Requirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    pub(crate) fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    // The helper deliberately preserves PluginFactory's fallible seam so test
    // factories can delegate the complete method without adapter boilerplate.
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(self.requirements.iter().cloned().fold(
            PreparedActivation::new(desired.clone()),
            PreparedActivation::requiring,
        ))
    }
}

#[derive(Debug)]
pub(crate) struct PassiveFactory(pub(crate) FactorySpec);

impl PassiveFactory {
    pub(crate) fn new(identity: FactoryIdentity) -> Self {
        Self(FactorySpec::new(identity))
    }

    #[must_use]
    pub(crate) fn requiring(mut self, requirement: Requirement) -> Self {
        self.0 = self.0.requiring(requirement);
        self
    }
}

#[async_trait]
impl PluginFactory for PassiveFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Echo;

#[async_trait]
impl ServiceEndpoint for Echo {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct EndpointFactory {
    spec: FactorySpec,
    key: ServiceKey,
    contract: ContractId,
    version: ContractVersion,
    endpoint: Arc<dyn ServiceEndpoint>,
}

impl EndpointFactory {
    pub(crate) fn new(
        identity: FactoryIdentity,
        key: impl Into<ServiceKey>,
        contract: impl Into<ContractId>,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Self {
        Self {
            spec: FactorySpec::new(identity),
            key: key.into(),
            contract: contract.into(),
            version,
            endpoint,
        }
    }

    #[must_use]
    pub(crate) fn requiring(mut self, requirement: Requirement) -> Self {
        self.spec = self.spec.requiring(requirement);
        self
    }
}

#[async_trait]
impl PluginFactory for EndpointFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().provide(
            self.key.clone(),
            self.contract.clone(),
            self.version,
            Arc::clone(&self.endpoint),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct NoopHandler;

#[async_trait]
impl EventHandler for NoopHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
pub(crate) struct ListenerCaptureFactory {
    pub(crate) spec: FactorySpec,
    pub(crate) context: Arc<Mutex<Option<Context>>>,
    pub(crate) listener: Arc<Mutex<Option<EventHandle>>>,
    pub(crate) dispose_during_activation: bool,
}

#[derive(Debug)]
pub(crate) struct ContextCaptureFactory {
    pub(crate) spec: FactorySpec,
    pub(crate) context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        *self.context.lock().expect("context capture poisoned") = Some(context);
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for ListenerCaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let listener = context.on("authority", Arc::new(NoopHandler), EventOptions::default())?;
        if self.dispose_during_activation {
            assert!(listener.dispose().await.is_clean());
        }
        *self.context.lock().expect("context capture poisoned") = Some(context);
        *self.listener.lock().expect("listener capture poisoned") = Some(listener);
        Ok(())
    }
}
