use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axum::{Router, body::Bytes, http::HeaderMap, response::Response, routing::post};
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_protocol::{
    MediaDescriptor, MediaKind, SpeechFormat, SpeechRequest, TranscriptionRequest,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use rsi_ai_xiaomi::{XiaomiConfig, XiaomiSpeechAdapter, XiaomiTranscriptionAdapter};
use serde_json::Value;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

async fn audio(
    capture: axum::extract::State<Capture>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let value: Value = serde_json::from_slice(&body).expect("request JSON");
    capture
        .0
        .0
        .lock()
        .expect("capture")
        .push((headers, value.clone()));
    let model = value["model"].as_str().expect("model");
    let body = if model.contains("asr") {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"world\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"audio\":{\"data\":\"AAE=\"}},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\n",
            "data: {\"choices\":[{\"delta\":{\"audio\":{\"data\":\"AgM=\"}},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(body))
        .expect("response")
}

#[tokio::test]
async fn xiaomi_audio_uses_chat_completions_wire_for_asr_and_tts() {
    let capture = Capture::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(audio))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let config = XiaomiConfig::new(format!("http://{address}")).expect("config");
    let transport = Arc::new(ReqwestTransport::new().expect("transport"));
    let registration = ProviderRegistration::builder("xiaomi", "xiaomi")
        .expect("registration")
        .with_credential(
            CredentialRequirement::new("xiaomi", ["MIMO_API_KEY"]).expect("requirement"),
        )
        .with_transcription(XiaomiTranscriptionAdapter::new(
            config.clone(),
            transport.clone(),
        ))
        .with_speech(XiaomiSpeechAdapter::new(config, transport))
        .build()
        .expect("provider");
    let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("xiaomi", "mimo-secret")
            .expect("credential")
            .build(),
    )
    .with_media_resolver(InMemoryMediaResolver::new(BTreeMap::from([(
        digest.to_owned(),
        b"hello world".to_vec(),
    )])))
    .register(registration)
    .expect("register")
    .build()
    .expect("registry");

    let transcript = registry
        .transcription(ModelRef::new("xiaomi", "mimo-v2.5-asr").expect("model"))
        .expect("asr")
        .transcribe(
            TranscriptionRequest::new(
                MediaDescriptor::new(MediaKind::Audio, "audio/wav", 11, digest).expect("audio"),
            )
            .expect("request")
            .with_language("en")
            .expect("language"),
        )
        .await
        .expect("transcript");
    assert_eq!(transcript.text, "hello world");
    let usage = transcript.usage.expect("usage");
    assert_eq!(usage.input_tokens, 8);
    assert_eq!(usage.output_tokens, 2);

    let speech = registry
        .speech(ModelRef::new("xiaomi", "mimo-v2.5-tts").expect("model"))
        .expect("speech")
        .synthesize(SpeechRequest::new("hello", "Mia", SpeechFormat::Pcm16).expect("request"))
        .await
        .expect("speech");
    assert_eq!(speech.audio.bytes, [0, 1, 2, 3]);
    let usage = speech.usage.expect("speech usage");
    assert_eq!(usage.input_tokens, 3);
    assert_eq!(usage.output_tokens, 4);

    let calls = capture.0.lock().expect("capture");
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|(headers, _)| headers["authorization"] == "Bearer mimo-secret")
    );
    assert!(
        calls[0].1["messages"][0]["content"][0]["input_audio"]["data"]
            .as_str()
            .is_some_and(|value| value.starts_with("data:audio/wav;base64,"))
    );
    assert_eq!(calls[0].1["asr_options"]["language"], "en");
    assert_eq!(calls[1].1["messages"][0]["role"], "assistant");
    assert_eq!(calls[1].1["audio"]["voice"], "Mia");
}
