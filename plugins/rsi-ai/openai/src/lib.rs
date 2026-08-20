use std::sync::Arc;

use rsi_ai_meta::{
    PluginError, PluginMediaResolver, PluginProvider, PluginProviderFactory, ProviderPlugin,
    build_plugin_provider,
};
use rsi_ai_openai::{
    OpenAiConfig, OpenAiImageAdapter, OpenAiRealtimeAdapter, OpenAiResponsesAdapter,
    OpenAiSpeechAdapter, OpenAiTranscriptionAdapter,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::{HttpTransport, ReqwestTransport};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct OpenAiFactory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_deployment")]
    deployment_id: String,
    #[serde(default = "default_endpoint")]
    endpoint: String,
    api_key: String,
}

impl PluginProviderFactory for OpenAiFactory {
    fn build(
        &self,
        generation: u64,
        config: Value,
        media: PluginMediaResolver,
    ) -> Result<PluginProvider, PluginError> {
        let config: Config = serde_json::from_value(config)
            .map_err(|error| PluginError::context("invalid OpenAI config", &error))?;
        let endpoint = OpenAiConfig::new(config.endpoint)
            .map_err(|_| PluginError::new("invalid OpenAI endpoint"))?;
        let transport: Arc<dyn HttpTransport> = Arc::new(
            ReqwestTransport::new()
                .map_err(|_| PluginError::new("cannot construct OpenAI transport"))?,
        );
        let deployment_id = config.deployment_id.clone();
        build_plugin_provider(
            config.deployment_id,
            "openai.api_key",
            config.api_key,
            media,
            move |requirement| {
                ProviderRegistration::builder(&deployment_id, "openai")
                    .map_err(|error| PluginError::context("invalid OpenAI deployment id", &error))?
                    .with_protocol("openai-responses", "https-sse-ws", "configured-endpoint")
                    .map_err(|error| {
                        PluginError::context("invalid OpenAI protocol identity", &error)
                    })
                    .map(|builder| {
                        builder
                            .with_config_generation(generation)
                            .with_credential(requirement)
                            .with_language(OpenAiResponsesAdapter::new(
                                endpoint.clone(),
                                Arc::clone(&transport),
                            ))
                            .with_image(OpenAiImageAdapter::new(
                                endpoint.clone(),
                                Arc::clone(&transport),
                            ))
                            .with_transcription(OpenAiTranscriptionAdapter::new(
                                endpoint.clone(),
                                Arc::clone(&transport),
                            ))
                            .with_speech(OpenAiSpeechAdapter::new(endpoint.clone(), transport))
                            .with_realtime(OpenAiRealtimeAdapter::production(endpoint))
                    })?
                    .build()
                    .map_err(|error| PluginError::context("invalid OpenAI registration", &error))
            },
        )
    }
}

fn default_deployment() -> String {
    "openai".to_owned()
}
fn default_endpoint() -> String {
    "https://api.openai.com".to_owned()
}

rsi_meta_plugin::export_plugin!(ProviderPlugin<OpenAiFactory>);

#[cfg(test)]
mod tests {
    use rsi_ai::ModelRef;
    use serde_json::json;

    use super::*;

    #[test]
    fn factory_builds_exactly_the_five_declared_capabilities_without_io() {
        assert!(include_str!("../plugin.toml").lines().any(|line| line
            == "provides = [\"rsi.ai.language\", \"rsi.ai.image\", \"rsi.ai.transcription\", \"rsi.ai.speech\", \"rsi.ai.realtime\"]"));
        let provider = OpenAiFactory
            .build(
                7,
                json!({"api_key":"fixture-secret"}),
                PluginMediaResolver::default(),
            )
            .expect("provider");
        assert_eq!(provider.deployment_id, "openai");
        let model = || ModelRef::new("openai", "fixture-model").expect("model");
        assert!(provider.registry.language(model()).is_ok());
        assert!(provider.registry.image(model()).is_ok());
        assert!(provider.registry.transcription(model()).is_ok());
        assert!(provider.registry.speech(model()).is_ok());
        assert!(provider.registry.realtime(model()).is_ok());
    }

    #[test]
    fn factory_rejects_unknown_config_fields() {
        assert!(
            OpenAiFactory
                .build(
                    7,
                    json!({"api_key":"fixture-secret", "unknown":true}),
                    PluginMediaResolver::default(),
                )
                .is_err()
        );
    }
}
