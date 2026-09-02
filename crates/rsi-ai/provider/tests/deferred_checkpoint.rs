use rsi_ai_protocol::{AiCapability, PreparedCallSnapshot, ProviderExtension, RetryPolicy};
use rsi_ai_provider::{DeferredLanguageCheckpoint, DeferredStatus};
use serde_json::json;

fn call() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "deployment:1".to_owned(),
        deployment_id: "deployment".to_owned(),
        provider_family: "provider".to_owned(),
        capability: AiCapability::Language,
        model: "model".to_owned(),
        protocol: "responses".to_owned(),
        transport: "https".to_owned(),
        endpoint_fingerprint: "endpoint".to_owned(),
        config_generation: 7,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "0".repeat(64),
    }
}

fn state() -> ProviderExtension {
    ProviderExtension::new("provider.deferred", 0, json!({"open":[]})).unwrap()
}

#[test]
fn deferred_checkpoint_enforces_monotonic_cursor_and_terminal_status() {
    let mut checkpoint = DeferredLanguageCheckpoint::new(
        call(),
        "operation-1",
        DeferredStatus::Queued,
        Some(state()),
    )
    .expect("checkpoint");
    checkpoint
        .advance(DeferredStatus::InProgress, false, 4, Some(state()))
        .expect("advance");
    assert!(
        checkpoint
            .advance(DeferredStatus::InProgress, false, 4, Some(state()))
            .is_err()
    );
    assert!(checkpoint.observe_status(DeferredStatus::Queued).is_err());
    checkpoint
        .observe_status(DeferredStatus::Completed)
        .expect("terminal");
    assert!(!checkpoint.event_stream_terminal());
    checkpoint
        .advance(DeferredStatus::Completed, true, 5, Some(state()))
        .expect("terminal event cursor");
    assert!(checkpoint.event_stream_terminal());
    assert!(checkpoint.observe_status(DeferredStatus::Failed).is_err());
}

#[test]
fn deferred_checkpoint_json_is_closed_and_revalidated_during_decode() {
    let checkpoint = DeferredLanguageCheckpoint::new(
        call(),
        "operation-1",
        DeferredStatus::Queued,
        Some(state()),
    )
    .expect("checkpoint");
    let mut value = serde_json::to_value(checkpoint).expect("JSON");
    value["unexpected"] = json!(true);
    assert!(serde_json::from_value::<DeferredLanguageCheckpoint>(value).is_err());

    let mut value = serde_json::to_value(
        DeferredLanguageCheckpoint::new(
            call(),
            "operation-1",
            DeferredStatus::Queued,
            Some(state()),
        )
        .expect("checkpoint"),
    )
    .expect("JSON");
    value["event_stream_terminal"] = json!(true);
    assert!(serde_json::from_value::<DeferredLanguageCheckpoint>(value).is_err());

    let mut value = serde_json::to_value(
        DeferredLanguageCheckpoint::new(
            call(),
            "operation-1",
            DeferredStatus::Queued,
            Some(state()),
        )
        .expect("checkpoint"),
    )
    .expect("JSON");
    value["call"]["credential_source"] = json!({
        "id":"contains a space",
        "source":"explicit",
        "environment_variable":"NOT_ALLOWED_HERE"
    });
    assert!(serde_json::from_value::<DeferredLanguageCheckpoint>(value).is_err());
}
