use futures_util::StreamExt;
use rsi_ai::{ModelRef, Registry};
use std::{fmt, sync::Arc, time::Duration};

use rsi_ai_auth::{
    CredentialId, CredentialManager, CredentialRequirement, CredentialStore, SecretValue,
    StoreError,
};
use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase, FinishReason,
    LanguageEvent, LanguageRequest, Message,
};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_testkit::ScriptedLanguageAdapter;

#[derive(Debug)]
struct SlowStore;

impl CredentialStore for SlowStore {
    fn get(&self, _id: &CredentialId) -> Result<Option<SecretValue>, StoreError> {
        std::thread::sleep(Duration::from_millis(150));
        Ok(Some(SecretValue::new("slow-secret").expect("secret")))
    }

    fn set(&self, _id: &CredentialId, _secret: &SecretValue) -> Result<(), StoreError> {
        Ok(())
    }

    fn delete(&self, _id: &CredentialId) -> Result<(), StoreError> {
        Ok(())
    }
}

impl fmt::Display for SlowStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("slow store")
    }
}

#[tokio::test]
async fn registry_prepare_start_and_finish_use_one_exact_adapter_stream() {
    let adapter = ScriptedLanguageAdapter::new(vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("hello".to_owned()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        },
    ]);
    let registration = ProviderRegistration::builder("scripted", "scripted")
        .expect("registration")
        .with_credential(
            CredentialRequirement::new("scripted.key", ["SCRIPTED_API_KEY"]).expect("requirement"),
        )
        .with_language(adapter.clone())
        .build()
        .expect("provider");
    let credentials = CredentialManager::builder()
        .with_explicit("scripted.key", "never-serialize-this")
        .expect("credential")
        .build();
    let registry = Registry::builder(credentials)
        .register(registration)
        .expect("registration")
        .build()
        .expect("registry");

    let model = registry
        .language(ModelRef::new("scripted", "future-model").expect("model ref"))
        .expect("language model");

    let request =
        LanguageRequest::new(vec![Message::user_text("hi").expect("message")]).expect("request");
    let prepared = model.prepare(request).await.expect("prepare");
    assert_eq!(
        adapter.start_count(),
        0,
        "prepare must not start provider I/O"
    );
    let snapshot_json = serde_json::to_string(prepared.snapshot()).expect("snapshot JSON");
    assert!(!snapshot_json.contains("never-serialize-this"));
    assert!(snapshot_json.contains("future-model"));

    let mut generation = prepared.start().await.expect("start");
    let mut observed = Vec::new();
    while let Some(event) = generation.next().await {
        observed.push(event);
    }
    let output = generation.finish().expect("assembled output");
    assert_eq!(output.visible_text(), "hello");
    assert_eq!(adapter.start_count(), 1);
    assert_eq!(observed.len(), 4);
}

#[tokio::test]
async fn complete_preserves_structured_provider_error_facts() {
    let provider_error = AiError::new(
        ErrorKind::RateLimited,
        ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        "slow down",
    )
    .expect("error")
    .with_status(429)
    .with_retry_after_ms(2_500)
    .with_request_id("request-1")
    .expect("request id");
    let adapter = ScriptedLanguageAdapter::new(vec![LanguageEvent::Failed {
        error: provider_error.clone(),
        replay: None,
    }]);
    let registry = Registry::builder(CredentialManager::builder().build())
        .register(
            ProviderRegistration::builder("scripted", "scripted")
                .expect("registration")
                .with_language(adapter)
                .build()
                .expect("provider"),
        )
        .expect("register")
        .build()
        .expect("registry");
    let error = registry
        .language(ModelRef::new("scripted", "model").expect("model"))
        .expect("language")
        .complete(
            LanguageRequest::new(vec![Message::user_text("hi").expect("message")])
                .expect("request"),
        )
        .await
        .expect_err("provider failure");

    assert_eq!(error.provider_error(), Some(&provider_error));
}

#[tokio::test]
async fn exact_provider_selection_never_falls_back_to_another_registration() {
    let first = ScriptedLanguageAdapter::new(Vec::new());
    let registry = Registry::builder(CredentialManager::builder().build())
        .register(
            ProviderRegistration::builder("first", "scripted")
                .expect("registration")
                .with_language(first)
                .build()
                .expect("provider"),
        )
        .expect("register")
        .build()
        .expect("registry");

    let error = registry
        .language(ModelRef::new("missing", "model").expect("model ref"))
        .expect_err("no provider fallback");
    assert_eq!(error.code(), "registry.provider_not_found");
}

#[tokio::test(flavor = "current_thread")]
async fn persistent_credential_lookup_does_not_block_the_async_runtime() {
    let registration = ProviderRegistration::builder("scripted", "scripted")
        .expect("registration")
        .with_credential(
            CredentialRequirement::new("scripted.key", std::iter::empty::<&str>())
                .expect("requirement"),
        )
        .with_language(ScriptedLanguageAdapter::new(Vec::new()))
        .build()
        .expect("provider");
    let credentials = CredentialManager::builder()
        .with_store(Arc::new(SlowStore))
        .build();
    let registry = Registry::builder(credentials)
        .register(registration)
        .expect("registration")
        .build()
        .expect("registry");
    let model = registry
        .language(ModelRef::new("scripted", "model").expect("model ref"))
        .expect("language model");
    let request =
        LanguageRequest::new(vec![Message::user_text("hi").expect("message")]).expect("request");

    let started = std::time::Instant::now();
    let prepare = tokio::spawn(async move { model.prepare(request).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "a synchronous credential store blocked the async executor"
    );
    prepare.abort();
}
