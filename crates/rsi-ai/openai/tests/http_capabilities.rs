use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Uri},
    response::Response,
    routing::post,
};
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_openai::{
    OpenAiConfig, OpenAiImageAdapter, OpenAiResponsesAdapter, OpenAiSpeechAdapter,
    OpenAiTranscriptionAdapter,
};
use rsi_ai_protocol::{
    ContentBlock, HostedTool, ImageRequest, LanguageRequest, LanguageSettings, MediaDescriptor,
    MediaKind, Message, MessageContent, ReasoningEffort, SpeechFormat, SpeechRequest, ToolCall,
    TranscriptionRequest,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use serde_json::{Value, json};

type CapturedCall = (String, HeaderMap, Vec<u8>);

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<CapturedCall>>>);

async fn endpoint(
    State(capture): State<Capture>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    capture
        .0
        .lock()
        .expect("capture")
        .push((uri.path().to_owned(), headers, body.to_vec()));
    if uri.path() == "/v1/responses" && body.windows(15).any(|part| part == b"incomplete-case") {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg-limit\",\"content_index\":0,\"delta\":\"partial\"}\n\n",
                "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp-limit\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":8}}}\n\n",
                "data: [DONE]\n\n"
            )))
            .expect("incomplete response");
    }
    if uri.path() == "/v1/responses" && body.windows(19).any(|part| part == b"citation-bound-case")
    {
        let item_id = "i".repeat(rsi_ai_protocol::MAX_ID_BYTES);
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(format!(
                concat!(
                    "data: {{\"type\":\"response.output_text.delta\",",
                    "\"item_id\":\"{}\",\"content_index\":0,\"delta\":\"cited\"}}\n\n",
                    "data: {{\"type\":\"response.output_text.annotation.added\",",
                    "\"item_id\":\"{}\",\"annotation_index\":18446744073709551615,",
                    "\"annotation\":{{\"type\":\"url_citation\",",
                    "\"url\":\"https://example.test/source\"}}}}\n\n",
                    "data: {{\"type\":\"response.completed\",\"response\":{{}}}}\n\n",
                    "data: [DONE]\n\n"
                ),
                item_id, item_id
            )))
            .expect("citation response");
    }
    match uri.path() {
        "/v1/responses" => Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"type\":\"response.web_search_call.in_progress\",\"item_id\":\"search-1\"}\n\n",
                "data: {\"type\":\"response.web_search_call.searching\",\"item_id\":\"search-1\"}\n\n",
                "data: {\"type\":\"response.web_search_call.completed\",\"item_id\":\"search-1\"}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg-1\",\"content_index\":0,\"delta\":\"hello\"}\n\n",
                "data: {\"type\":\"response.output_text.annotation.added\",\"item_id\":\"msg-1\",\"annotation_index\":0,\"annotation\":{\"type\":\"url_citation\",\"title\":\"Example\",\"url\":\"https://example.test/source\",\"start_index\":0,\"end_index\":5}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n",
                "data: [DONE]\n\n"
            )))
            .expect("response"),
        "/v1/images/generations" => Response::new(Body::from(
            json!({"data":[{"b64_json":"iVBORw=="}]}).to_string(),
        )),
        "/v1/audio/transcriptions" => {
            let text = format!("{}é", "a".repeat(65_535));
            Response::new(Body::from(json!({
                "text":text,
                "language":"en",
                "segments":[{"id":0,"start":0.0,"end":1.25,"text":text}]
            })
            .to_string()))
        }
        "/v1/audio/speech" => Response::builder()
            .header("content-type", "audio/mpeg")
            .body(Body::from(vec![0_u8, 1, 2, 3]))
            .expect("speech"),
        path => panic!("unexpected path {path}"),
    }
}

async fn language_model(capture: Capture) -> rsi_ai::LanguageModel {
    let app = Router::new()
        .route("/v1/responses", post(endpoint))
        .with_state(capture);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let registration = ProviderRegistration::builder("openai", "openai")
        .expect("registration")
        .with_credential(credential())
        .with_language(OpenAiResponsesAdapter::new(
            OpenAiConfig::new(format!("http://{address}")).expect("config"),
            Arc::new(ReqwestTransport::new().expect("transport")),
        ))
        .build()
        .expect("provider");
    Registry::builder(
        CredentialManager::builder()
            .with_explicit("openai", "openai-secret")
            .expect("credential")
            .build(),
    )
    .register(registration)
    .expect("registration")
    .build()
    .expect("registry")
    .language(ModelRef::new("openai", "gpt-5").expect("model"))
    .expect("language")
}

