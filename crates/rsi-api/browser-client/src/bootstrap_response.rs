//! Closed, bounded bootstrap responses use the shared transport validators.
use rsi_api_protocol::{ApiError, DeviceId, EndpointId, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    endpoint_id: EndpointId,
    device_id: DeviceId,
}

pub(crate) fn uncertain(error: ApiError, ticket: bool) -> ApiError {
    if ticket {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}

pub(crate) fn limit(status: u16) -> usize {
    if status == 200 { 1024 } else { 128 }
}

pub(crate) fn decode(
    status: u16,
    headers: &http::HeaderMap,
    bytes: &[u8],
    ticket: bool,
) -> Result<EndpointId> {
    // Keep well-formed remote rejection separate from failure to decode an outcome.
    let decoded = (|| {
        rsi_api_client::response_content_type(headers, "application/json")?;
        if bytes.len() > limit(status)
            || rsi_api_client::response_content_length(headers)?
                .is_some_and(|length| length != bytes.len())
        {
            return Err(ApiError::Invalid(
                "invalid bootstrap response length".into(),
            ));
        }
        if status != 200 {
            let error = rsi_api_client::decode_error(bytes)?;
            rsi_api_client::validate_error_status(status, &error)?;
            return Ok(Err(if matches!(error, ApiError::Backend(_)) {
                uncertain(error, ticket)
            } else {
                error
            }));
        }
        let identity: Identity = serde_json::from_slice(bytes)
            .map_err(|_| ApiError::Invalid("invalid bootstrap identity".into()))?;
        rsi_api_client::response_identity(headers, &identity.endpoint_id, None)?;
        let _ = identity.device_id;
        Ok(Ok(identity.endpoint_id))
    })();
    decoded.map_err(|error| uncertain(error, ticket))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers
    }

    #[test]
    fn definitive_http_errors_keep_their_identity() {
        for (status, code, expected) in [
            (401, "unauthorized", ApiError::Unauthorized),
            (429, "capacity", ApiError::Capacity),
            (409, "generation_retired", ApiError::ShuttingDown),
            (404, "unavailable", ApiError::Unavailable),
        ] {
            let bytes = format!("{{\"code\":\"{code}\"}}");
            let error = decode(status, &headers(), bytes.as_bytes(), true).unwrap_err();
            assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&expected)
            );
        }
    }

    #[test]
    fn malformed_or_lost_ticket_response_is_uncertain() {
        for (status, bytes) in [
            (200, b"{".as_slice()),
            (429, b"{\"code\":\"unauthorized\"}"),
            (500, b"{\"code\":\"backend\"}"),
        ] {
            assert!(matches!(
                decode(status, &headers(), bytes, true),
                Err(ApiError::OutcomeUnknown)
            ));
            assert!(!matches!(
                decode(status, &headers(), bytes, false),
                Err(ApiError::OutcomeUnknown)
            ));
        }
        assert!(matches!(
            decode(429, &headers(), &[b' '; 129], true),
            Err(ApiError::OutcomeUnknown)
        ));
        let mut head = headers();
        head.insert("content-length", "0".parse().unwrap());
        assert!(matches!(
            decode(401, &head, b"{\"code\":\"unauthorized\"}", true),
            Err(ApiError::OutcomeUnknown)
        ));
    }
}
