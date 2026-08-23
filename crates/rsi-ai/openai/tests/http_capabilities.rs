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
use futures_util::StreamExt as _;
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_openai::{
    OpenAiConfig, OpenAiImageAdapter, OpenAiResponsesAdapter, OpenAiSpeechAdapter,
    OpenAiTranscriptionAdapter,
};
use rsi_ai_protocol::{
    ContentBlock, ErrorKind, FreeformFormat, FreeformToolDefinition, HostedTool, ImageEvent,
    ImageRequest, LanguageModelLimits, LanguageRequest, LanguageSettings, MediaDescriptor,
    MediaKind, Message, MessageContent, ReasoningEffort, ResponseFormat, SpeechFormat,
    SpeechRequest, ToolCall, ToolChoice, ToolDefinition, TranscriptionRequest,
};
use rsi_ai_provider::{LanguageAdapter, ProviderRegistration};
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use serde_json::{Value, json};

type CapturedCall = (String, HeaderMap, Vec<u8>);

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<CapturedCall>>>);

fn language_config(endpoint: impl Into<String>) -> OpenAiConfig {
    OpenAiConfig::new(endpoint)
        .and_then(|config| {
            config.with_model_profile(
                "gpt-5",
                LanguageModelLimits::new(200_000, 4_096, 32_768).expect("model limits"),
            )
        })
        .expect("language config")
}

