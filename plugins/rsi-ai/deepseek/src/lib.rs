use std::sync::Arc;

use rsi_ai_deepseek::{DeepSeekAdapter, DeepSeekConfig};
use rsi_ai_meta::{
    PluginError, PluginMediaResolver, PluginProvider, PluginProviderFactory, ProviderPlugin,
    build_plugin_provider,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::{HttpTransport, ReqwestTransport};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct DeepSeekFactory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_deployment")]
    deployment_id: String,
    #[serde(default)]
    endpoint: Option<String>,
    api_key: String,
}

impl PluginProviderFactory for DeepSeekFactory {
    fn build(
        &self,
        generation: u64,
        config: Value,
        media: PluginMediaResolver,
    ) -> Result<PluginProvider, PluginError> {
        let config: Config = serde_json::from_value(config)
            .map_err(|error| PluginError::context("invalid DeepSeek config", &error))?;
        let adapter_config = match config.endpoint {
            Some(endpoint) => DeepSeekConfig::with_endpoint(endpoint)
                .map_err(|_| PluginError::new("invalid DeepSeek endpoint"))?,
            None => DeepSeekConfig::default(),
        };
        let transport: Arc<dyn HttpTransport> = Arc::new(
            ReqwestTransport::new()
                .map_err(|_| PluginError::new("cannot construct DeepSeek transport"))?,
        );
        let adapter = DeepSeekAdapter::new(adapter_config, transport)
            .map_err(|_| PluginError::new("invalid DeepSeek adapter"))?;
        let deployment_id = config.deployment_id.clone();
        build_plugin_provider(
            config.deployment_id,
            "deepseek.api_key",
            config.api_key,
            media,
            move |requirement| {
                ProviderRegistration::builder(&deployment_id, "deepseek")
                    .map_err(|error| {
                        PluginError::context("invalid DeepSeek deployment id", &error)
                    })?
                    .with_protocol("deepseek-chat", "https-sse", "configured-endpoint")
                    .map_err(|error| {
                        PluginError::context("invalid DeepSeek protocol identity", &error)
                    })
                    .map(|builder| {
                        builder
                            .with_config_generation(generation)
                            .with_credential(requirement)
                            .with_language(adapter)
                    })?
                    .build()
                    .map_err(|error| PluginError::context("invalid DeepSeek registration", &error))
            },
        )
    }
}

fn default_deployment() -> String {
    "deepseek".to_owned()
}

rsi_meta_plugin::export_plugin!(ProviderPlugin<DeepSeekFactory>);

#[cfg(test)]
mod tests {
    use rsi_ai::ModelRef;
    use serde_json::json;

    use super::*;

    #[test]
    fn factory_builds_only_the_declared_language_capability_without_io() {
        assert!(
            include_str!("../plugin.toml")
                .lines()
                .any(|line| line == "provides = [\"rsi.ai.language\"]")
        );
        let provider = DeepSeekFactory
            .build(
                7,
                json!({"api_key":"fixture-secret"}),
                PluginMediaResolver::default(),
            )
            .expect("provider");
        assert_eq!(provider.deployment_id, "deepseek");
        let model = || ModelRef::new("deepseek", "fixture-model").expect("model");
        assert!(provider.registry.language(model()).is_ok());
        assert!(provider.registry.image(model()).is_err());
        assert!(provider.registry.transcription(model()).is_err());
        assert!(provider.registry.speech(model()).is_err());
        assert!(provider.registry.realtime(model()).is_err());
    }

    #[test]
    fn factory_rejects_unknown_config_fields() {
        let error = DeepSeekFactory
            .build(
                7,
                json!({"api_key":"fixture-secret", "unknown":true}),
                PluginMediaResolver::default(),
            )
            .expect_err("unknown config field");
        let message = error.to_string();
        assert!(message.contains("unknown field"));
        assert!(!message.contains("fixture-secret"));
    }
}
