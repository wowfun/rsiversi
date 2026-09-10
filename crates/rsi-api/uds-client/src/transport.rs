use crate::UdsClientConfig;
use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::Full;
use rsi_api_client::{
    ConnectionTransport, decode_event_stream, decode_response, response_content_type,
    response_identity,
};
use rsi_api_protocol::{
    ApiError, ApiOutput, ApiResponseCapacity, EndpointId, HostEpoch, OperationEffect,
    OperationSpec, RequestEncoding, Result, RetainedBytes,
};
use rsi_meta::Execution;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct LocalTransport {
    pub config: UdsClientConfig,
    pub execution: Execution,
}
fn lost() -> ApiError {
    ApiError::Backend("local API response was lost".into())
}
fn protocol_failure(error: &hyper::Error) -> ApiError {
    ApiError::Backend(format!("local API response was lost: {error:?}"))
}
fn uncertain(error: ApiError, mutation: bool) -> ApiError {
    if mutation {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}

#[async_trait]
impl ConnectionTransport for LocalTransport {
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
        let deadline = self.execution.deadline_after(Duration::from_mins(1));
        let (mut sender, connection) = tokio::select! { biased;
            () = retiring.cancelled() => return Err(ApiError::ShuttingDown),
            result = deadline.timeout(crate::io::connect(&self.config.socket)) => result.map_err(|_| lost())??,
        };
        let request = self.request(operation, input, epoch)?;
        let mutation = operation.effect == OperationEffect::Mutation;
        let started = AtomicBool::new(false);
        let mut connection = Box::pin(connection);
        let mut ended = false;
        let pending = async {
            started.store(true, Ordering::Release);
            let response = sender.send_request(request);
            tokio::pin!(response);
            tokio::select! { biased;
                response = &mut response => response.map_err(|error| uncertain(protocol_failure(&error), mutation)),
                result = &mut connection => {
                    ended = true;
                    result.map_err(|error| uncertain(protocol_failure(&error), mutation))?;
                    response.await.map_err(|error| uncertain(protocol_failure(&error), mutation))
                },
            }
        };
        let response = tokio::select! { biased;
            () = retiring.cancelled() => return Err(uncertain(ApiError::ShuttingDown, mutation && started.load(Ordering::Acquire))),
            result = deadline.timeout(pending) => result.map_err(|_| uncertain(lost(), mutation && started.load(Ordering::Acquire)))??,
        };
        let (head, body) = response.into_parts();
        let received_epoch = response_identity(
            &head.headers,
            endpoint,
            epoch.or(Some(&self.config.host_epoch)),
        )
        .map_err(|error| uncertain(error, mutation))?;
        let source = crate::io::source(connection, body, ended);
        let (capacity, subscription) = match output {
            ApiResponseCapacity::Finite(capacity) => (Some(capacity.reserve()?), None),
            ApiResponseCapacity::Subscription { budget, maximum } => {
                (None, Some((budget, maximum)))
            }
        };
        if head.status.as_u16() == 200
            && let Some((budget, maximum)) = subscription
        {
            response_content_type(&head.headers, "text/event-stream")?;
            return Ok((
                received_epoch,
                ApiOutput::Stream(decode_event_stream(
                    source,
                    budget,
                    retained,
                    maximum,
                    self.execution.clone(),
                )),
            ));
        }
        let capacity = if capacity.is_none() && head.status.as_u16() == 422 {
            let (budget, maximum) = subscription.expect("subscription response admission");
            Some(budget.reserve(maximum)?)
        } else {
            capacity
        };
        let response = tokio::select! { biased;
            () = retiring.cancelled() => Err(uncertain(ApiError::ShuttingDown, mutation)),
            result = deadline.timeout(decode_response(head.status.as_u16(), &head.headers, source, capacity, mutation, &retained)) => result.unwrap_or_else(|_| Err(uncertain(lost(), mutation))),
        }?;
        Ok((received_epoch, ApiOutput::Reply(response)))
    }
}

impl LocalTransport {
    fn request(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
        epoch: Option<&HostEpoch>,
    ) -> Result<http::Request<Full<Bytes>>> {
        http::Request::builder()
            .method("POST")
            .uri(format!(
                "/api/v1/{}/{}/{}",
                operation.id.domain(),
                operation.id.name(),
                operation.id.version()
            ))
            .header("host", "rsi.local")
            .header("connection", "close")
            .header("x-rsi-wire-version", "1")
            .header("x-rsi-local-key", self.config.compatibility.as_str())
            .header(
                "x-rsi-host-epoch",
                epoch.unwrap_or(&self.config.host_epoch).as_str(),
            )
            .header(
                "content-type",
                match operation.encoding {
                    RequestEncoding::Json => "application/json",
                    RequestEncoding::Binary => "application/octet-stream",
                },
            )
            .body(Full::new(input.into_bytes()))
            .map_err(|_| ApiError::Invalid("invalid local API request".into()))
    }
}
