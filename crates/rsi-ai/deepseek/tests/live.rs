use std::{sync::Arc, time::Duration};

use rsi_ai_deepseek::{DeepSeekAdapter, DeepSeekConfig};
use rsi_ai_protocol::{LanguageModelLimits, LanguageRequest, LanguageSettings, Message};
use rsi_ai_provider::MissingMediaResolver;
use rsi_ai_testkit::{complete_language, language_context};
use rsi_ai_transport::ReqwestTransport;
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};

#[tokio::test]
#[ignore = "requires an explicit DEEPSEEK_API_KEY and spends live API quota"]
async fn deepseek_v4_flash_streams_a_real_completion() {
    let key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set explicitly");
    let adapter = DeepSeekAdapter::new(
        DeepSeekConfig::default()
            .with_model_profile(
                "deepseek-v4-flash",
                LanguageModelLimits::new(
                    required_u32("DEEPSEEK_CONTEXT_WINDOW_TOKENS"),
                    required_u32("DEEPSEEK_DEFAULT_OUTPUT_RESERVE_TOKENS"),
                    required_u32("DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS"),
                )
                .expect("live model limits"),
            )
            .expect("live model profile"),
        Arc::new(ReqwestTransport::new().expect("transport")),
    );
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
        complete_language(
            &adapter,
            language_context(
                "deepseek-live",
                "deepseek",
                "deepseek-v4-flash",
                Some(ResolvedCredential {
                    secret: SecretValue::new(key).expect("credential"),
                    source: CredentialSource::Environment {
                        variable: "DEEPSEEK_API_KEY".into(),
                    },
                }),
                Arc::new(MissingMediaResolver),
                0,
            ),
            "deepseek-v4-flash",
            request,
        ),
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

fn required_u32(name: &str) -> u32 {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set explicitly"))
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a u32"))
}
