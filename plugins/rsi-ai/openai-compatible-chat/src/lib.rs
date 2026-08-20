use std::sync::Arc;

use rsi_ai_meta::{
    PluginError, PluginMediaResolver, PluginProvider, PluginProviderFactory, ProviderPlugin,
    build_plugin_provider,
};
use rsi_ai_openai_compatible::{ChatCompletionsAdapter, ChatCompletionsConfig};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::{HttpTransport, ReqwestTransport};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct CompatibleFactory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_deployment")]
    deployment_id: String,
    endpoint: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default = "default_true")]
    allow_image_input: bool,
    api_key: String,
}

impl PluginProviderFactory for CompatibleFactory {
    fn build(
        &self,
        generation: u64,
        config: Value,
        media: PluginMediaResolver,
    ) -> Result<PluginProvider, PluginError> {
        let config: Config = serde_json::from_value(config)
            .map_err(|error| PluginError::context("invalid compatible Chat config", &error))?;
        let adapter_config = ChatCompletionsConfig::new(config.endpoint)
            .and_then(|value| value.with_path(config.path))
            .map(|value| value.with_image_input(config.allow_image_input))
            .map_err(|_| PluginError::new("invalid compatible Chat endpoint"))?;
        let transport: Arc<dyn HttpTransport> = Arc::new(
            ReqwestTransport::new()
                .map_err(|_| PluginError::new("cannot construct compatible Chat transport"))?,
        );
        let deployment_id = config.deployment_id.clone();
        build_plugin_provider(
            config.deployment_id,
            "compatible.api_key",
            config.api_key,
            media,
            move |requirement| {
                ProviderRegistration::builder(&deployment_id, "openai-compatible")
                    .map_err(|error| {
                        PluginError::context("invalid compatible Chat deployment id", &error)
                    })?
                    .with_protocol("chat-completions", "https-sse", "configured-endpoint")
                    .map_err(|error| {
                        PluginError::context("invalid compatible Chat protocol identity", &error)
                    })
                    .map(|builder| {
                        builder
                            .with_config_generation(generation)
                            .with_credential(requirement)
                            .with_language(ChatCompletionsAdapter::new(adapter_config, transport))
                    })?
                    .build()
                    .map_err(|error| {
                        PluginError::context("invalid compatible Chat registration", &error)
                    })
            },
        )
    }
}

fn default_deployment() -> String {
    "openai-compatible".to_owned()
}
fn default_path() -> String {
    "/v1/chat/completions".to_owned()
}
const fn default_true() -> bool {
    true
}

rsi_meta_plugin::export_plugin!(ProviderPlugin<CompatibleFactory>);

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
        let provider = CompatibleFactory
            .build(
                7,
                json!({"endpoint":"https://compatible.invalid", "api_key":"fixture-secret"}),
                PluginMediaResolver::default(),
            )
            .expect("provider");
        assert_eq!(provider.deployment_id, "openai-compatible");
        let model = || ModelRef::new("openai-compatible", "fixture-model").expect("model");
        assert!(provider.registry.language(model()).is_ok());
        assert!(provider.registry.image(model()).is_err());
        assert!(provider.registry.transcription(model()).is_err());
        assert!(provider.registry.speech(model()).is_err());
        assert!(provider.registry.realtime(model()).is_err());
    }

    #[test]
    fn factory_rejects_unknown_config_fields() {
        assert!(
            CompatibleFactory
                .build(
                    7,
                    json!({
                        "endpoint":"https://compatible.invalid",
                        "api_key":"fixture-secret",
                        "unknown":true
                    }),
                    PluginMediaResolver::default(),
                )
                .is_err()
        );
    }
}