#[test]
fn responses_describe_uses_the_exact_configured_model_capacity() {
    let adapter = OpenAiResponsesAdapter::new(
        OpenAiConfig::new("http://127.0.0.1:9")
            .and_then(|config| {
                config.with_model_profile(
                    "gpt-small",
                    LanguageModelLimits::new(128_000, 4_096, 16_384).expect("small limits"),
                )
            })
            .and_then(|config| {
                config.with_model_profile(
                    "gpt-large",
                    LanguageModelLimits::new(1_000_000, 8_192, 65_536).expect("large limits"),
                )
            })
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );

    assert_eq!(
        adapter
            .describe("gpt-small")
            .expect("small profile")
            .context_window_tokens(),
        128_000
    );
    assert_eq!(
        adapter
            .describe("gpt-large")
            .expect("large profile")
            .context_window_tokens(),
        1_000_000
    );
    assert_eq!(
        adapter
            .describe("unknown")
            .expect_err("unknown model")
            .kind(),
        ErrorKind::InvalidRequest
    );
}

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
    if uri.path() == "/v1/responses"
        && body
            .windows(b"custom-parser-case".len())
            .any(|part| part == b"custom-parser-case")
    {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"custom_tool_call\",\"id\":\"item-custom\",\"call_id\":\"call-custom\",\"name\":\"apply_patch\",\"input\":\"\"}}\n\n",
                "data: {\"type\":\"response.custom_tool_call_input.delta\",\"item_id\":\"item-custom\",\"delta\":\"*** Begin Patch\\n*** End Patch\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
                "data: [DONE]\n\n"
            )))
            .expect("custom tool response");
    }
    if uri.path() == "/v1/responses"
        && let Some(response) = responses_failure_fixture(&body)
    {
        return response;
    }
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
        "/v1/images/generations" | "/v1/images/edits" => Response::new(Body::from(
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

fn responses_failure_fixture(body: &[u8]) -> Option<Response> {
    const FIXTURES: [(&[u8], &str, &str); 5] = [
        (
            b"context-limit-case",
            r#"{"type":"response.failed","response":{"id":"resp-context-limit","status":"failed","error":{"code":"context_length_exceeded","message":"input exceeds this model's context window"}}}"#,
            "context limit response",
        ),
        (
            b"null-error-case",
            r#"{"type":"response.failed","response":{"error":null},"error":{"code":"server_error","message":"nested fallback"}}"#,
            "null nested error response",
        ),
        (
            b"envelope-message-case",
            r#"{"type":"response.failed","message":"do not scrape this envelope"}"#,
            "envelope message response",
        ),
        (
            b"invalid-error-code-case",
            r#"{"type":"response.failed","error":{"code":"bad code","message":"provider failed"}}"#,
            "invalid error code response",
        ),
        (
            b"top-level-error-case",
            r#"{"type":"error","code":"context_length_exceeded","message":"top-level context limit","param":null,"sequence_number":1}"#,
            "top-level error response",
        ),
    ];
    let (_, event, label) = FIXTURES
        .into_iter()
        .find(|(marker, _, _)| body.windows(marker.len()).any(|part| part == *marker))?;
    Some(
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(format!("data: {event}\n\ndata: [DONE]\n\n")))
            .expect(label),
    )
}

async fn one_image() -> Response {
    Response::new(Body::from(r#"{"data":[{"b64_json":"iVBORw=="}]}"#))
}

#[tokio::test]
async fn image_count_mismatch_follows_completed_image_events_with_output_validation() {
    let app = Router::new().route("/v1/images/generations", post(one_image));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let registration = ProviderRegistration::builder("openai", "openai")
        .expect("registration")
        .with_credential(credential())
        .with_image(OpenAiImageAdapter::new(
            OpenAiConfig::new(format!("http://{address}")).expect("config"),
            Arc::new(ReqwestTransport::new().expect("transport")),
        ))
        .build()
        .expect("provider");
    let image = Registry::builder(
        CredentialManager::builder()
            .with_explicit("openai", "openai-secret")
            .expect("credential")
            .build(),
    )
    .register(registration)
    .expect("registration")
    .build()
    .expect("registry")
    .image(ModelRef::new("openai", "gpt-image-1").expect("model"))
    .expect("image");

    let mut generation = image
        .prepare(ImageRequest::new("two dots", 2).expect("request"))
        .await
        .expect("prepare")
        .start()
        .await
        .expect("start");
    let events = generation.by_ref().collect::<Vec<_>>().await;
    assert_eq!(
        events,
        vec![
            ImageEvent::OutputStarted {
                index: 0,
                mime_type: "image/png".to_owned(),
            },
            ImageEvent::OutputChunk {
                index: 0,
                sequence: 1,
                bytes: vec![137, 80, 78, 71],
            },
            ImageEvent::OutputFinished { index: 0 },
        ]
    );
    let error = generation
        .finish()
        .expect_err("one image cannot satisfy a two-image request");
    assert_eq!(
        error.provider_error().map(rsi_ai_protocol::AiError::kind),
        Some(ErrorKind::OutputValidation)
    );
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
            language_config(format!("http://{address}")),
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
async fn text_only_language_request_is_buffered_with_content_length() {
    let capture = Capture::default();
    language_model(capture.clone())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user(vec![MessageContent::Text {
                    text: "hello".to_owned(),
                }])
                .expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect("response");

    let calls = capture.0.lock().expect("capture");
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.contains_key("content-length"));
    assert!(!calls[0].1.contains_key("transfer-encoding"));
}

fn freeform_patch_tool() -> ToolDefinition {
    function_patch_tool()
        .with_freeform(
            FreeformToolDefinition::new(FreeformFormat::Lark, "start: /.+/")
                .expect("freeform grammar"),
        )
        .expect("freeform tool")
}

fn function_patch_tool() -> ToolDefinition {
    ToolDefinition::new(
        "apply_patch",
        "Apply a patch.",
        json!({
            "type": "object",
            "required": ["patch"],
            "properties": {"patch": {"type": "string"}},
            "additionalProperties": false
        }),
    )
    .expect("function schema")
}

#[tokio::test]
async fn responses_preserves_historical_freeform_wire_kind_when_the_catalog_changes() {
    let capture = Capture::default();
    let output = language_model(capture.clone())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("custom-parser-case").expect("message"),
            ])
            .expect("request")
            .with_tools(vec![freeform_patch_tool()], ToolChoice::Auto)
            .expect("tools"),
        )
        .await
        .expect("custom tool response");
    let [ContentBlock::ToolCall(call)] = output.content.as_slice() else {
        panic!("expected one custom tool call: {:?}", output.content)
    };
    assert_eq!(call.id, "call-custom");
    assert_eq!(call.name, "apply_patch");
    assert_eq!(call.arguments, "*** Begin Patch\n*** End Patch");
    assert_eq!(call.kind, rsi_ai_protocol::ToolCallKind::Freeform);

    language_model(capture.clone())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::assistant(vec![MessageContent::ToolCall(call.clone())])
                    .expect("assistant tool call"),
                Message::tool_result(
                    call.id.clone(),
                    vec![MessageContent::Text {
                        text: "Done".to_owned(),
                    }],
                    false,
                )
                .expect("tool result"),
                Message::user_text("continue").expect("message"),
            ])
            .expect("request")
            .with_tools(vec![function_patch_tool()], ToolChoice::Auto)
            .expect("tools"),
        )
        .await
        .expect("custom history response");

    let calls = capture.0.lock().expect("capture");
    let first: Value = serde_json::from_slice(&calls[0].2).expect("first request");
    assert_eq!(first["tools"][0]["type"], "custom");
    assert_eq!(first["tools"][0]["format"]["syntax"], "lark");
    let second: Value = serde_json::from_slice(&calls[1].2).expect("second request");
    assert_eq!(second["tools"][0]["type"], "function");
    assert_eq!(second["input"][0]["type"], "custom_tool_call");
    assert_eq!(second["input"][0]["input"], call.arguments);
    assert_eq!(second["input"][1]["type"], "custom_tool_call_output");
}

