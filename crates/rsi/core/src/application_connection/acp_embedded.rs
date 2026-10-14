//! Private Windows ACP Service ownership; stdio stays with the application.
use super::ConnectionFactory;
use crate::application_services::{ServingService, ServingServiceContract};
use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct Factory(pub Arc<ConnectionFactory>);
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.0.prepare(desired)
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (profile, composition) = self.0.compose(&mut plan).await?;
        let running = Arc::new(
            crate::RunningRsi::boot_host_profile_in(composition, &profile, plan.context())
                .await
                .map_err(|error| self.0.diagnosed(error))?,
        );
        let (closed, stopped) = tokio::sync::watch::channel(false);
        let service = Arc::new(Service {
            running: running.clone(),
            stopped,
        });
        let owned = running.clone();
        plan.defer(
            "drain embedded ACP Service",
            Box::new(move || {
                Box::pin(async move {
                    let outcome = owned.shutdown().await;
                    closed.send_replace(true);
                    if outcome.is_clean() {
                        Ok(())
                    } else {
                        Err("embedded ACP Service cleanup failed".into())
                    }
                })
            }),
        )?;
        let backend =
            crate::acp_owner::backend(&running).map_err(|error| self.0.diagnosed(error))?;
        let supply = plan
            .context()
            .provide_local::<rsi_acp_agent::AgentBackendContract>(backend.clone())?;
        plan.defer(
            "close embedded ACP backend",
            Box::new(move || {
                Box::pin(async move {
                    let result = backend
                        .shutdown()
                        .await
                        .map_err(|_| "embedded ACP backend cleanup failed".to_owned());
                    drop(supply);
                    result
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<ServingServiceContract>(service)?;
        plan.defer(
            "withdraw embedded ACP Service",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Service {
    running: Arc<crate::RunningRsi>,
    stopped: tokio::sync::watch::Receiver<bool>,
}
#[async_trait]
impl ServingService for Service {
    async fn stopped(&self) -> Result<(), String> {
        let mut stopped = self.stopped.clone();
        loop {
            if *stopped.borrow_and_update() {
                return Ok(());
            }
            if stopped.changed().await.is_err() {
                return Err("embedded ACP Service owner ended".into());
            }
        }
    }
    async fn reload(&self) -> Result<(), String> {
        self.running
            .reload()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
