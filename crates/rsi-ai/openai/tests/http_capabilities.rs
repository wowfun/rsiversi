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
use rsi_ai_openai::{OpenAiConfig, OpenAiImageAdapter, OpenAiResponsesAdapter};
use rsi_ai_protocol::{
    AiCapability, AiError, ContentBlock, ErrorKind, FreeformFormat, FreeformToolDefinition,
    ImageAssembler, ImageEvent, ImageRequest, LanguageAssembler, LanguageAssemblyError,
    LanguageModelLimits, LanguageOutput, LanguageRequest, MediaDescriptor, MediaKind, Message,
    MessageContent, PreparedCallSnapshot, ProviderExtension, RetryPolicy, ToolCall, ToolCallKind,
    ToolChoice, ToolDefinition,
};
use rsi_ai_provider::{AbortSignal, ImageAdapter, LanguageAdapter, PrepareContext};
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};
use serde_json::{Value, json};

type CapturedCall = (String, HeaderMap, Vec<u8>);

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<CapturedCall>>>);

fn context(
    capability: AiCapability,
    media: InMemoryMediaResolver,
    media_admission_bytes: u64,
) -> PrepareContext {
    PrepareContext::new(
        PreparedCallSnapshot {
            call_id: "test-call".into(),
            deployment_id: "openai".into(),
            provider_family: "openai".into(),
            capability,
            model: match capability {
                AiCapability::Language => "gpt-5",
                AiCapability::Image => "gpt-image-1",
            }
            .into(),
            protocol: "openai-responses".into(),
            transport: "http".into(),
            endpoint_fingerprint: "test-endpoint".into(),
            config_generation: 1,
            credential_source: Some(CredentialSource::Keyring),
            retry_policy: RetryPolicy::default(),
            request_sha256: "0".repeat(64),
        },
        Some(ResolvedCredential {
            secret: SecretValue::new("openai-secret").expect("secret"),
            source: CredentialSource::Keyring,
        }),
        Arc::new(media),
        media_admission_bytes,
    )
    .expect("test provider context")
}

#[derive(Debug)]
enum CompleteError {
    Provider(AiError),
    Assembly(LanguageAssemblyError),
}

impl CompleteError {
    fn provider_error(&self) -> Option<&AiError> {
        match self {
            Self::Provider(error)
            | Self::Assembly(LanguageAssemblyError::Provider { error, .. }) => Some(error),
            Self::Assembly(LanguageAssemblyError::Protocol(_)) => None,
        }
    }
}

#[derive(Clone)]
struct TestLanguageModel {
    adapter: OpenAiResponsesAdapter,
    media: InMemoryMediaResolver,
    media_admission_bytes: u64,
}

impl TestLanguageModel {
    async fn complete(&self, request: LanguageRequest) -> Result<LanguageOutput, CompleteError> {
        let prepared = self
            .adapter
            .prepare(
                context(
                    AiCapability::Language,
                    self.media.clone(),
                    self.media_admission_bytes,
                ),
                "gpt-5".into(),
                request,
            )
            .await
            .map_err(CompleteError::Provider)?;
        let mut stream = prepared
            .start(AbortSignal::new())
            .await
            .map_err(CompleteError::Provider)?;
        let mut assembler = LanguageAssembler::new();
        while let Some(event) = stream.next().await {
            let event = event.map_err(CompleteError::Provider)?;
            assembler
                .push(&event)
                .map_err(|error| CompleteError::Assembly(error.into()))?;
        }
        assembler.finish().map_err(CompleteError::Assembly)
    }
}

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

