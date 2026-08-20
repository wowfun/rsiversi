use rsi_ai::Registry;
use rsi_ai_auth::CredentialManager;
use rsi_ai_meta::{
    PluginError, PluginMediaResolver, PluginProvider, PluginProviderFactory, ProviderPlugin,
};
use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, FinishReason, ImageEvent, LanguageEvent, LanguageRequest,
    MessageContent, MessageRole, RealtimeCloseReason, RealtimeEvent, SpeechEvent,
    TranscriptionEvent,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_testkit::{
    FunctionalLanguageAdapter, ScriptedImageAdapter, ScriptedRealtimeAdapter,
    ScriptedSpeechAdapter, ScriptedTranscriptionAdapter,
};
use serde_json::Value;

const CONFORMANCE_PROMPT: &str = "Use the echo tool to repeat: hello";
const DIRECT_PROMPT: &str = "Answer directly with: ready";
const ECHO_CALL_ID: &str = "echo-call-1";
const ECHO_ARGUMENTS: &str = r#"{"text":"hello"}"#;

#[derive(Debug, Default)]
pub struct ScriptedFactory;

impl PluginProviderFactory for ScriptedFactory {
    fn build(
        &self,
        generation: u64,
        config: Value,
        media: PluginMediaResolver,
    ) -> Result<PluginProvider, PluginError> {
        if config != serde_json::json!({}) {
            return Err(PluginError::new("scripted model config must be empty"));
        }
        let registration = ProviderRegistration::builder("scripted-model", "fixture")
            .map_err(|_| PluginError::new("invalid scripted deployment"))?
            .with_protocol("rsi-ai-fixture", "memory", "fixture")
            .map_err(|_| PluginError::new("invalid scripted protocol"))?
            .with_config_generation(generation)
            .with_language(FunctionalLanguageAdapter::new(|request| script(&request)))
            .with_image(ScriptedImageAdapter::new(vec![
                ImageEvent::OutputStarted {
                    index: 0,
                    mime_type: "image/png".to_owned(),
                },
                ImageEvent::OutputChunk {
                    index: 0,
                    sequence: 1,
                    bytes: b"fixture-image".to_vec(),
                },
                ImageEvent::OutputFinished { index: 0 },
                ImageEvent::Finished,
            ]))
            .with_transcription(ScriptedTranscriptionAdapter::new(vec![
                TranscriptionEvent::TextDelta {
                    text: "fixture transcript".to_owned(),
                },
                TranscriptionEvent::Finished {
                    language: Some("en".to_owned()),
                },
            ]))
            .with_speech(ScriptedSpeechAdapter::new(vec![
                SpeechEvent::OutputStarted {
                    mime_type: "audio/wav".to_owned(),
                },
                SpeechEvent::AudioChunk {
                    sequence: 1,
                    bytes: b"fixture-speech".to_vec(),
                },
                SpeechEvent::OutputFinished,
                SpeechEvent::Finished,
            ]))
            .with_realtime(ScriptedRealtimeAdapter::new_after_request(vec![
                RealtimeEvent::SessionStarted {
                    session_id: "fixture-realtime".to_owned(),
                },
                RealtimeEvent::OutputTextDelta {
                    response_id: "response-1".to_owned(),
                    text: "live".to_owned(),
                },
                RealtimeEvent::OutputAudioChunk {
                    response_id: "response-1".to_owned(),
                    sequence: 1,
                    bytes: b"fixture-live-audio".to_vec(),
                },
                RealtimeEvent::Closed {
                    reason: RealtimeCloseReason::Provider,
                },
            ]))
            .build()
            .map_err(|_| PluginError::new("invalid scripted registration"))?;
        let registry = Registry::builder(CredentialManager::builder().build())
            .with_media_resolver(media)
            .register(registration)
            .map_err(|_| PluginError::new("cannot register scripted model"))?
            .build()
            .map_err(|_| PluginError::new("cannot build scripted registry"))?;
        Ok(PluginProvider {
            registry,
            deployment_id: "scripted-model".to_owned(),
        })
    }
}

fn script(request: &LanguageRequest) -> Result<Vec<LanguageEvent>, AiError> {
    if request
        .messages()
        .iter()
        .any(|message| message.role() == MessageRole::Tool)
    {
        return Ok(text("hello"));
    }
    let user = request
        .messages()
        .iter()
        .rev()
        .find(|message| message.role() == MessageRole::User)
        .and_then(|message| message.content().first())
        .and_then(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        });
    match user {
        Some(CONFORMANCE_PROMPT) => Ok(tool_call()),
        Some(DIRECT_PROMPT) => Ok(text("ready")),
        _ => Err(AiError::new(
            rsi_ai_protocol::ErrorKind::InvalidRequest,
            rsi_ai_protocol::ErrorPhase::Prepare,
            rsi_ai_protocol::DispatchStatus::NotStarted,
            "scripted model received an unexpected request",
        )
        .expect("static fixture error")),
    }
}

fn text(value: &str) -> Vec<LanguageEvent> {
    vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text(value.to_owned()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        },
    ]
}

fn tool_call() -> Vec<LanguageEvent> {
    vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::ToolCall {
                id: ECHO_CALL_ID.to_owned(),
                name: "echo".to_owned(),
            },
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::ToolArguments(ECHO_ARGUMENTS.to_owned()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::ToolCalls,
            replay: None,
        },
    ]
}

rsi_meta_plugin::export_plugin!(ProviderPlugin<ScriptedFactory>);

#[cfg(test)]
mod tests {
    use futures_util::StreamExt as _;
    use rsi_ai::ModelRef;
    use rsi_ai_meta::PluginProviderFactory as _;
    use rsi_ai_protocol::Message;

    use super::*;

    #[tokio::test]
    async fn fixture_runs_through_the_public_registry_and_stream_grammar() {
        let provider = ScriptedFactory
            .build(7, serde_json::json!({}), PluginMediaResolver::default())
            .expect("provider");
        let model = provider
            .registry
            .language(ModelRef::new("scripted-model", "fixture-model").expect("model ref"))
            .expect("language model");
        let request = LanguageRequest::new(vec![Message::user_text(DIRECT_PROMPT).expect("user")])
            .expect("request");
        let prepared = model.prepare(request).await.expect("prepare");
        assert_eq!(prepared.snapshot().config_generation, 7);
        let mut generation = prepared.start().await.expect("start");
        while generation.next().await.is_some() {}
        assert_eq!(generation.finish().expect("finish").visible_text(), "ready");
    }
}
