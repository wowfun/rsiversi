//! Bounded local launch exchange; device credentials remain in `HttpOnly` cookies.
use crate::bridge::{self, Bridge, RequestData};
use futures_util::StreamExt;
use rsi_api_protocol::{ApiError, ByteBudget, EndpointId, OperationClass, Result};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use std::{sync::Arc, time::Duration};
use zeroize::Zeroizing;

/// Exchanges one ticket at the loopback Worker origin, or recovers an existing cookie.
/// An uncertain exchange is followed only by a cookie read, never ticket replay.
pub async fn bootstrap_local_browser(
    execution: Execution,
    ticket: Option<SecretValue>,
) -> Result<EndpointId> {
    let origin = bridge::origin(true)?;
    if ticket.as_ref().is_some_and(|ticket| {
        let value = ticket.expose_secret();
        value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit())
    }) {
        return Err(ApiError::Unauthorized);
    }
    let bridge = Bridge::new();
    let exchanged = execution
        .deadline_after(Duration::from_secs(15))
        .timeout(exchange(&bridge, &origin, ticket.as_ref()))
        .await
        .unwrap_or(Err(ApiError::OutcomeUnknown));
    let result = crate::bootstrap_recovery::recover(ticket.is_some(), exchanged, || async {
        execution
            .deadline_after(Duration::from_secs(15))
            .timeout(exchange(&bridge, &origin, None))
            .await
            .unwrap_or(Err(ApiError::OutcomeUnknown))
    })
    .await;
    crate::bootstrap_recovery::after_cleanup(result, bridge.close(&execution).await)
}
async fn exchange(
    bridge: &Arc<Bridge>,
    origin: &str,
    ticket: Option<&SecretValue>,
) -> Result<EndpointId> {
    let pending = bridge.start(RequestData {
        url: format!("{origin}/api/v1/browser-bootstrap"),
        class: OperationClass::Control,
        headers: vec![("x-rsi-csrf", "1".into())],
        authorization: None,
        launch_ticket: ticket.map(|ticket| Zeroizing::new(ticket.expose_secret().to_owned())),
        body: ByteBudget::new(1)?.copy(b"")?,
    })?;
    let uncertain = |error| crate::bootstrap_response::uncertain(error, ticket.is_some());
    let head = pending
        .head
        .await
        .map_err(|_| ApiError::OutcomeUnknown)?
        .map_err(uncertain)?;
    let maximum = crate::bootstrap_response::limit(head.status);
    if rsi_api_client::response_content_length(&head.headers)
        .map_err(uncertain)?
        .is_some_and(|n| n > maximum)
    {
        return Err(uncertain(ApiError::Capacity));
    }
    let mut source = pending.source;
    let mut bytes = Vec::new();
    while let Some(chunk) = source.next().await {
        let chunk = chunk.map_err(uncertain)?;
        if chunk.len() > maximum - bytes.len() {
            return Err(uncertain(ApiError::Capacity));
        }
        bytes.extend_from_slice(&chunk);
    }
    crate::bootstrap_response::decode(head.status, &head.headers, &bytes, ticket.is_some())
}