#[test]
fn responses_rejects_known_incompatible_history_during_prepare_validation() {
    let adapter = OpenAiResponsesAdapter::new(
        language_config("http://127.0.0.1:9"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let audio = |mime: &str| {
        MediaDescriptor::new(MediaKind::Audio, mime, 1, "0".repeat(64)).expect("audio descriptor")
    };
    let image = || {
        MediaDescriptor::new(MediaKind::Image, "image/png", 1, "1".repeat(64))
            .expect("image descriptor")
    };
    let call = ToolCall {
        id: "call-1".into(),
        name: "echo".into(),
        arguments: "{}".into(),
        kind: ToolCallKind::Function,
    };
    let cases = [
        LanguageRequest::new(vec![
            Message::assistant(vec![MessageContent::Reasoning {
                text: "prior reasoning".into(),
                evidence: None,
            }])
            .expect("assistant reasoning"),
        ])
        .expect("request"),
        LanguageRequest::new(vec![
            Message::assistant(vec![MessageContent::Image(image())]).expect("assistant media"),
        ])
        .expect("request"),
        LanguageRequest::new(vec![
            Message::user(vec![MessageContent::Audio(audio("audio/ogg"))]).expect("user audio"),
        ])
        .expect("request"),
        LanguageRequest::new(vec![
            Message::assistant(vec![MessageContent::ToolCall(call.clone())])
                .expect("assistant tool call"),
            Message::tool_result(
                call.id.clone(),
                vec![MessageContent::Audio(audio("audio/wav"))],
                false,
            )
            .expect("audio tool result"),
        ])
        .expect("request"),
    ];

    for request in cases {
        let error = adapter
            .validate_request("gpt-5", &request)
            .expect_err("known incompatible request must fail validation");
        assert_eq!(error.phase(), rsi_ai_protocol::ErrorPhase::Prepare);
        assert_eq!(
            error.dispatch_status(),
            rsi_ai_protocol::DispatchStatus::NotStarted
        );
    }
}

#[test]
fn responses_replay_extension_is_fully_validated_before_start() {
    let adapter = OpenAiResponsesAdapter::new(
        language_config("http://127.0.0.1:9"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    for extension in [
        ProviderExtension::new("another.provider", 0, json!({"response_id":"resp-1"})).unwrap(),
        ProviderExtension::new("openai.responses.replay", 0, json!({"response_id":""})).unwrap(),
        ProviderExtension::new("openai.responses.replay", 0, json!({})).unwrap(),
    ] {
        let request = LanguageRequest::new(vec![Message::user_text("continue").unwrap()])
            .unwrap()
            .with_extensions(vec![extension])
            .unwrap();
        assert!(adapter.validate_request("gpt-5", &request).is_err());
    }
}

#[allow(clippy::too_many_lines)] // One local HTTP fixture enumerates bounded provider wire cases.
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
        && body
            .windows(b"deep-json-case".len())
            .any(|part| part == b"deep-json-case")
    {
        let nested = format!("{}null{}", "[".repeat(65), "]".repeat(65));
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(format!(
                "data: {{\"type\":\"response.completed\",\"response\":{{}},\"extra\":{nested}}}\n\ndata: [DONE]\n\n"
            )))
            .expect("deep JSON response");
    }
    if uri.path() == "/v1/responses"
        && body
            .windows(b"large-terminal-frame-case".len())
            .any(|part| part == b"large-terminal-frame-case")
    {
        let echoed_output = "x".repeat(300 * 1024);
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(format!(
                concat!(
                    "data: {{\"type\":\"response.output_text.delta\",",
                    "\"item_id\":\"msg-large-terminal\",\"content_index\":0,",
                    "\"delta\":\"visible\"}}\n\n",
                    "data: {{\"type\":\"response.completed\",",
                    "\"response\":{{\"id\":\"resp-large-terminal\",",
                    "\"status\":\"completed\",\"output\":[{{\"text\":\"{}\"}}]}}}}\n\n",
                    "data: [DONE]\n\n"
                ),
                echoed_output
            )))
            .expect("large terminal response");
    }
    if uri.path() == "/v1/responses"
        && body
            .windows(b"oversized-response-id-case".len())
            .any(|part| part == b"oversized-response-id-case")
    {
        let response_id = "r".repeat(rsi_ai_protocol::MAX_EXTENSION_BYTES);
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(format!(
                "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"{response_id}\",\"status\":\"completed\"}}}}\n\ndata: [DONE]\n\n"
            )))
            .expect("oversized response id");
    }
    if uri.path() == "/v1/responses"
        && body
            .windows(b"empty-response-id-case".len())
            .any(|part| part == b"empty-response-id-case")
    {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"\",\"status\":\"completed\"}}\n\ndata: [DONE]\n\n",
            ))
            .expect("empty response id");
    }
    if uri.path() == "/v1/responses"
        && body
            .windows(b"queued-event-case".len())
            .any(|part| part == b"queued-event-case")
    {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"type\":\"response.queued\",\"response\":{\"id\":\"resp-queued\",\"status\":\"queued\"},\"sequence_number\":1}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg-queued\",\"content_index\":0,\"delta\":\"ready\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-queued\",\"status\":\"completed\"}}\n\n",
                "data: [DONE]\n\n"
            )))
            .expect("queued response");
    }
    if uri.path() == "/v1/responses"
        && body
            .windows(b"refusal-event-case".len())
            .any(|part| part == b"refusal-event-case")
    {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"type\":\"response.refusal.delta\",\"item_id\":\"msg-refusal\",\"output_index\":0,\"content_index\":0,\"delta\":\"I cannot help with that.\",\"sequence_number\":1}\n\n",
                "data: {\"type\":\"response.refusal.done\",\"item_id\":\"msg-refusal\",\"output_index\":0,\"content_index\":0,\"refusal\":\"I cannot help with that.\",\"sequence_number\":2}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-refusal\",\"status\":\"completed\"}}\n\n",
                "data: [DONE]\n\n"
            )))
            .expect("refusal response");
    }
    if uri.path() == "/v1/responses"
        && let Some(response) = responses_failure_fixture(&body)
    {
        return response;
    }
    if uri.path() == "/v1/responses"
        && let Some(response) = completed_status_conflict_fixture(&body)
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
            json!({"data":[{"b64_json":"iVBORw0KGgo="}]}).to_string(),
        )),
        path => panic!("unexpected path {path}"),
    }
}

