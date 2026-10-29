use bytes::Bytes;
use rsi_api_protocol::ApiError;

pub(crate) fn json(error: &ApiError) -> Bytes {
    Bytes::from_static(match error {
        ApiError::UnknownOperation => b"{\"code\":\"unknown_operation\"}",
        ApiError::OperationRetiring => b"{\"code\":\"operation_retiring\"}",
        ApiError::ProtocolMismatch => b"{\"code\":\"protocol_mismatch\"}",
        ApiError::CallShapeMismatch => b"{\"code\":\"call_shape_mismatch\"}",
        ApiError::OutcomeUnknown => b"{\"code\":\"outcome_unknown\"}",
        ApiError::Unauthorized => b"{\"code\":\"unauthorized\"}",
        ApiError::Capacity => b"{\"code\":\"capacity\"}",
        ApiError::ShuttingDown => b"{\"code\":\"generation_retired\"}",
        ApiError::Unavailable => b"{\"code\":\"unavailable\"}",
        ApiError::Invalid(_) => b"{\"code\":\"invalid\"}",
        ApiError::Backend(_) | ApiError::Domain(_) => b"{\"code\":\"backend\"}",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;
    use http_body_util::BodyExt as _;
    use rsi_api_protocol::ByteBudget;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn finite_and_stream_errors_keep_exact_codes_statuses_and_redaction() {
        for (error, status, code) in [
            (
                ApiError::UnknownOperation,
                StatusCode::NOT_FOUND,
                "unknown_operation",
            ),
            (
                ApiError::OperationRetiring,
                StatusCode::CONFLICT,
                "operation_retiring",
            ),
            (
                ApiError::ProtocolMismatch,
                StatusCode::CONFLICT,
                "protocol_mismatch",
            ),
            (
                ApiError::CallShapeMismatch,
                StatusCode::CONFLICT,
                "call_shape_mismatch",
            ),
            (
                ApiError::OutcomeUnknown,
                StatusCode::INTERNAL_SERVER_ERROR,
                "outcome_unknown",
            ),
            (
                ApiError::Unauthorized,
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                ApiError::Capacity,
                StatusCode::TOO_MANY_REQUESTS,
                "capacity",
            ),
            (
                ApiError::ShuttingDown,
                StatusCode::CONFLICT,
                "generation_retired",
            ),
            (ApiError::Unavailable, StatusCode::NOT_FOUND, "unavailable"),
            (
                ApiError::Invalid("private input".into()),
                StatusCode::BAD_REQUEST,
                "invalid",
            ),
            (
                ApiError::Backend("private backend".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "backend",
            ),
        ] {
            let expected = format!(r#"{{"code":"{code}"}}"#);
            let response = crate::server::failure(error.clone());
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()["content-type"], "application/json");
            assert_eq!(
                response.headers()["content-length"],
                expected.len().to_string()
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body, expected.as_bytes());

            let response = crate::sse::response(
                Box::pin(futures_util::stream::iter([Err(error)])),
                CancellationToken::new(),
            );
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["content-type"], "text/event-stream");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                body,
                format!(": ready\n\nevent: error\ndata: {expected}\n\nevent: end\ndata: {{}}\n\n")
                    .as_bytes()
            );
        }
    }

    #[tokio::test]
    async fn domain_errors_preserve_payload_and_lease_in_both_response_forms() {
        let budget = ByteBudget::default();
        let payload = br#"{"domain":"fixture","detail":"owned by domain"}"#;
        let error = || ApiError::Domain(budget.copy(payload).unwrap());
        let response = crate::server::failure(error());
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.headers()["content-type"],
            "application/vnd.rsi.domain-error+json"
        );
        assert_eq!(
            response.headers()["content-length"],
            payload.len().to_string()
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes, payload.as_slice());
        assert_eq!(budget.used(), payload.len());
        drop(bytes);
        assert_eq!(budget.used(), 0);

        let mut body = crate::sse::response(
            Box::pin(futures_util::stream::iter([Err(error())])),
            CancellationToken::new(),
        )
        .into_body();
        for expected in [b": ready\n\n".as_slice(), b"event: domain-error\ndata: "] {
            assert_eq!(
                body.frame().await.unwrap().unwrap().into_data().unwrap(),
                expected
            );
        }
        let bytes = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(bytes, payload.as_slice());
        drop(body);
        assert_eq!(
            budget.used(),
            payload.len(),
            "transport bytes retain the domain lease"
        );
        drop(bytes);
        assert_eq!(budget.used(), 0);
    }
}
