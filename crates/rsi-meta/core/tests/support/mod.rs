#![allow(dead_code)]

use async_trait::async_trait;
use rsi_meta::{
    Context, EventHandler, EventListenerId, EventOptions, EventOutcome, FiberHandle,
    InvocationContext, PluginDescriptor, PluginFactory, ProviderChannel, Result, ServiceEndpoint,
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

#[derive(Debug)]
pub(crate) struct PassiveFactory(pub(crate) PluginDescriptor);

#[async_trait]
impl PluginFactory for PassiveFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
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
    pub(crate) descriptor: PluginDescriptor,
    pub(crate) endpoint: Arc<dyn ServiceEndpoint>,
}

#[async_trait]
impl PluginFactory for EndpointFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let provision = &self.descriptor.provides[0];
        context.provide(
            provision.key.clone(),
            provision.contract.clone(),
            provision.version,
            Arc::clone(&self.endpoint),
        )
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
    pub(crate) descriptor: PluginDescriptor,
    pub(crate) context: Arc<Mutex<Option<Context>>>,
    pub(crate) listener: Arc<Mutex<Option<EventListenerId>>>,
    pub(crate) remove_while_staged: bool,
}

#[derive(Debug)]
pub(crate) struct ContextCaptureFactory {
    pub(crate) descriptor: PluginDescriptor,
    pub(crate) context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(context);
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for ListenerCaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let listener = context.on("authority", Arc::new(NoopHandler), EventOptions::default())?;
        if self.remove_while_staged {
            assert!(context.off(listener));
        }
        *self.context.lock().expect("context capture poisoned") = Some(context);
        *self.listener.lock().expect("listener capture poisoned") = Some(listener);
        Ok(())
    }
}