fn completed_status_conflict_fixture(body: &[u8]) -> Option<Response> {
    let marker = b"completed-failed-status-case";
    body.windows(marker.len())
        .any(|part| part == marker)
        .then(|| {
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(concat!(
                    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg-inconsistent\",\"content_index\":0,\"delta\":\"must not succeed\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-inconsistent\",\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"failed despite completed event\"}}}\n\n",
                    "data: [DONE]\n\n"
                )))
                .expect("inconsistent completed response")
        })
}

fn responses_failure_fixture(body: &[u8]) -> Option<Response> {
    const FIXTURES: [(&[u8], &str, &str); 6] = [
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
        (
            b"stream-auth-case",
            r#"{"type":"error","code":"invalid_api_key","message":"invalid credential"}"#,
            "stream authentication response",
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
    Response::new(Body::from(r#"{"data":[{"b64_json":"iVBORw0KGgo="}]}"#))
}

async fn non_png_image() -> Response {
    Response::new(Body::from(r#"{"data":[{"b64_json":"bm90LWEtcG5n"}]}"#))
}

#[tokio::test]
async fn image_output_is_validated_before_png_is_declared() {
    let app = Router::new().route("/v1/images/generations", post(non_png_image));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let image = OpenAiImageAdapter::new(
        OpenAiConfig::new(format!("http://{address}")).expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let prepared = image
        .prepare(
            context(AiCapability::Image, InMemoryMediaResolver::default(), 0),
            "gpt-image-1".into(),
            ImageRequest::new("not a png", 1).expect("request"),
        )
        .await
        .expect("prepare");
    let mut generation = prepared.start(AbortSignal::new()).await.expect("start");
    let error = generation
        .next()
        .await
        .expect("one outcome")
        .expect_err("invalid bytes must fail before an output is declared");
    assert_eq!(error.kind(), ErrorKind::OutputValidation);
}

#[tokio::test]
async fn image_count_mismatch_follows_completed_image_events_with_output_validation() {
    let app = Router::new().route("/v1/images/generations", post(one_image));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let image = OpenAiImageAdapter::new(
        OpenAiConfig::new(format!("http://{address}")).expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let prepared = image
        .prepare(
            context(AiCapability::Image, InMemoryMediaResolver::default(), 0),
            "gpt-image-1".into(),
            ImageRequest::new("two dots", 2).expect("request"),
        )
        .await
        .expect("prepare");
    let mut generation = prepared.start(AbortSignal::new()).await.expect("start");
    let mut events = Vec::new();
    let error = loop {
        match generation.next().await {
            Some(Ok(event)) => events.push(event),
            Some(Err(error)) => break error,
            None => panic!("count mismatch must fail"),
        }
    };
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
                bytes: vec![137, 80, 78, 71, 13, 10, 26, 10],
            },
            ImageEvent::OutputFinished { index: 0 },
        ]
    );
    assert_eq!(error.kind(), ErrorKind::OutputValidation);
}

async fn language_model(capture: Capture) -> TestLanguageModel {
    let app = Router::new()
        .route("/v1/responses", post(endpoint))
        .with_state(capture);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    TestLanguageModel {
        adapter: OpenAiResponsesAdapter::new(
            language_config(format!("http://{address}")),
            Arc::new(ReqwestTransport::new().expect("transport")),
        ),
        media: InMemoryMediaResolver::default(),
        media_admission_bytes: 0,
    }
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
        output.replay.expect("replay").value()["response_id"],
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
async fn responses_completed_event_rejects_a_conflicting_embedded_status() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("completed-failed-status-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect_err("conflicting terminal status must not become success");
    assert_eq!(
        error.provider_error().expect("provider error").kind(),
        ErrorKind::Protocol
    );
}

#[tokio::test]
async fn responses_reject_json_beyond_the_shared_structure_bound() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![Message::user_text("deep-json-case").expect("message")])
                .expect("request"),
        )
        .await
        .expect_err("deep provider JSON must fail before event interpretation");
    let provider = error.provider_error().expect("provider error facts");
    assert_eq!(provider.kind(), ErrorKind::Protocol);
    assert_eq!(
        provider.safe_summary(),
        "OpenAI response exceeds the JSON structure limits"
    );
}

#[tokio::test]
async fn responses_accepts_a_terminal_snapshot_larger_than_the_default_delta_frame() {
    let output = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("large-terminal-frame-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect("the terminal snapshot repeats already streamed output within its body bound");
    assert_eq!(output.visible_text(), "visible");
}

#[tokio::test]
async fn responses_rejects_a_response_id_that_cannot_become_replay_state() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("oversized-response-id-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect_err("oversized provider response identity must be a typed stream failure");
    let provider = error.provider_error().expect("provider error facts");
    assert_eq!(provider.kind(), ErrorKind::OutputValidation);
    assert_eq!(
        provider.safe_summary(),
        "OpenAI response id is outside replay-state bounds"
    );
}

#[tokio::test]
async fn responses_rejects_an_empty_response_id() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("empty-response-id-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect_err("empty provider response identity must be a typed stream failure");
    let provider = error.provider_error().expect("provider error facts");
    assert_eq!(provider.kind(), ErrorKind::OutputValidation);
    assert_eq!(
        provider.safe_summary(),
        "OpenAI response id is outside replay-state bounds"
    );
}

#[tokio::test]
async fn responses_accepts_documented_queued_and_refusal_events() {
    let model = language_model(Capture::default()).await;
    let queued = model
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("queued-event-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect("documented queued event");
    assert_eq!(queued.visible_text(), "ready");

    let refusal = model
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("refusal-event-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect("documented refusal events");
    assert_eq!(refusal.visible_text(), "I cannot help with that.");
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
async fn responses_stream_authentication_error_is_not_retryable_as_a_server_failure() {
    let error = language_model(Capture::default())
        .await
        .complete(
            LanguageRequest::new(vec![
                Message::user_text("stream-auth-case").expect("message"),
            ])
            .expect("request"),
        )
        .await
        .expect_err("authentication event must fail");
    let provider = error.provider_error().expect("provider error facts");
    assert_eq!(provider.kind(), ErrorKind::Authentication);
    assert_eq!(provider.provider_code(), Some("invalid_api_key"));
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

#[tokio::test]
async fn responses_and_image_edit_share_credential_and_start_time_media_resolution() {
    let capture = Capture::default();
    let app = Router::new()
        .route("/v1/responses", post(endpoint))
        .route("/v1/images/edits", post(endpoint))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });

    let config = language_config(format!("http://{address}"));
    let transport = Arc::new(ReqwestTransport::new().expect("transport"));
    let digest = "0f4636c78f65d3639ece5a064b5ae753e3408614a14fb18ab4d7540d2c248543";
    let media =
        InMemoryMediaResolver::new(BTreeMap::from([(digest.to_owned(), vec![137, 80, 78, 71])]));
    let descriptor =
        MediaDescriptor::new(MediaKind::Image, "image/png", 4, digest).expect("image input");
    let language = TestLanguageModel {
        adapter: OpenAiResponsesAdapter::new(config.clone(), transport.clone()),
        media: media.clone(),
        media_admission_bytes: 4,
    }
    .complete(
        LanguageRequest::new(vec![
            Message::user(vec![
                MessageContent::Text {
                    text: "hello".to_owned(),
                },
                MessageContent::Image(descriptor.clone()),
            ])
            .expect("message"),
        ])
        .expect("request"),
    )
    .await
    .expect("language output");
    assert_eq!(language.visible_text(), "hello");

    let adapter = OpenAiImageAdapter::new(config, transport);
    let prepared = adapter
        .prepare(
            context(AiCapability::Image, media, 4),
            "gpt-image-1".into(),
            ImageRequest::new("edit", 1)
                .expect("request")
                .with_inputs(vec![descriptor], None)
                .expect("edit"),
        )
        .await
        .expect("prepare");
    let mut stream = prepared.start(AbortSignal::new()).await.expect("start");
    let mut assembler = ImageAssembler::new();
    while let Some(event) = stream.next().await {
        assembler
            .push(&event.expect("image event"))
            .expect("grammar");
    }
    assert_eq!(
        assembler.finish().expect("image output").images[0].bytes,
        [137, 80, 78, 71, 13, 10, 26, 10]
    );

    let calls = capture.0.lock().expect("capture");
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|(_, headers, _)| {
        headers
            .get("authorization")
            .is_some_and(|value| value == "Bearer openai-secret")
    }));
    assert_eq!(calls[0].1["transfer-encoding"], "chunked");
    assert_eq!(calls[1].0, "/v1/images/edits");
    assert!(
        calls[1]
            .2
            .windows(4)
            .any(|window| window == [137, 80, 78, 71])
    );
}