#[tokio::test]
async fn responses_forces_a_declared_freeform_tool_as_custom() {
    let capture = Capture::default();
    language_model(capture.clone())
        .await
        .complete(
            LanguageRequest::new(vec![Message::user_text("force patch").expect("message")])
                .expect("request")
                .with_tools(
                    vec![freeform_patch_tool()],
                    ToolChoice::Specific("apply_patch".to_owned()),
                )
                .expect("tools"),
        )
        .await
        .expect("response");

    let calls = capture.0.lock().expect("capture");
    let request: Value = serde_json::from_slice(&calls[0].2).expect("request JSON");
    assert_eq!(request["tools"][0]["type"], "custom");
    assert_eq!(request["tool_choice"]["type"], "custom");
    assert_eq!(request["tool_choice"]["name"], "apply_patch");
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
async fn responses_failed_event_preserves_the_context_limit_error() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("context-limit-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect_err("context limit must remain a provider failure");
    let provider = error.provider_error().expect("provider error facts");
    assert_eq!(provider.kind(), ErrorKind::ContextLimit);
    assert_eq!(provider.provider_code(), Some("context_length_exceeded"));
    assert_eq!(
        provider.safe_summary(),
        "input exceeds this model's context window"
    );
}

#[tokio::test]
async fn responses_top_level_error_event_preserves_provider_facts() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("top-level-error-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect_err("top-level error event must fail");
    let provider = error.provider_error().expect("provider error facts");
    assert_eq!(provider.kind(), ErrorKind::ContextLimit);
    assert_eq!(provider.provider_code(), Some("context_length_exceeded"));
    assert_eq!(provider.safe_summary(), "top-level context limit");
}

#[tokio::test]
async fn responses_failed_event_uses_only_valid_nested_error_facts() {
    let model = language_model(Capture::default()).await;
    let request = |prompt: &str| {
        LanguageRequest::new(vec![Message::user_text(prompt).expect("message")]).expect("request")
    };

    let fallback = model
        .complete(request("null-error-case"))
        .await
        .expect_err("nested provider error must fail");
    let fallback = fallback.provider_error().expect("provider error facts");
    assert_eq!(fallback.safe_summary(), "nested fallback");
    assert_eq!(fallback.provider_code(), Some("server_error"));

    let envelope = model
        .complete(request("envelope-message-case"))
        .await
        .expect_err("envelope failure must fail");
    assert_eq!(
        envelope
            .provider_error()
            .expect("provider error facts")
            .safe_summary(),
        "OpenAI Responses did not complete successfully"
    );

    let invalid = model
        .complete(request("invalid-error-code-case"))
        .await
        .expect_err("invalid provider code must fail closed");
    let invalid = invalid.provider_error().expect("provider error facts");
    assert_eq!(invalid.kind(), ErrorKind::Protocol);
    assert_eq!(
        invalid.safe_summary(),
        "OpenAI Responses returned an invalid error code"
    );
    assert_eq!(invalid.provider_code(), None);
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
        .route("/v1/images/edits", post(endpoint))
        .route("/v1/audio/transcriptions", post(endpoint))
        .route("/v1/audio/speech", post(endpoint))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });

    let config = language_config(format!("http://{address}"));
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
    let image_digest = "0f4636c78f65d3639ece5a064b5ae753e3408614a14fb18ab4d7540d2c248543";
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("openai", "openai-secret")
            .expect("credential")
            .build(),
    )
    .with_media_resolver(InMemoryMediaResolver::new(BTreeMap::from([
        (audio_digest.to_owned(), b"hello world".to_vec()),
        (image_digest.to_owned(), vec![137, 80, 78, 71]),
    ])))
    .register(registration)
    .expect("register")
    .build()
    .expect("registry");
    let image_media =
        MediaDescriptor::new(MediaKind::Image, "image/png", 4, image_digest).expect("image input");

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
                        kind: rsi_ai_protocol::ToolCallKind::Function,
                    }),
                ])
                .expect("assistant history"),
                Message::tool_result(
                    "call-1",
                    vec![
                        MessageContent::Text {
                            text: "tool text".to_owned(),
                        },
                        MessageContent::Image(image_media.clone()),
                    ],
                    false,
                )
                .expect("rich tool result"),
                Message::user(vec![
                    MessageContent::Text {
                        text: "hello".to_owned(),
                    },
                    MessageContent::Image(image_media.clone()),
                ])
                .expect("message"),
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
            .expect("settings")
            .with_response_format(
                ResponseFormat::json_schema(
                    "answer",
                    None,
                    json!({"type":"string", "const":"\0rsi-media-0\0"}),
                )
                .expect("response format"),
            )
            .expect("structured output"),
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
        .generate(
            ImageRequest::new("a dot", 1)
                .expect("request")
                .with_inputs(vec![image_media], None)
                .expect("image edit"),
        )
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
    assert_eq!(calls[0].1["transfer-encoding"], "chunked");
    assert!(!calls[0].1.contains_key("content-length"));
    let response_body: Value = serde_json::from_slice(&calls[0].2).expect("responses request");
    assert_eq!(response_body["stream"], true);
    assert_eq!(response_body["max_output_tokens"], 800);
    assert_eq!(response_body["temperature"], 0.2);
    assert_eq!(response_body["top_p"], 0.75);
    assert_eq!(response_body["reasoning"]["effort"], "high");
    assert_eq!(response_body["input"][0]["type"], "message");
    assert_eq!(response_body["input"][0]["content"][0]["text"], "before");
    assert_eq!(response_body["input"][1]["type"], "function_call");
    assert_eq!(
        response_body["input"][2]["output"][0],
        json!({"type":"input_text", "text":"tool text"})
    );
    assert_eq!(
        response_body["input"][2]["output"][1]["image_url"],
        "data:image/png;base64,iVBORw=="
    );
    assert_eq!(
        response_body["input"][3]["content"][1]["image_url"],
        "data:image/png;base64,iVBORw=="
    );
    assert_eq!(
        response_body["text"]["format"]["schema"]["const"],
        "\0rsi-media-0\0"
    );
    assert_eq!(calls[1].0, "/v1/images/edits");
    assert_eq!(calls[1].1["transfer-encoding"], "chunked");
    assert!(!calls[1].1.contains_key("content-length"));
    assert!(
        calls[1]
            .2
            .windows(4)
            .any(|window| window == [137, 80, 78, 71])
    );
    assert_eq!(calls[2].1["transfer-encoding"], "chunked");
    assert!(!calls[2].1.contains_key("content-length"));
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