#[tokio::test]
async fn max_output_token_incomplete_response_preserves_partial_output() {
    let output = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("incomplete-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect("bounded incomplete is a successful terminal");
    assert_eq!(output.visible_text(), "partial");
    assert_eq!(
        output.finish_reason,
        rsi_ai_protocol::FinishReason::MaxTokens
    );
    assert_eq!(output.usage.expect("usage").output_tokens, 8);
    assert_eq!(
        output.replay.expect("replay").value["response_id"],
        "resp-limit"
    );
}

#[tokio::test]
async fn responses_adapter_bounds_source_ids_for_maximum_provider_item_ids() {
    let output = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("citation-bound-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect("bounded citation succeeds");

    let source_id = &output.sources.first().expect("source").id;
    assert!(source_id.len() <= rsi_ai_protocol::MAX_ID_BYTES);
    assert!(source_id.starts_with("openai-source-"));
}

fn credential() -> CredentialRequirement {
    CredentialRequirement::new("openai", ["OPENAI_API_KEY"]).expect("requirement")
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end test proves all four HTTP capabilities.
async fn openai_http_adapters_cover_responses_images_asr_and_tts() {
    let capture = Capture::default();
    let app = Router::new()
        .route("/v1/responses", post(endpoint))
        .route("/v1/images/generations", post(endpoint))
        .route("/v1/audio/transcriptions", post(endpoint))
        .route("/v1/audio/speech", post(endpoint))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });

    let config = OpenAiConfig::new(format!("http://{address}")).expect("config");
    let transport = Arc::new(ReqwestTransport::new().expect("transport"));
    let registration = ProviderRegistration::builder("openai", "openai")
        .expect("registration")
        .with_credential(credential())
        .with_language(OpenAiResponsesAdapter::new(
            config.clone(),
            transport.clone(),
        ))
        .with_image(OpenAiImageAdapter::new(config.clone(), transport.clone()))
        .with_transcription(OpenAiTranscriptionAdapter::new(
            config.clone(),
            transport.clone(),
        ))
        .with_speech(OpenAiSpeechAdapter::new(config, transport))
        .build()
        .expect("provider");
    let audio_digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("openai", "openai-secret")
            .expect("credential")
            .build(),
    )
    .with_media_resolver(InMemoryMediaResolver::new(BTreeMap::from([(
        audio_digest.to_owned(),
        b"hello world".to_vec(),
    )])))
    .register(registration)
    .expect("register")
    .build()
    .expect("registry");

    let language = registry
        .language(ModelRef::new("openai", "gpt-5").expect("model"))
        .expect("language")
        .complete(
            LanguageRequest::new(vec![
                Message::assistant(vec![
                    MessageContent::Text {
                        text: "before".to_owned(),
                    },
                    MessageContent::ToolCall(ToolCall {
                        id: "call-1".to_owned(),
                        name: "lookup".to_owned(),
                        arguments: "{}".to_owned(),
                    }),
                ])
                .expect("assistant history"),
                Message::user_text("hello").expect("message"),
            ])
            .expect("request")
            .with_hosted_tools(vec![HostedTool::WebSearch { max_uses: None }])
            .expect("hosted tool")
            .with_settings(
                LanguageSettings::default()
                    .with_max_output_tokens(800)
                    .expect("tokens")
                    .with_sampling(Some(0.2), Some(0.75))
                    .expect("sampling")
                    .with_reasoning_effort(ReasoningEffort::High),
            )
            .expect("settings"),
        )
        .await
        .expect("response");
    assert_eq!(
        language.content,
        vec![ContentBlock::Text {
            text: "hello".to_owned()
        }]
    );
    assert_eq!(language.sources.len(), 1);
    assert_eq!(
        language.sources[0].url.as_deref(),
        Some("https://example.test/source")
    );
    assert_eq!(
        language.replay.expect("replay").value["response_id"],
        "resp-1"
    );

    let image = registry
        .image(ModelRef::new("openai", "gpt-image-1").expect("model"))
        .expect("image")
        .generate(ImageRequest::new("a dot", 1).expect("request"))
        .await
        .expect("image output");
    assert_eq!(image.images[0].bytes, [137, 80, 78, 71]);

    let audio =
        MediaDescriptor::new(MediaKind::Audio, "audio/wav", 11, audio_digest).expect("audio");
    let transcript = registry
        .transcription(ModelRef::new("openai", "gpt-4o-transcribe").expect("model"))
        .expect("transcription")
        .transcribe(
            TranscriptionRequest::new(audio)
                .expect("request")
                .with_timestamps(true),
        )
        .await
        .expect("transcript");
    assert_eq!(transcript.text.len(), 65_537);
    assert!(transcript.text.ends_with('é'));
    assert_eq!(transcript.segments[0].end_ms, 1_250);

    let speech = registry
        .speech(ModelRef::new("openai", "gpt-4o-mini-tts").expect("model"))
        .expect("speech")
        .synthesize(SpeechRequest::new("hello", "alloy", SpeechFormat::Mp3).expect("request"))
        .await
        .expect("speech output");
    assert_eq!(speech.audio.bytes, [0, 1, 2, 3]);

    let calls = capture.0.lock().expect("capture");
    assert_eq!(calls.len(), 4);
    assert!(calls.iter().all(|(_, headers, _)| {
        headers
            .get("authorization")
            .is_some_and(|value| value == "Bearer openai-secret")
    }));
    let response_body: Value = serde_json::from_slice(&calls[0].2).expect("responses request");
    assert_eq!(response_body["stream"], true);
    assert_eq!(response_body["max_output_tokens"], 800);
    assert_eq!(response_body["temperature"], 0.2);
    assert_eq!(response_body["top_p"], 0.75);
    assert_eq!(response_body["reasoning"]["effort"], "high");
    assert_eq!(response_body["input"][0]["type"], "message");
    assert_eq!(response_body["input"][0]["content"][0]["text"], "before");
    assert_eq!(response_body["input"][1]["type"], "function_call");
    let transcription_body = &calls[2].2;
    assert!(
        transcription_body
            .windows(11)
            .any(|window| window == b"hello world")
    );
    let speech_body: Value = serde_json::from_slice(&calls[3].2).expect("speech request");
    assert!(
        speech_body.get("speed").is_none(),
        "an unset optional speed must be omitted, not serialized as null"
    );
}
