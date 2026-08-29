use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, DeferredLanguageBatch, DeferredLanguageCheckpoint,
    DeferredStatus, LanguageAssembler, LanguageEvent, MAX_LANGUAGE_OUTPUT_BYTES, ModelRef,
    PreparedCallSnapshot, RetryPolicy,
};
use serde_json::json;

#[test]
fn runtime_contracts_revalidate_during_deserialization() {
    serde_json::from_value::<ModelRef>(json!({
        "deployment": "",
        "model": "gpt-5"
    }))
    .expect_err("empty deployment must fail");

    serde_json::from_value::<RetryPolicy>(json!({
        "max_retries": 17,
        "retryable_kinds": ["transport"],
        "initial_delay_ms": 1,
        "max_delay_ms": 10,
        "jitter_per_mille": 0
    }))
    .expect_err("unbounded retries must fail");

    let snapshot = json!({
        "call_id": "call-1",
        "deployment_id": "openai",
        "provider_family": "openai",
        "capability": "language",
        "model": "gpt-5",
        "protocol": "openai-responses",
        "transport": "http",
        "endpoint_fingerprint": "test",
        "config_generation": 0,
        "credential_source": null,
        "retry_policy": RetryPolicy::default(),
        "request_sha256": "0".repeat(64)
    });
    serde_json::from_value::<PreparedCallSnapshot>(snapshot.clone())
        .expect_err("zero provider generation must fail");

    let mut valid_snapshot = snapshot;
    valid_snapshot["config_generation"] = json!(1);
    serde_json::from_value::<DeferredLanguageCheckpoint>(json!({
        "call": valid_snapshot,
        "operation_id": "operation-1",
        "status": DeferredStatus::InProgress,
        "event_stream_terminal": true,
        "sequence_number": null,
        "provider_state": null
    }))
    .expect_err("terminal checkpoint without a sequence must fail during decode");
}

#[test]
fn deferred_batches_share_the_assembler_output_budget() {
    let snapshot = PreparedCallSnapshot {
        call_id: "call-1".to_owned(),
        deployment_id: "deployment".to_owned(),
        provider_family: "provider".to_owned(),
        capability: AiCapability::Language,
        model: "model".to_owned(),
        protocol: "protocol".to_owned(),
        transport: "transport".to_owned(),
        endpoint_fingerprint: "endpoint".to_owned(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "0".repeat(64),
    };
    let mut checkpoint =
        DeferredLanguageCheckpoint::new(snapshot, "operation-1", DeferredStatus::InProgress, None)
            .unwrap();
    checkpoint
        .advance(DeferredStatus::InProgress, false, 1, None)
        .unwrap();
    let half = "x".repeat(MAX_LANGUAGE_OUTPUT_BYTES / 2 + 1);
    let first = DeferredLanguageBatch::new(
        vec![
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text(half.clone()),
            },
        ],
        checkpoint.clone(),
    )
    .unwrap();
    checkpoint
        .advance(DeferredStatus::InProgress, false, 2, None)
        .unwrap();
    let second = DeferredLanguageBatch::new(
        vec![LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text(half),
        }],
        checkpoint,
    )
    .unwrap();

    let mut assembler = LanguageAssembler::new();
    for event in first.events() {
        assembler.push(event).expect("first batch remains bounded");
    }
    assert_eq!(
        assembler
            .push(&second.events()[0])
            .expect_err("second batch exceeds the complete stream budget")
            .code(),
        "stream.output_too_large"
    );
}
