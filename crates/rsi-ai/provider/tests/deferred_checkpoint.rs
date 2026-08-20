use rsi_ai_protocol::ProviderExtension;
use rsi_ai_provider::{
    Capability, DeferredLanguageCheckpoint, DeferredStatus, PreparedCallSnapshot, RetryPolicy,
};
use serde_json::json;

fn call() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "deployment:1".to_owned(),
        deployment_id: "deployment".to_owned(),
        provider_family: "provider".to_owned(),
        capability: Capability::Language,
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
    ProviderExtension {
        namespace: "provider.deferred".to_owned(),
        version: 0,
        value: json!({"open":[]}),
    }
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
        .advance(DeferredStatus::InProgress, true, 4, Some(state()))
        .expect("advance");
    assert!(
        checkpoint
            .advance(DeferredStatus::InProgress, true, 4, Some(state()))
            .is_err()
    );
    assert!(checkpoint.observe_status(DeferredStatus::Queued).is_err());
    checkpoint
        .observe_status(DeferredStatus::Completed)
        .expect("terminal");
    assert!(checkpoint.observe_status(DeferredStatus::Failed).is_err());
}

#[test]
fn deferred_checkpoint_json_is_closed_and_validated_after_decode() {
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
    value["stream_created"] = json!(true);
    let decoded: DeferredLanguageCheckpoint = serde_json::from_value(value).expect("shape");
    assert!(decoded.validate().is_err());

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
    let decoded: DeferredLanguageCheckpoint = serde_json::from_value(value).expect("shape");
    assert!(decoded.validate().is_err());
}
