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
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_openai_compatible::{ChatCompletionsAdapter, ChatCompletionsConfig};
use rsi_ai_protocol::{
    ContentBlock, LanguageRequest, LanguageSettings, MediaDescriptor, MediaKind, Message,
    MessageContent, ReasoningEffort, ResponseFormat, ToolCall,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_testkit::InMemoryMediaResolver;
use rsi_ai_transport::ReqwestTransport;
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Option<(HeaderMap, Value)>>>);

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
        ChatCompletionsConfig::new(format!("http://{address}")).expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let credential =
        CredentialRequirement::new("deepseek", ["DEEPSEEK_API_KEY"]).expect("requirement");
    let image = [137, 80, 78, 71];
    let image_digest = "0f4636c78f65d3639ece5a064b5ae753e3408614a14fb18ab4d7540d2c248543";
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("deepseek", "super-secret")
            .expect("credential")
            .build(),
    )
    .with_media_resolver(InMemoryMediaResolver::new(BTreeMap::from([(
        image_digest.to_owned(),
        image.to_vec(),
    )])))
    .register(
        ProviderRegistration::builder("deepseek", "deepseek")
            .expect("registration")
            .with_credential(credential)
            .with_language(adapter)
            .build()
            .expect("provider"),
    )
    .expect("register")
    .build()
    .expect("registry");

    let prior = Message::assistant(vec![
        MessageContent::Reasoning {
            text: "prior thought".to_owned(),
            evidence: None,
        },
        MessageContent::ToolCall(ToolCall {
            id: "old-call".to_owned(),
            name: "lookup".to_owned(),
            arguments: "{}".to_owned(),
        }),
    ])
    .expect("assistant history");
    let output = registry
        .language(ModelRef::new("deepseek", "deepseek-reasoner").expect("model"))
        .expect("language")
        .complete(
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
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,iVBORw=="
    );
    assert!(!format!("{body:?}").contains("super-secret"));
}
