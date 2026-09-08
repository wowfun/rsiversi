use rsi_api_client::{
    finite_response_head, response_content_type, response_identity, validate_error_status,
};
use rsi_api_protocol::{ApiError, EndpointId, HostEpoch};

#[test]
fn identity_headers_reject_duplicate_or_changed_generations_before_payload_use() {
    let endpoint = EndpointId::from_bytes([1; 16]);
    let epoch = HostEpoch::from_bytes([2; 16]);
    let mut headers = http::HeaderMap::new();
    headers.insert("x-rsi-wire-version", "1".parse().unwrap());
    headers.insert("x-rsi-endpoint-id", endpoint.as_str().parse().unwrap());
    headers.insert("x-rsi-host-epoch", epoch.as_str().parse().unwrap());
    assert_eq!(response_identity(&headers, &endpoint, None).unwrap(), epoch);
    assert_eq!(
        response_identity(&headers, &endpoint, Some(&HostEpoch::from_bytes([3; 16]))),
        Err(ApiError::ShuttingDown)
    );
    assert!(response_identity(&headers, &EndpointId::from_bytes([4; 16]), None).is_err());
    for name in [
        "x-rsi-wire-version",
        "x-rsi-endpoint-id",
        "x-rsi-host-epoch",
    ] {
        let mut duplicate = headers.clone();
        duplicate.append(name, headers.get(name).unwrap().clone());
        assert!(response_identity(&duplicate, &endpoint, None).is_err());
    }
    headers.insert("x-rsi-host-epoch", "2, 2".parse().unwrap());
    assert!(response_identity(&headers, &endpoint, None).is_err());
}

#[test]
fn finite_and_sse_metadata_reject_contradictory_representations() {
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("content-length", "2".parse().unwrap());
    let head = finite_response_head(200, &headers).unwrap();
    assert_eq!(head.length, Some(2));
    assert!(!head.domain_error);
    assert!(finite_response_head(422, &headers).is_err());
    headers.insert(
        "content-type",
        "application/vnd.rsi.domain-error+json".parse().unwrap(),
    );
    assert!(finite_response_head(422, &headers).unwrap().domain_error);
    headers.append("content-length", "2".parse().unwrap());
    assert!(finite_response_head(422, &headers).is_err());
    headers.remove("content-length");
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    response_content_type(&headers, "text/event-stream").unwrap();
    headers.insert("content-encoding", "gzip".parse().unwrap());
    assert!(response_content_type(&headers, "text/event-stream").is_err());
    assert!(finite_response_head(200, &headers).is_err());
}

#[test]
fn transport_status_must_agree_with_a_closed_common_error() {
    for (status, error) in [
        (401, ApiError::Unauthorized),
        (429, ApiError::Capacity),
        (409, ApiError::ShuttingDown),
        (404, ApiError::Unavailable),
        (400, ApiError::Invalid(String::new())),
        (500, ApiError::OutcomeUnknown),
        (500, ApiError::Backend(String::new())),
    ] {
        validate_error_status(status, &error).unwrap();
        assert!(validate_error_status(200, &error).is_err());
    }
}
