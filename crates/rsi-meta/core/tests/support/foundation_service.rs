use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    Context, ContractVersion, FactoryIdentity, InvocationContext, PluginDescriptor, PluginFactory,
    ProviderChannel, Provision, Requirement, Result, ServiceEndpoint, ServiceFrame,
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
    descriptor: PluginDescriptor,
    pub(crate) overlays: Arc<Mutex<Vec<Vec<Value>>>>,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

impl ProviderFactory {
    pub(crate) fn new(cleanup: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("provider", "1"))
                .providing(Provision::new("echo", "test.echo", V1)),
            overlays: Arc::new(Mutex::new(Vec::new())),
            cleanup,
        }
    }
}

#[async_trait]
impl PluginFactory for ProviderFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.provide(
            "echo",
            "test.echo",
            V1,
            Arc::new(EchoEndpoint {
                overlays: Arc::clone(&self.overlays),
            }),
        )?;
        let cleanup = Arc::clone(&self.cleanup);
        context.defer(
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
    descriptor: PluginDescriptor,
    pub(crate) observed: Arc<Mutex<Vec<Vec<u8>>>>,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

#[allow(dead_code)]
impl ConsumerFactory {
    pub(crate) fn new(cleanup: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("consumer", "1"))
                .requiring(Requirement::new("echo", "test.echo", V1)),
            observed: Arc::new(Mutex::new(Vec::new())),
            cleanup,
        }
    }
}

#[async_trait]
impl PluginFactory for ConsumerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let service = context.service("echo")?;
        let response = service
            .open()?
            .unary(ServiceFrame::new(b"active".to_vec()))
            .await?;
        self.observed
            .lock()
            .expect("observation log poisoned")
            .push(response.into_bytes());
        let cleanup = Arc::clone(&self.cleanup);
        context.defer(
            "consumer",
            Box::new(move || {
                async move {
                    let response = service
                        .open()
                        .map_err(|error| error.to_string())?
                        .unary(ServiceFrame::new(b"cleanup".to_vec()))
                        .await
                        .map_err(|error| error.to_string())?;
                    if response.as_bytes() != b"cleanup" {
                        return Err("cleanup call returned wrong bytes".to_owned());
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
