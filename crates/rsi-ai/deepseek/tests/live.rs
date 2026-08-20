use std::{sync::Arc, time::Duration};

use rsi_ai::{ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_deepseek::{DeepSeekAdapter, DeepSeekConfig};
use rsi_ai_protocol::{LanguageRequest, LanguageSettings, Message};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::ReqwestTransport;

#[tokio::test]
#[ignore = "requires an explicit DEEPSEEK_API_KEY and spends live API quota"]
async fn deepseek_v4_flash_streams_a_real_completion() {
    let key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set explicitly");
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::default(),
        Arc::new(ReqwestTransport::new().expect("transport")),
    )
    .expect("adapter");
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("deepseek.live", key)
            .expect("credential")
            .build(),
    )
    .register(
        ProviderRegistration::builder("deepseek-live", "deepseek")
            .expect("registration")
            .with_credential(
                CredentialRequirement::new("deepseek.live", std::iter::empty::<String>())
                    .expect("requirement"),
            )
            .with_language(adapter)
            .build()
            .expect("provider"),
    )
    .expect("register")
    .build()
    .expect("registry");
    let request = LanguageRequest::new(vec![
        Message::user_text("Reply with the exact text LIVE_OK and nothing else.").expect("message"),
    ])
    .expect("request")
    .with_settings(
        LanguageSettings::default()
            .with_max_output_tokens(512)
            .expect("settings"),
    )
    .expect("request settings");

    let output = tokio::time::timeout(
        Duration::from_mins(2),
        registry
            .language(ModelRef::new("deepseek-live", "deepseek-v4-flash").expect("model"))
            .expect("language")
            .complete(request),
    )
    .await
    .expect("live DeepSeek request timed out")
    .expect("live DeepSeek completion");

    assert_eq!(output.visible_text().trim(), "LIVE_OK");
    assert!(
        output
            .usage
            .as_ref()
            .is_some_and(|usage| usage.input_tokens > 0 && usage.output_tokens > 0)
    );
}
