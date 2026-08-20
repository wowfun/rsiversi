use std::sync::Arc;

use axum::{Router, routing::post};
use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_deepseek::{DeepSeekAdapter, DeepSeekConfig};
use rsi_ai_protocol::{LanguageRequest, LanguageSettings, Message, ReasoningEffort};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::ReqwestTransport;

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
        DeepSeekConfig::with_endpoint(format!("http://{address}")).expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    )
    .expect("adapter");
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("deepseek", "test")
            .expect("credential")
            .build(),
    )
    .register(
        ProviderRegistration::builder("deepseek", "deepseek")
            .expect("registration")
            .with_credential(
                CredentialRequirement::new("deepseek", ["DEEPSEEK_API_KEY"])
                    .expect("credential requirement"),
            )
            .with_language(adapter)
            .build()
            .expect("provider"),
    )
    .expect("register")
    .build()
    .expect("registry");
    let output = registry
        .language(ModelRef::new("deepseek", "deepseek-chat").expect("model"))
        .expect("language")
        .complete(
            LanguageRequest::new(vec![Message::user_text("hi").expect("message")])
                .expect("request"),
        )
        .await
        .expect("complete");
    assert_eq!(output.visible_text(), "ok");
}

#[tokio::test]
async fn deepseek_rejects_unsupported_settings_during_prepare() {
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::with_endpoint("http://127.0.0.1:9").expect("config"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    )
    .expect("adapter");
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("deepseek", "test")
            .expect("credential")
            .build(),
    )
    .register(
        ProviderRegistration::builder("deepseek", "deepseek")
            .expect("registration")
            .with_credential(
                CredentialRequirement::new("deepseek", ["DEEPSEEK_API_KEY"])
                    .expect("credential requirement"),
            )
            .with_language(adapter)
            .build()
            .expect("provider"),
    )
    .expect("register")
    .build()
    .expect("registry");
    let request = LanguageRequest::new(vec![Message::user_text("hi").expect("message")])
        .expect("request")
        .with_settings(LanguageSettings::default().with_reasoning_effort(ReasoningEffort::High))
        .expect("request settings");
    let error = registry
        .language(ModelRef::new("deepseek", "deepseek-reasoner").expect("model"))
        .expect("language")
        .complete(request)
        .await
        .expect_err("unsupported setting");
    assert_eq!(error.code(), "provider.unsupported");
    assert!(error.to_string().contains("reasoning_effort"));
}
