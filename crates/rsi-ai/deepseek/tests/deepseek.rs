use std::sync::Arc;

use axum::{Router, routing::post};
use rsi_ai_deepseek::{DeepSeekAdapter, DeepSeekConfig};
use rsi_ai_protocol::{
    LanguageModelLimits, LanguageRequest, LanguageSettings, Message, ReasoningEffort,
};
use rsi_ai_provider::MissingMediaResolver;
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
async fn deepseek_uses_its_chat_path_and_requires_the_done_sentinel() {
    let app = Router::new().route(
        "/chat/completions",
        post(|| async {
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
        LanguageRequest::new(vec![Message::user_text("hi").expect("message")]).expect("request"),
    )
    .await
    .expect("complete");
    assert_eq!(output.visible_text(), "ok");
}

#[tokio::test]
async fn deepseek_rejects_unsupported_settings_during_prepare() {
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::with_endpoint("http://127.0.0.1:9")
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
