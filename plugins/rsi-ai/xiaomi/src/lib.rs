use std::sync::Arc;

use rsi_ai_meta::{
    PluginError, PluginMediaResolver, PluginProvider, PluginProviderFactory, ProviderPlugin,
    build_plugin_provider,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::{HttpTransport, ReqwestTransport};
use rsi_ai_xiaomi::{XiaomiConfig, XiaomiSpeechAdapter, XiaomiTranscriptionAdapter};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct XiaomiFactory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_deployment")]
    deployment_id: String,
    #[serde(default)]
    endpoint: Option<String>,
    api_key: String,
}

impl PluginProviderFactory for XiaomiFactory {
    fn build(
        &self,
        generation: u64,
        config: Value,
        media: PluginMediaResolver,
    ) -> Result<PluginProvider, PluginError> {
        let config: Config = serde_json::from_value(config)
            .map_err(|error| PluginError::context("invalid Xiaomi config", &error))?;
        let adapter_config = match config.endpoint {
            Some(endpoint) => XiaomiConfig::new(endpoint)
                .map_err(|_| PluginError::new("invalid Xiaomi endpoint"))?,
            None => XiaomiConfig::default(),
        };
        let transport: Arc<dyn HttpTransport> = Arc::new(
            ReqwestTransport::new()
                .map_err(|_| PluginError::new("cannot construct Xiaomi transport"))?,
        );
        let deployment_id = config.deployment_id.clone();
        build_plugin_provider(
            config.deployment_id,
            "xiaomi.api_key",
            config.api_key,
            media,
            move |requirement| {
                ProviderRegistration::builder(&deployment_id, "xiaomi")
                    .map_err(|error| PluginError::context("invalid Xiaomi deployment id", &error))?
                    .with_protocol("mimo-chat-audio", "https-sse", "configured-endpoint")
                    .map_err(|error| {
                        PluginError::context("invalid Xiaomi protocol identity", &error)
                    })
                    .map(|builder| {
                        builder
                            .with_config_generation(generation)
                            .with_credential(requirement)
                            .with_transcription(XiaomiTranscriptionAdapter::new(
                                adapter_config.clone(),
                                Arc::clone(&transport),
                            ))
                            .with_speech(XiaomiSpeechAdapter::new(adapter_config, transport))
                    })?
                    .build()
                    .map_err(|error| PluginError::context("invalid Xiaomi registration", &error))
            },
        )
    }
}

fn default_deployment() -> String {
    "xiaomi".to_owned()
}

rsi_meta_plugin::export_plugin!(ProviderPlugin<XiaomiFactory>);

#[cfg(test)]
mod tests {
    use rsi_ai::ModelRef;
    use serde_json::json;

    use super::*;

    #[test]
    fn factory_builds_only_the_two_declared_audio_capabilities_without_io() {
        assert!(
            include_str!("../plugin.toml")
                .lines()
                .any(|line| line == "provides = [\"rsi.ai.transcription\", \"rsi.ai.speech\"]")
        );
        let provider = XiaomiFactory
            .build(
                7,
                json!({"api_key":"fixture-secret"}),
                PluginMediaResolver::default(),
            )
            .expect("provider");
        assert_eq!(provider.deployment_id, "xiaomi");
        let model = || ModelRef::new("xiaomi", "fixture-model").expect("model");
        assert!(provider.registry.language(model()).is_err());
        assert!(provider.registry.image(model()).is_err());
        assert!(provider.registry.transcription(model()).is_ok());
        assert!(provider.registry.speech(model()).is_ok());
        assert!(provider.registry.realtime(model()).is_err());
    }

    #[test]
    fn factory_rejects_unknown_config_fields() {
        assert!(
            XiaomiFactory
                .build(
                    7,
                    json!({"api_key":"fixture-secret", "unknown":true}),
                    PluginMediaResolver::default(),
                )
                .is_err()
        );
    }
}
