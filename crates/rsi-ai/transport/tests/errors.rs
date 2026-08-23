use bytes::Bytes;
use futures_util::stream;
use rsi_ai_protocol::{DispatchStatus, ErrorKind, ErrorPhase};
use rsi_ai_transport::{
    ByteStream, TransportError, provider_http_error, reclassify_context_limit,
    transport_body_error, transport_json_response_error, transport_stream_error,
};

fn body(value: &'static str) -> ByteStream {
    Box::pin(stream::iter([Ok(Bytes::from_static(value.as_bytes()))]))
}

#[tokio::test]
async fn shared_http_error_mapping_preserves_provider_code_and_sanitizes_summary() {
    let error = provider_http_error(
        429,
        body(r#"{"error":{"message":"slow\u0000down","code":"rate_limit"}}"#),
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await;

    assert_eq!(error.kind(), ErrorKind::RateLimited);
    assert_eq!(error.phase(), ErrorPhase::FirstEvent);
    assert_eq!(error.dispatch_status(), DispatchStatus::Dispatched);
    assert_eq!(error.status(), Some(429));
    assert_eq!(error.provider_code(), Some("rate_limit"));
    assert!(!error.safe_summary().contains('\0'));
}

#[tokio::test]
async fn shared_http_error_mapping_is_status_driven_and_preserves_provider_codes() {
    let ordinary_invalid_request = provider_http_error(
        400,
        body(r#"{"error":{"message":"bad request","code":"invalid_parameter"}}"#),
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await;
    assert_eq!(ordinary_invalid_request.kind(), ErrorKind::InvalidRequest);

    let misleading_code = provider_http_error(
        404,
        body(r#"{"error":{"message":"missing","code":"context_length_exceeded"}}"#),
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await;
    assert_eq!(misleading_code.kind(), ErrorKind::NotFound);
    assert_eq!(
        misleading_code.provider_code(),
        Some("context_length_exceeded")
    );
}

#[tokio::test]
async fn shared_context_limit_classification_preserves_validated_error_facts() {
    let error = provider_http_error(
        422,
        body(r#"{"error":{"message":"too long","code":"context_length_exceeded"}}"#),
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await
    .with_retry_after_ms(17);
    let classified = reclassify_context_limit(error);

    assert_eq!(classified.kind(), ErrorKind::ContextLimit);
    assert_eq!(classified.status(), Some(422));
    assert_eq!(classified.provider_code(), Some("context_length_exceeded"));
    assert_eq!(classified.retry_after_ms(), Some(17));
    assert_eq!(classified.safe_summary(), "too long");
}

#[tokio::test]
async fn shared_http_error_mapping_distinguishes_quota_and_expired_remote_work() {
    let quota = provider_http_error(
        402,
        body(r#"{"error":{"message":"quota"}}"#),
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await;
    assert_eq!(quota.kind(), ErrorKind::Quota);

    let expired = provider_http_error(
        404,
        body(r#"{"error":{"message":"expired"}}"#),
        ErrorPhase::DeferredPoll,
        "provider rejected the request",
    )
    .await;
    assert_eq!(expired.kind(), ErrorKind::RemoteExpired);
}

#[tokio::test]
async fn shared_http_error_mapping_never_panics_on_an_invalid_status_argument() {
    let error = provider_http_error(
        42,
        body(r#"{"error":{"message":"not HTTP"}}"#),
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await;

    assert_eq!(error.kind(), ErrorKind::Transport);
    assert_eq!(error.status(), None);
}

#[test]
fn stream_transport_mapping_preserves_cancellation_and_timeout() {
    let cancelled = transport_stream_error(TransportError::new(
        "http.cancelled",
        "HTTP response body was cancelled",
    ));
    assert_eq!(cancelled.kind(), ErrorKind::Cancelled);

    let timed_out = transport_stream_error(TransportError::new(
        "http.timeout",
        "HTTP request timed out",
    ));
    assert_eq!(timed_out.kind(), ErrorKind::Timeout);
}

#[test]
fn successful_response_limits_are_output_validation_not_transport_or_protocol() {
    let body = transport_body_error(TransportError::new(
        "http.body_too_large",
        "response body exceeds its byte bound",
    ));
    assert_eq!(body.kind(), ErrorKind::OutputValidation);
    assert_eq!(body.phase(), ErrorPhase::Assemble);

    let json = transport_json_response_error(TransportError::new(
        "json.extract_limit",
        "JSON item exceeds its byte bound",
    ));
    assert_eq!(json.kind(), ErrorKind::OutputValidation);
    assert_eq!(json.phase(), ErrorPhase::Assemble);

    let malformed = transport_json_response_error(TransportError::new(
        "json.extract",
        "JSON response is malformed",
    ));
    assert_eq!(malformed.kind(), ErrorKind::Protocol);
}
