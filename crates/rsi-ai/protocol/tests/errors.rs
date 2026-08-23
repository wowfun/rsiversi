use rsi_ai_protocol::{AiError, DispatchStatus, ErrorKind, ErrorPhase};

#[test]
fn context_limit_has_a_stable_provider_neutral_code() {
    assert_eq!(ErrorKind::ContextLimit.code(), "provider.context_limit");
    assert_eq!(
        ErrorKind::from_code("provider.context_limit"),
        Some(ErrorKind::ContextLimit)
    );
    let error = AiError::new(
        ErrorKind::ContextLimit,
        ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        "context limit",
    )
    .expect("error facts");
    let encoded = serde_json::to_value(&error).expect("serialize error");
    assert_eq!(encoded["kind"], "context_limit");
    assert_eq!(
        serde_json::from_value::<AiError>(encoded)
            .expect("deserialize error")
            .kind(),
        ErrorKind::ContextLimit
    );
}

#[test]
fn provider_error_revalidates_during_deserialization() {
    let error = AiError::new(
        ErrorKind::Server,
        ErrorPhase::FirstEvent,
        DispatchStatus::Dispatched,
        "safe",
    )
    .expect("error facts");
    let mut encoded = serde_json::to_value(error).unwrap();
    encoded["status"] = serde_json::json!(42);

    let error = serde_json::from_value::<AiError>(encoded)
        .expect_err("invalid HTTP status must not enter typed error facts");
    assert!(error.to_string().contains("HTTP status"), "{error}");
}

#[test]
fn provider_error_rejects_invalid_status_at_construction() {
    let error = AiError::new(
        ErrorKind::Server,
        ErrorPhase::FirstEvent,
        DispatchStatus::Dispatched,
        "safe",
    )
    .expect("error facts")
    .with_status(42)
    .expect_err("invalid HTTP status must fail at the builder boundary");
    assert!(error.to_string().contains("HTTP status"), "{error}");
}
