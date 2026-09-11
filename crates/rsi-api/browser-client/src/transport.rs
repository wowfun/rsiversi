use crate::{
    BrowserClientConfig,
    bridge::{self, Bridge, RequestData},
};
use async_trait::async_trait;
use futures_util::StreamExt;
use rsi_api_client::{
    ConnectionTransport, decode_event_stream, decode_response, response_content_type,
    response_identity,
};
use rsi_api_protocol::{
    ApiError, ApiOutput, ApiResponseCapacity, ByteBudget, DeviceId, EndpointId, HostEpoch,
    OperationClass, OperationEffect, OperationSpec, RequestEncoding, Result, RetainedBytes,
};
use rsi_meta::Execution;
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Debug)]
pub(crate) struct BrowserTransport {
    pub bridge: Arc<Bridge>,
    pub execution: Execution,
    origin: String,
    require_h2: bool,
    expected_device: Mutex<Option<DeviceId>>,
}
impl BrowserTransport {
    pub fn new(execution: Execution, config: &BrowserClientConfig) -> Result<Self> {
        Ok(Self {
            origin: bridge::origin(config.allow_loopback_http)?,
            execution,
            bridge: Bridge::new(),
            require_h2: !config.allow_loopback_http,
            expected_device: Mutex::new(None),
        })
    }
    pub fn pin_device(&self, device: DeviceId) {
        *self
            .expected_device
            .lock()
            .expect("browser device pin poisoned") = Some(device);
    }
    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = base_headers();
        if let Some(device) = self
            .expected_device
            .lock()
            .expect("browser device pin poisoned")
            .as_ref()
        {
            headers.push(("x-rsi-expected-device", device.as_str().to_owned()));
        }
        headers
    }
    pub async fn close(&self) -> Result<()> {
        self.bridge.close(&self.execution).await
    }
    fn protocol(&self, headers: &http::HeaderMap) -> Result<()> {
        if self.require_h2
            && headers
                .get("x-rsi-http-version")
                .map(http::HeaderValue::as_bytes)
                != Some(b"2".as_slice())
        {
            return Err(ApiError::Invalid(
                "HTTPS browser API requires HTTP/2".into(),
            ));
        }
        Ok(())
    }
    pub async fn cookie(
        &self,
        endpoint: &EndpointId,
        token: Option<Zeroizing<String>>,
    ) -> Result<()> {
        let path = if token.is_some() { "login" } else { "logout" };
        let pending = self.bridge.start(RequestData {
            url: format!("{}/api/v1/{path}", self.origin),
            class: OperationClass::Control,
            headers: self.headers(),
            authorization: token,
            body: ByteBudget::new(1)?.copy(b"")?,
        })?;
        let started = pending.started.clone();
        self.execution
            .deadline_after(Duration::from_secs(15))
            .timeout(async {
                let head = pending
                    .head
                    .await
                    .unwrap_or_else(|_| Err(bridge::lost()))
                    .map_err(|error| uncertain(error, started.load(Ordering::Acquire)))?;
                self.protocol(&head.headers)
                    .map_err(|e| uncertain(e, true))?;
                response_identity(&head.headers, endpoint, None).map_err(|e| uncertain(e, true))?;
                if head.status != 200 {
                    return decode_response(
                        head.status,
                        &head.headers,
                        pending.source,
                        None,
                        true,
                        &rsi_api_protocol::ByteBudget::default(),
                    )
                    .await
                    .map(|_| ());
                }
                if head.headers.contains_key("content-encoding") {
                    return Err(ApiError::OutcomeUnknown);
                }
                let declared = rsi_api_client::response_content_length(&head.headers)
                    .map_err(|_| ApiError::OutcomeUnknown)?;
                if declared.is_some_and(|length| length != 2) {
                    return Err(ApiError::OutcomeUnknown);
                }
                let mut source = pending.source;
                let mut received = [0; 2];
                let mut used = 0;
                while let Some(chunk) = source.next().await {
                    let chunk = chunk.map_err(|_| ApiError::OutcomeUnknown)?;
                    if chunk.len() > 2 - used {
                        return Err(ApiError::OutcomeUnknown);
                    }
                    received[used..used + chunk.len()].copy_from_slice(&chunk);
                    used += chunk.len();
                }
                if used != 2 || received != *b"{}" {
                    return Err(ApiError::OutcomeUnknown);
                }
                Ok(())
            })
            .await
            .unwrap_or_else(|_| Err(uncertain(bridge::lost(), started.load(Ordering::Acquire))))
    }
}
impl Drop for BrowserTransport {
    fn drop(&mut self) {
        self.bridge.retire();
    }
}
fn base_headers() -> Vec<(&'static str, String)> {
    vec![
        ("x-rsi-wire-version", "1".into()),
        ("x-rsi-csrf", "1".into()),
    ]
}
fn uncertain(error: ApiError, mutation: bool) -> ApiError {
    if mutation {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}
#[async_trait]
impl ConnectionTransport for BrowserTransport {
    async fn exchange(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
        endpoint: &EndpointId,
        epoch: Option<&HostEpoch>,
        output: rsi_api_client::ClientResponseCapacity,
        retiring: &CancellationToken,
    ) -> Result<(HostEpoch, ApiOutput)> {
        let rsi_api_client::ClientResponseCapacity {
            receiving: output,
            retained,
        } = output;
        if retiring.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let mut headers = self.headers();
        headers.push((
            "content-type",
            match operation.encoding {
                RequestEncoding::Json => "application/json",
                RequestEncoding::Binary => "application/octet-stream",
            }
            .into(),
        ));
        if let Some(epoch) = epoch {
            headers.push(("x-rsi-host-epoch", epoch.as_str().into()));
        }
        let pending = self.bridge.start(RequestData {
            url: format!(
                "{}/api/v1/{}/{}/{}",
                self.origin,
                operation.id.domain(),
                operation.id.name(),
                operation.id.version()
            ),
            class: operation.class,
            headers,
            authorization: None,
            body: input,
        })?;
        let mutation = operation.effect == OperationEffect::Mutation;
        let started = pending.started.clone();
        let deadline = self.execution.deadline_after(Duration::from_mins(1));
        let head = tokio::select! { biased;
            () = retiring.cancelled() => return Err(uncertain(ApiError::ShuttingDown, mutation && started.load(Ordering::Acquire))),
            result = deadline.timeout(pending.head) => result.map_err(|_| uncertain(bridge::lost(), mutation && started.load(Ordering::Acquire)))?
                .map_err(|_| uncertain(bridge::lost(), mutation && started.load(Ordering::Acquire)))?
                .map_err(|error| uncertain(error, mutation && started.load(Ordering::Acquire)))?,
        };
        let received_epoch = response_identity(&head.headers, endpoint, epoch)
            .map_err(|e| uncertain(e, mutation))?;
        self.protocol(&head.headers)
            .map_err(|e| uncertain(e, mutation))?;
        let (capacity, subscription) = match output {
            ApiResponseCapacity::Finite(capacity) => (Some(capacity.reserve()?), None),
            ApiResponseCapacity::Subscription { budget, maximum } => {
                (None, Some((budget, maximum)))
            }
        };
        if head.status == 200
            && let Some((budget, maximum)) = subscription
        {
            response_content_type(&head.headers, "text/event-stream")?;
            return Ok((
                received_epoch,
                ApiOutput::Stream(decode_event_stream(
                    pending.source,
                    budget,
                    retained,
                    maximum,
                    self.execution.clone(),
                )),
            ));
        }
        let capacity = if capacity.is_none() && head.status == 422 {
            let (budget, maximum) = subscription.expect("subscription response admission");
            Some(budget.reserve(maximum)?)
        } else {
            capacity
        };
        let result = tokio::select! { biased;
            () = retiring.cancelled() => Err(uncertain(ApiError::ShuttingDown, mutation)),
            result = deadline.timeout(decode_response(head.status, &head.headers, pending.source, capacity, mutation, &retained)) =>
                result.unwrap_or_else(|_| Err(uncertain(bridge::lost(), mutation))),
        };
        result.map(|message| (received_epoch, ApiOutput::Reply(message)))
    }
}
