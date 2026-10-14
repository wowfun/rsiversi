use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};

#[derive(Debug, Default)]
pub(crate) struct McpApiFactory;

fn unsupported() -> MetaError {
    MetaError::Activation("MCP management requires native Unix Profile authority".into())
}

#[async_trait]
impl PluginFactory for McpApiFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Err(unsupported())
    }

    async fn activate(&self, _: ActivationPlan) -> rsi_meta::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn unsupported_management_never_enters_the_runtime() {
        let runtime = rsi_meta::Runtime::default();
        let result = runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "rsi.mcp.api",
                    "test",
                    rsi_meta::UpdateMode::RestartRequired,
                    std::sync::Arc::new(super::McpApiFactory),
                ),
                rsi_meta::ConfigValue::Null,
            )
            .await;
        assert!(matches!(result, Err(rsi_meta::MetaError::Activation(_))));
        assert!(runtime.snapshot().fibers.is_empty());
        assert!(runtime.shutdown().await.is_clean());
    }
}
