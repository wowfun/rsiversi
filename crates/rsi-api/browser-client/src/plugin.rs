use crate::{BrowserClient, BrowserClientConfig};
use async_trait::async_trait;
use rsi_api_protocol::ApiClientContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::sync::{Arc, Mutex};
use tokio_util::task::TaskTracker;

/// Ordinary Meta owner of one same-origin browser connection generation.
#[derive(Clone, Debug, Default)]
pub struct BrowserClientFactory;
impl BrowserClientFactory {
    /// Starts negotiation under the invoking plugin's cleanup owner before publication.
    ///
    /// # Panics
    /// Panics if an earlier negotiation panic poisoned its retained connection slot.
    pub async fn connect_in(
        plan: &mut ActivationPlan,
        config: BrowserClientConfig,
    ) -> rsi_meta::Result<Arc<BrowserClient>> {
        let execution = plan.context().runtime().execution().clone();
        let transport = Arc::new(
            crate::transport::BrowserTransport::new(execution.clone(), &config)
                .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let tasks = TaskTracker::new();
        let published: Arc<Mutex<Option<Arc<BrowserClient>>>> = Arc::new(Mutex::new(None));
        let retiring = tasks.clone();
        let retained = published.clone();
        let cleanup = transport.clone();
        plan.defer(
            "cancel and drain browser connection startup",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.bridge.retire();
                    retiring.close();
                    retiring.wait().await;
                    let client = retained
                        .lock()
                        .expect("browser connection slot poisoned")
                        .take();
                    match client {
                        Some(client) => client.close().await,
                        None => cleanup.close().await,
                    }
                    .map_err(|error| error.to_string())
                })
            }),
        )?;
        let task_execution = execution.clone();
        execution
            .spawn(tasks.track_future(async move {
                let client =
                    Arc::new(BrowserClient::negotiate(task_execution, config, transport).await?);
                *published.lock().expect("browser connection slot poisoned") = Some(client.clone());
                Ok::<_, rsi_api_protocol::ApiError>(client)
            }))
            .await
            .map_err(|_| MetaError::Activation("browser negotiation task stopped".into()))?
            .map_err(|error| MetaError::Activation(error.to_string()))
    }
}
#[async_trait]
impl PluginFactory for BrowserClientFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: BrowserClientConfig = serde_json::from_value(desired.clone())
            .map_err(|_| MetaError::InvalidInput("invalid browser API configuration".into()))?;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            std::mem::size_of::<BrowserClientConfig>() + 32,
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<BrowserClientConfig>()?;
        let client = Self::connect_in(&mut plan, config).await?;
        let supply = plan.context().provide_local::<ApiClientContract>(client)?;
        plan.defer(
            "withdraw browser API client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
