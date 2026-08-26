use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, FactoryIdentity, InvocationContext, Message,
    PluginFactory, PreparedActivation, ProviderChannel, Requirement, Result, ServiceEndpoint,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct EchoEndpoint {
    overlays: Arc<Mutex<Vec<Vec<Value>>>>,
}

#[async_trait]
impl ServiceEndpoint for EchoEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        self.overlays
            .lock()
            .expect("overlay log poisoned")
            .push(invocation.edge_overlay().to_vec());
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ProviderFactory {
    identity: FactoryIdentity,
    pub(crate) overlays: Arc<Mutex<Vec<Vec<Value>>>>,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

impl ProviderFactory {
    pub(crate) fn new(cleanup: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            identity: FactoryIdentity::builtin("provider", "1"),
            overlays: Arc::new(Mutex::new(Vec::new())),
            cleanup,
        }
    }
}

#[async_trait]
impl PluginFactory for ProviderFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().provide(
            "echo",
            "test.echo",
            V1,
            Arc::new(EchoEndpoint {
                overlays: Arc::clone(&self.overlays),
            }),
        )?;
        let cleanup = Arc::clone(&self.cleanup);
        plan.context().defer(
            "provider",
            Box::new(move || {
                async move {
                    cleanup
                        .lock()
                        .expect("cleanup log poisoned")
                        .push("provider");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
#[allow(dead_code)] // Shared with the service-invariants target, not every importer.
pub(crate) struct ConsumerFactory {
    identity: FactoryIdentity,
    pub(crate) observed: Arc<Mutex<Vec<Vec<u8>>>>,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

#[allow(dead_code)]
impl ConsumerFactory {
    pub(crate) fn new(cleanup: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            identity: FactoryIdentity::builtin("consumer", "1"),
            observed: Arc::new(Mutex::new(Vec::new())),
            cleanup,
        }
    }
}

#[async_trait]
impl PluginFactory for ConsumerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "echo",
                "test.echo",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let service = plan
            .inject("echo")
            .expect("prepared echo requirement must be injected")
            .clone();
        let response = service.invoke(Message::new(b"active".to_vec())).await?;
        self.observed
            .lock()
            .expect("observation log poisoned")
            .push(response.into_parts().0);
        let cleanup = Arc::clone(&self.cleanup);
        plan.context().defer(
            "consumer",
            Box::new(move || {
                async move {
                    if !matches!(service.open(), Err(rsi_meta::MetaError::StaleCapability)) {
                        return Err(
                            "ordinary capability admission remained open during cleanup".to_owned()
                        );
                    }
                    cleanup
                        .lock()
                        .expect("cleanup log poisoned")
                        .push("consumer");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}
