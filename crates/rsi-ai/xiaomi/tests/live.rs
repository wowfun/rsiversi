use std::{collections::BTreeMap, sync::Arc, time::Duration};

use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_protocol::{SpeechFormat, SpeechRequest, TranscriptionRequest};
use rsi_ai_provider::{MediaResolver, ProviderRegistration};
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use rsi_ai_xiaomi::{XiaomiConfig, XiaomiSpeechAdapter, XiaomiTranscriptionAdapter};

fn registry<R>(key: &str, endpoint: &str, media: R) -> Registry
where
    R: MediaResolver + 'static,
{
    let config = XiaomiConfig::new(endpoint).expect("endpoint");
    let transport = Arc::new(ReqwestTransport::new().expect("transport"));
    Registry::builder(
        CredentialManager::builder()
            .with_explicit("xiaomi.live", key)
            .expect("credential")
            .build(),
    )
    .with_media_resolver(media)
    .register(
        ProviderRegistration::builder("xiaomi-live", "xiaomi")
            .expect("registration")
            .with_credential(
                CredentialRequirement::new("xiaomi.live", std::iter::empty::<String>())
                    .expect("requirement"),
            )
            .with_transcription(XiaomiTranscriptionAdapter::new(
                config.clone(),
                transport.clone(),
            ))
            .with_speech(XiaomiSpeechAdapter::new(config, transport))
            .build()
            .expect("provider"),
    )
    .expect("register")
    .build()
    .expect("registry")
}

fn token_plan_origin() -> String {
    let base = std::env::var("XIAOMI_TOKEN_PLAN_BASE_URL")
        .unwrap_or_else(|_| "https://token-plan-cn.xiaomimimo.com/v1".to_owned());
    base.trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_owned()
}

#[tokio::test]
#[ignore = "requires an explicit XIAOMI_TOKEN_PLAN_API_KEY and spends live plan quota"]
async fn xiaomi_token_plan_synthesizes_then_transcribes_real_audio() {
    let key = std::env::var("XIAOMI_TOKEN_PLAN_API_KEY")
        .expect("XIAOMI_TOKEN_PLAN_API_KEY must be set explicitly");
    assert!(
        key.starts_with("tp-"),
        "Xiaomi Token Plan key has the wrong format"
    );
    let endpoint = token_plan_origin();

    let speech = tokio::time::timeout(
        Duration::from_mins(2),
        registry(&key, &endpoint, InMemoryMediaResolver::default())
            .speech(ModelRef::new("xiaomi-live", "mimo-v2.5-tts").expect("model"))
            .expect("speech")
            .synthesize(
                SpeechRequest::new("Hello world.", "mimo_default", SpeechFormat::Wav)
                    .expect("speech request"),
            ),
    )
    .await
    .expect("live Xiaomi TTS request timed out")
    .expect("live Xiaomi TTS");
    assert!(
        speech.audio.bytes.len() > 44,
        "Xiaomi TTS returned no WAV payload"
    );

    let descriptor = speech.audio.descriptor.clone();
    let media = InMemoryMediaResolver::new(BTreeMap::from([(
        descriptor.sha256().to_owned(),
        speech.audio.bytes,
    )]));
    let transcription = tokio::time::timeout(
        Duration::from_mins(2),
        registry(&key, &endpoint, media)
            .transcription(ModelRef::new("xiaomi-live", "mimo-v2.5-asr").expect("model"))
            .expect("transcription")
            .transcribe(
                TranscriptionRequest::new(descriptor)
                    .expect("transcription request")
                    .with_language("en")
                    .expect("transcription language"),
            ),
    )
    .await
    .expect("live Xiaomi ASR request timed out")
    .expect("live Xiaomi ASR");
    let transcript = transcription.text.to_ascii_lowercase();
    assert!(
        transcript.contains("hello"),
        "unexpected live transcript: {transcript:?}"
    );
    assert!(
        transcript.contains("world"),
        "unexpected live transcript: {transcript:?}"
    );
}
