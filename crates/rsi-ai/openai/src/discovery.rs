//! Bounded `OpenAI` model-list wire translation, shared by compatible providers.
use rsi_ai_protocol::{
    AiError, DiscoveredModel, DispatchStatus, ErrorKind, ErrorPhase, MAX_DISCOVERY_BYTES,
    validate_discovered_models,
};
use rsi_ai_transport::{HttpRequest, HttpTransport, collect_body};
use rsi_ai_transport::{invalid_request_error, provider_error};
use rsi_credentials_protocol::SecretValue;
use serde_json::Value;

/// Performs one GET; dropping the future cancels its transport request.
pub async fn list(
    transport: &dyn HttpTransport,
    endpoint: &str,
    path: &str,
    secret: &SecretValue,
) -> Result<Vec<DiscoveredModel>, AiError> {
    let request = HttpRequest::new(http::Method::GET, crate::endpoint_url(endpoint, path))
        .and_then(|request| request.bearer_auth(secret))
        .map_err(|_| invalid_request_error("Invalid model discovery endpoint or credential"))?;
    let stop = tokio_util::sync::CancellationToken::new();
    let _cancel = stop.clone().drop_guard();
    let response = transport.execute(request, stop).await.map_err(|_| {
        provider_error(
            ErrorKind::Transport,
            ErrorPhase::Connect,
            DispatchStatus::Unknown,
            "Model discovery connection failed",
        )
    })?;
    if !(200..300).contains(&response.status) {
        let kind = match response.status {
            401 => ErrorKind::Authentication,
            403 => ErrorKind::Permission,
            408 | 504 => ErrorKind::Timeout,
            429 => ErrorKind::RateLimited,
            500..=599 => ErrorKind::Server,
            _ => ErrorKind::InvalidRequest,
        };
        return Err(provider_error(
            kind,
            ErrorPhase::Connect,
            DispatchStatus::Dispatched,
            format!("Model discovery returned HTTP {}", response.status),
        ));
    }
    let body = collect_body(response.body, MAX_DISCOVERY_BYTES)
        .await
        .map_err(|_| {
            provider_error(
                ErrorKind::Protocol,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "Model discovery body failed or exceeded 4 MiB",
            )
        })?;
    parse(&body).map_err(|message| {
        provider_error(
            ErrorKind::Protocol,
            ErrorPhase::Assemble,
            DispatchStatus::Dispatched,
            message,
        )
    })
}
/// Queries the official provider's configured API root.
pub async fn discover_models(
    transport: &dyn HttpTransport,
    endpoint: &str,
    secret: &SecretValue,
) -> Result<Vec<DiscoveredModel>, AiError> {
    list(transport, endpoint, "/v1/models", secret).await
}
fn parse(bytes: &[u8]) -> Result<Vec<DiscoveredModel>, &'static str> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid model discovery JSON")?;
    if !matches!(value.get("has_more"), None | Some(Value::Bool(false)))
        || ["next", "next_cursor"]
            .iter()
            .any(|key| match value.get(key) {
                None | Some(Value::Null) => false,
                Some(Value::String(cursor)) => !cursor.is_empty(),
                _ => true,
            })
    {
        return Err(
            "Model discovery requires a complete list; pagination is unsupported. Enter a model manually",
        );
    }
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Model discovery requires a data array")?;
    if rows.len() > rsi_ai_protocol::MAX_DISCOVERED_MODELS {
        return Err("model discovery exceeds 4096 candidates");
    }
    let mut models = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .ok_or("Model discovery requires model identifiers")?;
        let name = match row.get("name").or_else(|| row.get("display_name")) {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(name.clone()),
            _ => return Err("Invalid discovered model name"),
        };
        let capacity = |keys: &[&str]| -> Result<Option<u32>, &'static str> {
            for key in keys {
                if let Some(value) = row.pointer(key).filter(|v| !v.is_null()) {
                    return value
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0)
                        .map(Some)
                        .ok_or("Invalid discovered model capacity");
                }
            }
            Ok(None)
        };
        models.push(DiscoveredModel {
            id: id.into(),
            name,
            context_window_tokens: capacity(&[
                "/context_window_tokens",
                "/context_window",
                "/context_length",
                "/contextWindow",
                "/limit/context",
            ])?,
            max_output_tokens: capacity(&[
                "/max_output_tokens",
                "/maxOutputTokens",
                "/max_tokens",
                "/maxTokens",
                "/limit/output",
                "/top_provider/max_completion_tokens",
            ])?,
        });
    }
    validate_discovered_models(&models)?;
    Ok(models)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct ExpectUrl(String);
    #[async_trait::async_trait]
    impl HttpTransport for ExpectUrl {
        async fn execute(
            &self,
            request: HttpRequest,
            _: tokio_util::sync::CancellationToken,
        ) -> Result<rsi_ai_transport::HttpResponse, rsi_ai_transport::TransportError> {
            assert_eq!(request.url().as_str(), self.0);
            Ok(rsi_ai_transport::HttpResponse {
                status: 200,
                headers: http::HeaderMap::new(),
                body: Box::pin(futures_util::stream::once(async {
                    Ok(bytes::Bytes::from_static(br#"{"data":[]}"#))
                })),
            })
        }
    }
    #[tokio::test]
    async fn api_root_and_version_base_use_the_same_discovery_and_inference_paths() {
        for base in ["https://api.openai.com", "http://127.0.0.1/gateway"] {
            for suffix in ["", "/", "/v1", "/v1/"] {
                let endpoint = format!("{base}{suffix}");
                discover_models(
                    &ExpectUrl(format!("{base}/v1/models")),
                    &endpoint,
                    &SecretValue::new("test-only-key").unwrap(),
                )
                .await
                .unwrap();
                let config = crate::OpenAiConfig::new(endpoint).unwrap();
                for path in [
                    "/v1/responses",
                    "/v1/images/generations",
                    "/v1/responses/id",
                ] {
                    assert_eq!(config.url(path), format!("{base}{path}"));
                }
                assert_eq!(
                    config.url("/responses"),
                    format!("{}{}/responses", base, suffix.trim_end_matches('/'))
                );
            }
        }
    }
    #[test]
    fn incomplete_lists_fail_instead_of_silently_truncating() {
        for continuation in [
            serde_json::json!({"has_more":true}),
            serde_json::json!({"has_more":"true"}),
            serde_json::json!({"next":"cursor"}),
            serde_json::json!({"next_cursor":"cursor"}),
        ] {
            let mut value = continuation;
            value["data"] = serde_json::json!([{"id":"first-page-only"}]);
            assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        assert!(parse(br#"{"data":[],"has_more":false,"next":null,"next_cursor":""}"#).is_ok());
    }
    #[test]
    fn lists_preserve_missing_limits_and_reject_invalid_or_duplicate_metadata() {
        let rows = parse(br#"{"data":[{"id":"a","owned_by":"vendor"},{"id":"b","context_length":10000,"max_output_tokens":1000}]}"#).unwrap();
        assert_eq!(rows[0].context_window_tokens, None);
        assert_eq!(rows[1].max_output_tokens, Some(1000));
        for value in [
            serde_json::json!({"data":[{"id":"a"},{"id":"a"}]}),
            serde_json::json!({"data":[{"id":"a","context_window":-1}]}),
            serde_json::json!({"data":[{"id":"a","max_tokens":1.5}]}),
            serde_json::json!({"data":[{}]}),
            serde_json::json!({"models":[]}),
        ] {
            assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        assert!(
            parse(
                &serde_json::to_vec(
                    &serde_json::json!({"data":vec![serde_json::json!({"id":"a"});4097]})
                )
                .unwrap()
            )
            .is_err()
        );
        assert!(parse(b"not json").is_err());
    }
}
