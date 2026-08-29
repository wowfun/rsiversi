use std::{
    collections::BTreeMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
};
use futures_util::StreamExt as _;
use rsi_ai_openai_compatible::{ChatCompletionsAdapter, ChatCompletionsConfig};
use rsi_ai_protocol::{
    ContentBlock, ErrorKind, ErrorPhase, LanguageEvent, LanguageModelLimits, LanguageRequest,
    LanguageSettings, MAX_CONTENT_BLOCKS, MediaDescriptor, MediaKind, Message, MessageContent,
    ReasoningEffort, ResponseFormat, ToolCall, ToolCallKind,
};
use rsi_ai_provider::{AbortSignal, LanguageAdapter, MediaResolver, MissingMediaResolver};
use rsi_ai_testkit::{InMemoryMediaResolver, complete_language, language_context};
use rsi_ai_transport::ReqwestTransport;
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Option<(HeaderMap, Value)>>>);

fn model_limits() -> LanguageModelLimits {
    LanguageModelLimits::new(128_000, 4_096, 16_384).expect("model limits")
}

fn credential() -> ResolvedCredential {
    ResolvedCredential {
        secret: SecretValue::new("super-secret").expect("secret"),
        source: CredentialSource::Keyring,
    }
}

async fn complete(
    adapter: &ChatCompletionsAdapter,
    request: LanguageRequest,
    media: Arc<dyn MediaResolver>,
) -> Result<rsi_ai_protocol::LanguageOutput, rsi_ai_testkit::LanguageRunError> {
    complete_language(
        adapter,
        language_context(
            "chat",
            "openai-compatible",
            "fixture-model",
            Some(credential()),
            media,
            0,
        ),
        "fixture-model",
        request,
    )
    .await
}

#[test]
fn chat_describe_uses_the_exact_configured_model_capacity() {
    let adapter = ChatCompletionsAdapter::new(
        ChatCompletionsConfig::new("http://127.0.0.1:9")
            .and_then(|config| config.with_model_profile("small", model_limits()))
            .and_then(|config| {
                config.with_model_profile(
                    "large",
                    LanguageModelLimits::new(1_000_000, 8_192, 65_536).expect("large limits"),
                )
            })
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );

    let small = adapter.describe("small").expect("small profile");
    let large = adapter.describe("large").expect("large profile");
    assert_eq!(small.context_window_tokens(), 128_000);
    assert_eq!(small.max_output_reserve_tokens(), 16_384);
    assert_eq!(large.context_window_tokens(), 1_000_000);
    assert_eq!(large.max_output_reserve_tokens(), 65_536);
    assert_eq!(
        adapter
            .describe("unknown")
            .expect_err("unknown model")
            .kind(),
        ErrorKind::InvalidRequest
    );
}

