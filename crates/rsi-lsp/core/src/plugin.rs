use crate::{Config, LanguageService};
use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use std::sync::Arc;
/// Exact language provider generation, selected by ordinary Meta dependencies.
#[derive(Debug)]
pub struct LanguageContract;
impl LocalContract for LanguageContract {
    const KEY: &'static str = "rsi.lsp";
    type Service = LanguageService;
}
/// Operator-configured, lazy stdio language provider.
#[derive(Debug, Clone, Default)]
pub struct LanguageFactory;
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::InvalidInput(e.to_string())
}
#[async_trait]
impl PluginFactory for LanguageFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let value: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        value.validate().map_err(meta)?;
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<rsi_process::DuplexProcessContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>()
            .requiring_local::<rsi_files_protocol::FilesContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = LanguageService::new(
            serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?,
            plan.local::<rsi_process::DuplexProcessContract>()?,
            plan.local::<rsi_sandbox::SandboxContract>()?,
            plan.local::<rsi_files_protocol::FilesContract>()?,
            plan.context().runtime().execution().clone(),
        )
        .map_err(meta)?;
        let supply = plan
            .context()
            .provide_local::<LanguageContract>(Arc::clone(&service))?;
        plan.defer(
            "retire language servers and drain queries",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    service.close().await.map_err(|e| e.to_string())
                })
            }),
        )
    }
}
