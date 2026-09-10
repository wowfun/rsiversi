use std::sync::Arc;

use axum::{Router, routing::post};
use rsi_ai_deepseek::{DeepSeekAdapter, DeepSeekConfig, DeepSeekProtocol};
use rsi_ai_protocol::{
    LanguageModelLimits, LanguageRequest, LanguageSettings, Message, ReasoningEffort,
};
use rsi_ai_provider::{LanguageAdapter, MissingMediaResolver};
use rsi_ai_testkit::{complete_language, language_context};
use rsi_ai_transport::ReqwestTransport;
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};

fn context(model: &str) -> rsi_ai_provider::PrepareContext {
    language_context(
        "deepseek",
        "deepseek",
        model,
        Some(ResolvedCredential {
            secret: SecretValue::new("test").expect("secret"),
            source: CredentialSource::Keyring,
        }),
        Arc::new(MissingMediaResolver),
        0,
    )
}

#[tokio::test]
async fn responses_rejects_media_and_unknown_custom_tools_before_dispatch() {
    use rsi_ai_protocol::{
        FreeformFormat, FreeformToolDefinition, MediaDescriptor, MediaKind, MessageContent,
        ToolCall, ToolCallKind, ToolDefinition,
    };
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::with_endpoint("http://127.0.0.1:9")
            .unwrap()
            .with_model_profile(
                "model",
                LanguageModelLimits::new(128_000, 4096, 16384).unwrap(),
            )
            .unwrap(),
        Arc::new(ReqwestTransport::new().unwrap()),
    );
    let image = MessageContent::Image(
        MediaDescriptor::new(MediaKind::Image, "image/png", 1, "1".repeat(64)).unwrap(),
    );
    let audio = MessageContent::Audio(
        MediaDescriptor::new(MediaKind::Audio, "audio/wav", 1, "2".repeat(64)).unwrap(),
    );
    let call = ToolCall {
        id: "call".into(),
        name: "custom".into(),
        arguments: "input".into(),
        kind: ToolCallKind::Freeform,
    };
    let tool = |name| {
        ToolDefinition::new(name, "Custom tool", serde_json::json!({"type":"object"}))
            .unwrap()
            .with_freeform(
                FreeformToolDefinition::new(FreeformFormat::Lark, "start: /.+/").unwrap(),
            )
            .unwrap()
    };
    let base = LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap();
    let requests = [
        LanguageRequest::new(vec![Message::user(vec![image.clone()]).unwrap()]).unwrap(),
        LanguageRequest::new(vec![Message::user(vec![audio]).unwrap()]).unwrap(),
        LanguageRequest::new(vec![
            Message::assistant(vec![MessageContent::ToolCall(ToolCall {
                arguments: "{}".into(),
                kind: ToolCallKind::Function,
                ..call.clone()
            })])
            .unwrap(),
            Message::tool_result("call", vec![image], false).unwrap(),
        ])
        .unwrap(),
        LanguageRequest::new(vec![
            Message::assistant(vec![MessageContent::ToolCall(call)]).unwrap(),
        ])
        .unwrap(),
        base.clone()
            .with_tools(vec![tool("custom")], rsi_ai_protocol::ToolChoice::Auto)
            .unwrap(),
    ];
    for request in requests {
        let failure = adapter
            .prepare(context("model"), "model".into(), request)
            .await
            .err()
            .unwrap();
        assert_eq!(failure.kind(), rsi_ai_protocol::ErrorKind::Unsupported);
        assert_eq!(
            failure.dispatch_status(),
            rsi_ai_protocol::DispatchStatus::NotStarted
        );
    }
    adapter
        .validate_request(
            "model",
            &base
                .with_tools(vec![tool("apply_patch")], rsi_ai_protocol::ToolChoice::Auto)
                .unwrap(),
        )
        .unwrap();
}

#[tokio::test]
async fn stateless_responses_rejects_remote_state_before_dispatch() {
    use rsi_ai_openai::{OpenAiConfig, OpenAiResponsesAdapter, ResponsesState};
    let config = OpenAiConfig::new("http://127.0.0.1:9")
        .unwrap()
        .with_model_profile(
            "model",
            LanguageModelLimits::new(128_000, 4096, 16384).unwrap(),
        )
        .unwrap()
        .with_responses_options(
            "/responses",
            ResponsesState::Stateless,
            rsi_ai_protocol::MessageRole::System,
        )
        .unwrap();
    let adapter = OpenAiResponsesAdapter::new(config, Arc::new(ReqwestTransport::new().unwrap()));
    let request = LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap();
    let failure = adapter
        .prepare_deferred(context("model"), "model".into(), request.clone())
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind(), rsi_ai_protocol::ErrorKind::Unsupported);
    assert_eq!(
        failure.dispatch_status(),
        rsi_ai_protocol::DispatchStatus::NotStarted
    );
    let replay = request
        .with_extensions(vec![
            rsi_ai_protocol::ProviderExtension::new(
                "openai.responses.replay",
                0,
                serde_json::json!({"response_id":"old"}),
            )
            .unwrap(),
        ])
        .unwrap();
    let failure = adapter
        .prepare(context("model"), "model".into(), replay)
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind(), rsi_ai_protocol::ErrorKind::Unsupported);
    assert_eq!(
        failure.dispatch_status(),
        rsi_ai_protocol::DispatchStatus::NotStarted
    );
}