async fn chat(State(capture): State<Capture>, headers: HeaderMap, body: String) -> Response {
    *capture.0.lock().expect("capture lock") =
        Some((headers, serde_json::from_str(&body).expect("request JSON")));
    let events = [
        json!({"choices":[{"delta":{"role":"assistant","content":null,"reasoning_content":"think"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"content":"answer"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"q\":"}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"rust\"}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_cache_hit_tokens":3}}),
    ];
    let mut body = String::new();
    for event in events {
        writeln!(&mut body, "data: {event}\n").expect("writing to String cannot fail");
    }
    body.push_str("data: [DONE]\n\n");
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("response")
}

async fn too_many_tool_calls() -> Response {
    let mut body = String::new();
    for index in 0..=MAX_CONTENT_BLOCKS {
        writeln!(
            &mut body,
            "data: {}\n",
            json!({
                "choices":[{
                    "delta":{"tool_calls":[{
                        "index":index,
                        "id":format!("call-{index}"),
                        "type":"function",
                        "function":{"name":"lookup","arguments":"{}"}
                    }]},
                    "finish_reason":null
                }]
            })
        )
        .expect("writing to String cannot fail");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("response")
}

#[tokio::test]
async fn chat_stream_rejects_tool_state_before_exceeding_content_bounds() {
    let app = Router::new().route("/v1/chat/completions", post(too_many_tool_calls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let adapter = ChatCompletionsAdapter::new(
        ChatCompletionsConfig::new(format!("http://{address}"))
            .and_then(|config| config.with_model_profile("fixture-model", model_limits()))
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let prepared = adapter
        .prepare(
            language_context(
                "chat",
                "openai-compatible",
                "fixture-model",
                Some(credential()),
                Arc::new(MissingMediaResolver),
                0,
            ),
            "fixture-model".into(),
            LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap(),
        )
        .await
        .expect("prepare");
    let mut stream = prepared.start(AbortSignal::new()).await.expect("start");
    let mut starts = 0;
    let mut failure = None;
    while let Some(event) = stream.next().await {
        match event.expect("adapter event") {
            LanguageEvent::ContentStarted { .. } => starts += 1,
            LanguageEvent::Failed { error, .. } => {
                failure = Some(error);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(starts, MAX_CONTENT_BLOCKS);
    let failure = failure.expect("translator must terminate with a bounded failure");
    assert_eq!(failure.kind(), ErrorKind::OutputValidation);
}

#[tokio::test]
async fn chat_prepare_rejects_freeform_calls_retained_in_history() {
    let adapter = ChatCompletionsAdapter::new(
        ChatCompletionsConfig::new("http://127.0.0.1:9")
            .and_then(|config| config.with_model_profile("fixture-model", model_limits()))
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let request = LanguageRequest::new(vec![
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "custom-call".to_owned(),
            name: "apply_patch".to_owned(),
            arguments: "*** Begin Patch\n*** End Patch".to_owned(),
            kind: ToolCallKind::Freeform,
        })])
        .expect("assistant history"),
        Message::tool_result(
            "custom-call",
            vec![MessageContent::Text {
                text: "Done".to_owned(),
            }],
            false,
        )
        .expect("tool result"),
    ])
    .expect("request");
    let error = complete(&adapter, request, Arc::new(MissingMediaResolver))
        .await
        .expect_err("freeform history must fail before provider I/O");
    let provider = error.provider_error().expect("structured provider error");
    assert_eq!(provider.kind(), ErrorKind::Unsupported);
    assert_eq!(provider.phase(), ErrorPhase::Prepare);
}

#[tokio::test]
async fn chat_prepare_rejects_nonadjacent_tool_results() {
    let adapter = ChatCompletionsAdapter::new(
        ChatCompletionsConfig::new("http://127.0.0.1:9")
            .and_then(|config| config.with_model_profile("fixture-model", model_limits()))
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let request = LanguageRequest::new(vec![
        Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "call-1".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
            kind: ToolCallKind::Function,
        })])
        .expect("assistant history"),
        Message::user_text("interposed user message").expect("user message"),
        Message::tool_result(
            "call-1",
            vec![MessageContent::Text {
                text: "late result".to_owned(),
            }],
            false,
        )
        .expect("tool result"),
    ])
    .expect("provider-neutral history");

    let error = complete(&adapter, request, Arc::new(MissingMediaResolver))
        .await
        .expect_err("Chat history must fail before provider I/O");
    let provider = error.provider_error().expect("structured provider error");
    assert_eq!(provider.kind(), ErrorKind::InvalidRequest);
    assert_eq!(provider.phase(), ErrorPhase::Prepare);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end test proves the full normalized stream.
async fn chat_adapter_preserves_reasoning_tools_usage_and_redacts_auth() {
    let capture = Capture::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });

    let adapter = ChatCompletionsAdapter::new(
        ChatCompletionsConfig::new(format!("http://{address}"))
            .and_then(|config| config.with_model_profile("deepseek-reasoner", model_limits()))
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let image = [137, 80, 78, 71];
    let image_digest = "0f4636c78f65d3639ece5a064b5ae753e3408614a14fb18ab4d7540d2c248543";
    let media =
        InMemoryMediaResolver::new(BTreeMap::from([(image_digest.to_owned(), image.to_vec())]));

    let prior = Message::assistant(vec![
        MessageContent::Reasoning {
            text: "prior thought".to_owned(),
            evidence: None,
        },
        MessageContent::ToolCall(ToolCall {
            id: "old-call".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
            kind: rsi_ai_protocol::ToolCallKind::Function,
        }),
        MessageContent::ToolCall(ToolCall {
            id: "old-call-2".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
            kind: rsi_ai_protocol::ToolCallKind::Function,
        }),
    ])
    .expect("assistant history");
    let output = complete_language(
        &adapter,
        language_context(
            "deepseek",
            "openai-compatible",
            "deepseek-reasoner",
            Some(credential()),
            Arc::new(media),
            4,
        ),
        "deepseek-reasoner",
        LanguageRequest::new(vec![
            Message::user(vec![
                MessageContent::Text {
                    text: "hello".to_owned(),
                },
                MessageContent::Image(
                    MediaDescriptor::new(MediaKind::Image, "image/png", 4, image_digest)
                        .expect("image"),
                ),
            ])
            .expect("user"),
            prior,
            Message::tool_result(
                "old-call",
                vec![
                    MessageContent::Text {
                        text: "tool text".to_owned(),
                    },
                    MessageContent::Image(
                        MediaDescriptor::new(MediaKind::Image, "image/png", 4, image_digest)
                            .expect("tool image"),
                    ),
                ],
                false,
            )
            .expect("rich tool result"),
            Message::tool_result(
                "old-call-2",
                vec![
                    MessageContent::Text {
                        text: "second tool text".to_owned(),
                    },
                    MessageContent::Image(
                        MediaDescriptor::new(MediaKind::Image, "image/png", 4, image_digest)
                            .expect("second tool image"),
                    ),
                ],
                false,
            )
            .expect("second rich tool result"),
        ])
        .expect("request")
        .with_settings(
            LanguageSettings::default()
                .with_max_output_tokens(321)
                .expect("tokens")
                .with_sampling(Some(0.4), Some(0.8))
                .expect("sampling")
                .with_seed(7)
                .with_stop(vec!["END".to_owned()])
                .expect("stop")
                .with_reasoning_effort(ReasoningEffort::Medium),
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
    .expect("completion");

    assert_eq!(
        output.content,
        vec![
            ContentBlock::Reasoning {
                text: "think".to_owned()
            },
            ContentBlock::Text {
                text: "answer".to_owned()
            },
            ContentBlock::ToolCall(ToolCall {
                id: "call-1".to_owned(),
                name: "lookup".to_owned(),
                arguments: "{\"q\":\"rust\"}".to_owned(),
                kind: rsi_ai_protocol::ToolCallKind::Function,
            }),
        ]
    );
    let usage = output.usage.expect("usage");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.cache_read_tokens, Some(3));

    let (headers, body) = capture.0.lock().expect("capture").take().expect("call");
    assert_eq!(headers["authorization"], "Bearer super-secret");
    assert_eq!(headers["transfer-encoding"], "chunked");
    assert!(!headers.contains_key("content-length"));
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_tokens"], 321);
    assert_eq!(body["temperature"], 0.4);
    assert_eq!(body["top_p"], 0.8);
    assert_eq!(body["seed"], 7);
    assert_eq!(body["stop"], json!(["END"]));
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["const"],
        "\0rsi-media-0\0"
    );
    assert_eq!(body["messages"][1]["reasoning_content"], "prior thought");
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["messages"][2]["content"], "tool text");
    assert_eq!(body["messages"][3]["role"], "tool");
    assert_eq!(body["messages"][3]["content"], "second tool text");
    assert_eq!(body["messages"][4]["role"], "user");
    assert_eq!(body["messages"][5]["role"], "user");
    assert_eq!(
        body["messages"][4]["content"][0]["image_url"]["url"],
        "data:image/png;base64,iVBORw=="
    );
    assert_eq!(
        body["messages"][5]["content"][0]["image_url"]["url"],
        "data:image/png;base64,iVBORw=="
    );
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,iVBORw=="
    );
    assert!(!format!("{body:?}").contains("super-secret"));
}
