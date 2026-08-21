use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axum::{Router, body::Bytes, http::HeaderMap, response::Response, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_protocol::{
    ErrorKind, MediaDescriptor, MediaKind, SpeechFormat, SpeechRequest, TranscriptionRequest,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use rsi_ai_xiaomi::{XiaomiConfig, XiaomiSpeechAdapter, XiaomiTranscriptionAdapter};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const LARGE_MEDIA_BYTES: usize = 3 * 48 * 1024 + 5;
const LARGE_RESPONSE_BYTES: usize = 3 * 256 * 1024 + 7;

fn patterned_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index % 251).expect("pattern byte"))
        .collect()
}

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
    } else if value["stream"] == true {
        concat!(
            "data: {\"choices\":[{\"delta\":{\"audio\":{\"data\":\"AAE=\"}},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\n",
            "data: {\"choices\":[{\"delta\":{\"audio\":{\"data\":\"AgM=\"}},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else if value["messages"][0]["content"] == "empty" {
        r#"{"choices":[{"message":{"audio":{"data":""}}}]}"#.to_owned()
    } else if value["messages"][0]["content"] == "multiple" {
        r#"{"choices":[{"message":{"audio":{"data":"AA=="}}},{"message":{"audio":{"data":"AQ=="}}}]}"#.to_owned()
    } else if value["messages"][0]["content"] == "invalid-base64" {
        r#"{"choices":[{"message":{"audio":{"data":"%%%"}}}]}"#.to_owned()
    } else if value["messages"][0]["content"] == "large-output" {
        let encoded = BASE64.encode(patterned_bytes(LARGE_RESPONSE_BYTES));
        format!(r#"{{"choices":[{{"message":{{"audio":{{"data":"{encoded}"}}}}}}]}}"#)
    } else {
        r#"{"choices":[{"message":{"audio":{"data":"\u0041AECAw=="}}}]}"#.to_owned()
    };
    Response::builder()
        .header(
            "content-type",
            if value["stream"] == true {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .body(axum::body::Body::from(body))
        .expect("response")
}

async fn audio_registry(capture: Capture, input_audio: &[u8]) -> (Registry, MediaDescriptor) {
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
    let digest = hex::encode(Sha256::digest(input_audio));
    let descriptor = MediaDescriptor::new(
        MediaKind::Audio,
        "audio/wav",
        u64::try_from(input_audio.len()).expect("audio length"),
        &digest,
    )
    .expect("audio");
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("xiaomi", "mimo-secret")
            .expect("credential")
            .build(),
    )
    .with_media_resolver(InMemoryMediaResolver::new(BTreeMap::from([(
        digest.clone(),
        input_audio.to_vec(),
    )])))
    .register(registration)
    .expect("register")
    .build()
    .expect("registry");
    (registry, descriptor)
}

#[tokio::test]
async fn xiaomi_audio_uses_chat_completions_wire_for_asr_and_tts() {
    let capture = Capture::default();
    let input_audio = b"hello world";
    let (registry, descriptor) = audio_registry(capture.clone(), input_audio).await;

    let transcript = registry
        .transcription(ModelRef::new("xiaomi", "mimo-v2.5-asr").expect("model"))
        .expect("asr")
        .transcribe(
            TranscriptionRequest::new(descriptor)
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

    let speech = registry
        .speech(ModelRef::new("xiaomi", "mimo-v2.5-tts").expect("model"))
        .expect("speech")
        .synthesize(SpeechRequest::new("hello", "Mia", SpeechFormat::Mp3).expect("request"))
        .await
        .expect("non-streaming speech");
    assert_eq!(speech.audio.bytes, [0, 1, 2, 3]);

    let calls = capture.0.lock().expect("capture");
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|(headers, _)| headers["authorization"] == "Bearer mimo-secret")
    );
    assert_eq!(calls[0].0["transfer-encoding"], "chunked");
    assert!(!calls[0].0.contains_key("content-length"));
    let encoded_audio = calls[0].1["messages"][0]["content"][0]["input_audio"]["data"]
        .as_str()
        .and_then(|value| value.strip_prefix("data:audio/wav;base64,"))
        .expect("audio data URL");
    assert_eq!(BASE64.decode(encoded_audio).expect("base64"), input_audio);
    assert_eq!(calls[0].1["asr_options"]["language"], "en");
    assert_eq!(calls[1].1["messages"][0]["role"], "assistant");
    assert_eq!(calls[1].1["audio"]["voice"], "Mia");
    assert_eq!(calls[2].1["audio"]["format"], "mp3");
}

#[tokio::test]
async fn nonstream_speech_distinguishes_output_validation_from_protocol_errors() {
    let (registry, _) = audio_registry(Capture::default(), b"unused audio").await;
    for (text, expected, message) in [
        (
            "empty",
            ErrorKind::OutputValidation,
            "empty provider audio is not a valid speech result",
        ),
        (
            "multiple",
            ErrorKind::OutputValidation,
            "multiple choices violate the normalized output contract",
        ),
        (
            "invalid-base64",
            ErrorKind::Protocol,
            "invalid provider base64 violates the wire protocol",
        ),
    ] {
        let error = registry
            .speech(ModelRef::new("xiaomi", "mimo-v2.5-tts").expect("model"))
            .expect("speech")
            .synthesize(SpeechRequest::new(text, "Mia", SpeechFormat::Mp3).expect("request"))
            .await
            .expect_err(message);
        assert_eq!(
            error.provider_error().map(rsi_ai_protocol::AiError::kind),
            Some(expected)
        );
    }
}

#[tokio::test]
async fn base64_request_and_response_cross_incremental_chunk_boundaries() {
    let capture = Capture::default();
    let input_audio = patterned_bytes(LARGE_MEDIA_BYTES);
    let (registry, descriptor) = audio_registry(capture.clone(), &input_audio).await;
    registry
        .transcription(ModelRef::new("xiaomi", "mimo-v2.5-asr").expect("model"))
        .expect("asr")
        .transcribe(TranscriptionRequest::new(descriptor).expect("request"))
        .await
        .expect("multi-chunk request");
    let speech = registry
        .speech(ModelRef::new("xiaomi", "mimo-v2.5-tts").expect("model"))
        .expect("speech")
        .synthesize(SpeechRequest::new("large-output", "Mia", SpeechFormat::Mp3).expect("request"))
        .await
        .expect("multi-chunk response");
    assert_eq!(speech.audio.bytes, patterned_bytes(LARGE_RESPONSE_BYTES));

    let calls = capture.0.lock().expect("capture");
    let encoded_audio = calls[0].1["messages"][0]["content"][0]["input_audio"]["data"]
        .as_str()
        .and_then(|value| value.strip_prefix("data:audio/wav;base64,"))
        .expect("audio data URL");
    assert_eq!(BASE64.decode(encoded_audio).expect("base64"), input_audio);
}
