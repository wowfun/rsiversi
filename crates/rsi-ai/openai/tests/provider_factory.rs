use rsi_ai::LanguageRouterFactory;
use rsi_ai_image::ImageRouterFactory;
use rsi_ai_openai::OpenAiFactory;
use rsi_ai_protocol::ImageEvent;
use rsi_ai_protocol::{ImageCallContract, ImageRequest, LanguageCallContract, ModelRef};
use rsi_ai_provider::{ImageRegistrarContract, ProviderRegistration, RegistrationGate};
use rsi_ai_testkit::ScriptedImageAdapter;
use rsi_credentials_protocol::{CredentialRef, CredentialsAdminContract, SecretValue};
use rsi_credentials_testkit::MemoryCredentialsFactory;
use rsi_meta::{FiberState, ResolvedFactory, Runtime, UpdateMode};
use serde_json::{Value, json};
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

fn provider_config(deployment: &str) -> Value {
    json!({
        "deployment": deployment,
        "credential": {
            "owner": "rsi.ai.openai",
            "slot": "api"
        },
        "endpoint": "http://127.0.0.1:9",
        "language": true,
        "image": true,
        "language_models": {
            "gpt-5": {
                "context_window_tokens": 200_000,
                "default_output_reserve_tokens": 4096,
                "max_output_reserve_tokens": 32768
            }
        }
    })
}

async fn foundation(runtime: &Runtime) {
    runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let admin = runtime
        .root()
        .lookup_local::<CredentialsAdminContract>()
        .unwrap();
    admin
        .set(
            &CredentialRef::new("rsi.ai.openai", "api").unwrap(),
            SecretValue::new("test-secret").unwrap(),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("rsi.ai.language", Arc::new(LanguageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("rsi.ai.image", Arc::new(ImageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn one_provider_generation_publishes_and_withdraws_both_facets() {
    let runtime = Runtime::default();
    foundation(&runtime).await;
    let provider = runtime
        .root()
        .apply(
            linked("rsi.ai.provider.openai", Arc::new(OpenAiFactory::default())),
            provider_config("openai"),
        )
        .await
        .unwrap();
    assert_eq!(provider.snapshot().state, FiberState::Active);

    let language = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let image = runtime.root().lookup_local::<ImageCallContract>().unwrap();
    let language_model = ModelRef::new("openai", "gpt-5").unwrap();
    assert_eq!(
        language
            .describe(&language_model)
            .unwrap()
            .context_window_tokens(),
        200_000
    );
    let language_generation = language
        .prepare(
            language_model,
            rsi_ai_protocol::LanguageRequest::new(vec![
                rsi_ai_protocol::Message::user_text("hello").unwrap(),
            ])
            .unwrap(),
        )
        .await
        .unwrap()
        .snapshot()
        .config_generation;
    let image_generation = image
        .prepare(
            ModelRef::new("openai", "gpt-image-1").unwrap(),
            ImageRequest::new("cat", 1).unwrap(),
        )
        .await
        .unwrap()
        .snapshot()
        .config_generation;
    assert_eq!(language_generation, image_generation);

    assert!(provider.dispose().await.is_clean());
    assert!(
        language
            .describe(&ModelRef::new("openai", "gpt-5").unwrap())
            .is_err()
    );
    assert!(
        image
            .prepare(
                ModelRef::new("openai", "gpt-image-1").unwrap(),
                ImageRequest::new("cat", 1).unwrap(),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn later_facet_collision_rolls_back_the_earlier_hidden_route() {
    let runtime = Runtime::default();
    foundation(&runtime).await;
    let image_registrar = runtime
        .root()
        .lookup_local::<ImageRegistrarContract>()
        .unwrap();
    let existing = Arc::new(
        ProviderRegistration::builder("collision", "scripted")
            .unwrap()
            .with_config_generation(99)
            .with_image(ScriptedImageAdapter::new(vec![ImageEvent::Finished]))
            .build()
            .unwrap(),
    );
    let gate = RegistrationGate::new();
    let existing_lease = image_registrar
        .register_image(existing, gate.clone())
        .unwrap();
    gate.commit();

    let failed = runtime
        .root()
        .apply(
            linked("rsi.ai.provider.openai", Arc::new(OpenAiFactory::default())),
            provider_config("collision"),
        )
        .await
        .unwrap();
    assert!(matches!(failed.snapshot().state, FiberState::Failed(_)));
    let language = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    assert!(
        language
            .describe(&ModelRef::new("collision", "gpt-5").unwrap())
            .is_err(),
        "the hidden Language reservation must be withdrawn when Image registration fails"
    );
    drop(existing_lease);
}
