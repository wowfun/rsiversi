use crate::response;
use async_trait::async_trait;
use rsi_api_client::ConnectionTransport;
use rsi_api_protocol::{
    ApiError, ApiOutput, ApiResponseCapacity, EndpointId, HostEpoch, OperationClass,
    OperationEffect, OperationSpec, RequestEncoding, Result, RetainedBytes,
};
use rsi_meta::Execution;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct NativeTransport {
    pub transport: reqwest::Client,
    pub execution: Execution,
    pub origin: String,
    pub authorization: http::HeaderValue,
}
#[async_trait]
impl ConnectionTransport for NativeTransport {
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
        let (capacity, subscription_budget) = match output {
            ApiResponseCapacity::Finite(capacity) => (Some(capacity.reserve()?), None),
            ApiResponseCapacity::Subscription { budget, .. } => (None, Some(budget)),
        };
        let url = format!(
            "{}/api/v1/{}/{}/{}",
            self.origin,
            operation.id.domain(),
            operation.id.name(),
            operation.id.version()
        );
        let mut request = self
            .transport
            .post(url)
            .header("authorization", self.authorization.clone())
            .header("x-rsi-wire-version", "1")
            .header(
                "content-type",
                match operation.encoding {
                    RequestEncoding::Json => "application/json",
                    RequestEncoding::Binary => "application/octet-stream",
                },
            )
            .body(input.into_bytes());
        if let Some(epoch) = epoch {
            request = request.header("x-rsi-host-epoch", epoch.as_str());
        }
        let request = request.build().map_err(|_| response::invalid())?;
        let started = AtomicBool::new(false);
        let mutation = operation.effect == OperationEffect::Mutation;
        let deadline = self.execution.deadline_after(Duration::from_mins(1));
        let pending = async {
            started.store(true, Ordering::Release);
            self.transport.execute(request).await
        };
        let response = tokio::select! { biased;
            () = retiring.cancelled() => return Err(if mutation && started.load(Ordering::Acquire) { ApiError::OutcomeUnknown } else { ApiError::ShuttingDown }),
            response = deadline.timeout(pending) => match response {
                Err(_) => return Err(response::uncertain(ApiError::Backend("API response deadline elapsed".into()), mutation)),
                Ok(Err(error)) if error.is_connect() => return Err(ApiError::Backend("API connection failed".into())),
                Ok(Err(_)) => return Err(response::uncertain(ApiError::Backend("API response was lost".into()), mutation)),
                Ok(Ok(response)) => response,
            },
        };
        let received_epoch = response::identity(response.headers(), endpoint, epoch)
            .map_err(|error| response::uncertain(error, mutation))?;
        if response.status().as_u16() == 200 && operation.class == OperationClass::Subscription {
            response::content_type(response.headers(), "text/event-stream")?;
            let source = response::stream(
                response,
                subscription_budget.expect("subscription response admission"),
                retained,
                operation.maximum_response_bytes,
                self.execution.clone(),
            );
            return Ok((received_epoch, ApiOutput::Stream(source)));
        }
        let capacity = if capacity.is_none() && response.status().as_u16() == 422 {
            Some(
                subscription_budget
                    .expect("subscription response admission")
                    .reserve(operation.maximum_response_bytes)?,
            )
        } else {
            capacity
        };
        let result = tokio::select! { biased;
            () = retiring.cancelled() => Err(response::uncertain(ApiError::ShuttingDown, mutation)),
            result = deadline.timeout(response::finite(response, capacity, mutation, &retained)) => result.unwrap_or_else(|_| Err(response::uncertain(ApiError::Backend("API body deadline elapsed".into()), mutation))),
        };
        result.map(|message| (received_epoch, ApiOutput::Reply(message)))
    }
}