#[tokio::test]
async fn deepseek_defaults_to_stateless_responses_with_plain_reasoning() {
    let received = Arc::new(std::sync::Mutex::new(None));
    let capture = received.clone();
    let app = Router::new().route("/responses", post(move |axum::Json(body): axum::Json<serde_json::Value>| async move {
        *capture.lock().unwrap() = Some(body);
        concat!(
            "data: {\"type\":\"response.created\",\"sequence_number\":0}\n\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"item_id\":\"reason\",\"delta\":\"reasoning\",\"sequence_number\":1}\n\n",
            "data: {\"type\":\"response.reasoning_text.done\",\"item_id\":\"reason\",\"text\":\"reasoning\",\"sequence_number\":2}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"answer\",\"delta\":\"ok\",\"sequence_number\":3}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"stateless-id\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}},\"sequence_number\":4}\n\n"
        )
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::with_endpoint(format!("http://{address}"))
            .unwrap()
            .with_model_profile(
                "deepseek-v4-flash",
                LanguageModelLimits::new(128_000, 4096, 16384).unwrap(),
            )
            .unwrap(),
        Arc::new(ReqwestTransport::new().unwrap()),
    );
    let output = complete_language(
        &adapter,
        context("deepseek-v4-flash"),
        "deepseek-v4-flash",
        LanguageRequest::new(vec![
            Message::developer_text("Sampled context").unwrap(),
            Message::assistant(vec![rsi_ai_protocol::MessageContent::Reasoning {
                text: "prior plain reasoning".into(),
                evidence: None,
            }])
            .unwrap(),
            Message::user_text("hi").unwrap(),
        ])
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(output.visible_text(), "ok");
    assert!(output.replay.is_none());
    assert!(output.content.iter().any(|block| matches!(block, rsi_ai_protocol::ContentBlock::Reasoning { text, .. } if text == "reasoning")));
    let body = received.lock().unwrap().take().unwrap();
    assert_eq!(body["input"][0]["role"], "system");
    assert_eq!(
        body["input"][1],
        serde_json::json!({
            "type":"reasoning", "content":[{"type":"reasoning_text", "text":"prior plain reasoning"}]
        })
    );
    assert!(body.get("previous_response_id").is_none());
    assert!(body.get("background").is_none());
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn deepseek_uses_its_chat_path_and_requires_the_done_sentinel() {
    let received = Arc::new(std::sync::Mutex::new(None));
    let capture = received.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| async move {
            *capture.lock().unwrap() = Some(body);
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });

    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::with_endpoint(format!("http://{address}"))
            .map(|config| config.with_protocol(DeepSeekProtocol::ChatCompletions))
            .and_then(|config| {
                config.with_model_profile(
                    "deepseek-chat",
                    LanguageModelLimits::new(128_000, 4_096, 16_384).expect("model limits"),
                )
            })
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let output = complete_language(
        &adapter,
        context("deepseek-chat"),
        "deepseek-chat",
        LanguageRequest::new(vec![
            Message::system_text("Root instructions").unwrap(),
            Message::user_text("hi").unwrap(),
            Message::developer_text("Sampled time: 42").unwrap(),
        ])
        .expect("request"),
    )
    .await
    .expect("complete");
    assert_eq!(output.visible_text(), "ok");
    assert_eq!(
        received.lock().unwrap().as_ref().unwrap()["messages"],
        serde_json::json!([
            {"role":"system", "content":[{"type":"text", "text":"Root instructions"}]},
            {"role":"user", "content":[{"type":"text", "text":"hi"}]},
            {"role":"system", "content":[{"type":"text", "text":"Sampled time: 42"}]},
        ])
    );
}

#[tokio::test]
async fn deepseek_rejects_unsupported_settings_during_prepare() {
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::with_endpoint("http://127.0.0.1:9")
            .map(|config| config.with_protocol(DeepSeekProtocol::ChatCompletions))
            .and_then(|config| {
                config.with_model_profile(
                    "deepseek-reasoner",
                    LanguageModelLimits::new(128_000, 4_096, 16_384).expect("model limits"),
                )
            })
            .expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
    let request = LanguageRequest::new(vec![Message::user_text("hi").expect("message")])
        .expect("request")
        .with_settings(LanguageSettings::default().with_reasoning_effort(ReasoningEffort::High))
        .expect("request settings");
    let error = complete_language(
        &adapter,
        context("deepseek-reasoner"),
        "deepseek-reasoner",
        request,
    )
    .await
    .expect_err("unsupported setting");
    let provider = error.provider_error().expect("provider failure");
    assert_eq!(provider.kind().code(), "provider.unsupported");
    assert!(provider.to_string().contains("reasoning_effort"));
}
