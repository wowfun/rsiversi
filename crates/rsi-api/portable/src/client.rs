use crate::{error, wire};
use async_trait::async_trait;
use rsi_api_client::{ClientConnection, ClientResponseCapacity, ConnectionTransport};
use rsi_api_protocol::{
    ApiClient, ApiError, ApiOutput, ApiResponseCapacity, ByteBudget, ConnectionDescription,
    EndpointId, HostEpoch, OperationClass, OperationEffect, OperationSpec, Result, RetainedBytes,
    portable::{Description, Header, MAXIMUM_DESCRIPTION_BYTES},
};
use rsi_meta::{Capability, Execution};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// A Portable API connection using the shared client's admission and lifetime owner.
#[derive(Debug)]
pub struct PortableApiClient {
    connection: ClientConnection,
    transport: Arc<Transport>,
}
impl PortableApiClient {
    /// Negotiates the exact captured provider capability, without caller-origin claims.
    pub async fn connect(execution: Execution, capability: Capability) -> Result<Self> {
        let scratch = ByteBudget::default();
        let mut call = capability.open().map_err(|_| wire::lost())?;
        wire::send_header(&mut call, &scratch, &Header::Describe {}).await?;
        call.finish();
        let Header::Description { bytes } = wire::header(&mut call).await? else {
            return Err(wire::invalid());
        };
        if bytes > MAXIMUM_DESCRIPTION_BYTES {
            return Err(wire::invalid());
        }
        let body = wire::payload(&mut call, scratch.reserve(bytes)?, bytes).await?;
        wire::eof(&mut call).await?;
        let description: Description =
            serde_json::from_slice(body.as_bytes()).map_err(|_| wire::invalid())?;
        let transport = Arc::new(Transport {
            capability: Mutex::new(Some(capability)),
            description: description.connection.clone(),
            scratch,
        });
        let connection = ClientConnection::from_negotiated(
            execution,
            transport.clone(),
            description.connection,
            description.operations,
        )?;
        Ok(Self {
            connection,
            transport,
        })
    }
    /// Fences local calls and drops the retained provider capture.
    pub fn retire(&self) {
        self.connection.retire();
        self.transport
            .capability
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
    /// Releases unpolled streams and waits for local exchanges to relinquish captures.
    pub async fn close(&self) {
        self.retire();
        self.connection.close().await;
    }
}
impl Drop for PortableApiClient {
    fn drop(&mut self) {
        self.retire();
    }
}
#[async_trait]
impl ApiClient for PortableApiClient {
    fn description(&self) -> &ConnectionDescription {
        self.connection.description()
    }
    fn operations(&self) -> &[OperationSpec] {
        self.connection.operations()
    }
    fn input_budget(&self, class: OperationClass) -> ByteBudget {
        self.connection.input_budget(class)
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.connection.call(operation, input).await
    }
}
#[derive(Debug)]
struct Transport {
    capability: Mutex<Option<Capability>>,
    description: ConnectionDescription,
    scratch: ByteBudget,
}
#[async_trait]
impl ConnectionTransport for Transport {
    async fn exchange(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
        endpoint: &EndpointId,
        epoch: Option<&HostEpoch>,
        output: ClientResponseCapacity,
        retiring: &CancellationToken,
    ) -> Result<(HostEpoch, ApiOutput)> {
        if endpoint != &self.description.endpoint_id
            || epoch.is_some_and(|epoch| *epoch != self.description.host_epoch)
        {
            return Err(ApiError::Unavailable);
        }
        let capability = self
            .capability
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(ApiError::ShuttingDown)?;
        let result = tokio::select! {
            biased;
            () = retiring.cancelled() => Err(if operation.effect == OperationEffect::Mutation { ApiError::OutcomeUnknown } else { ApiError::ShuttingDown }),
            result = exchange(capability, operation, input, output, &self.scratch) => result,
        };
        result.map(|output| (self.description.host_epoch.clone(), output))
    }
}
async fn exchange(
    capability: Capability,
    operation: &OperationSpec,
    input: RetainedBytes,
    output: ClientResponseCapacity,
    scratch: &ByteBudget,
) -> Result<ApiOutput> {
    let mutation = operation.effect == OperationEffect::Mutation;
    let mut call = capability.open().map_err(|_| ApiError::Unavailable)?;
    let result = async {
        wire::send_header(
            &mut call,
            scratch,
            &Header::Call {
                operation: operation.id.clone(),
                bytes: input.len(),
            },
        )
        .await?;
        wire::send_payload(&mut call, scratch, input.as_bytes(), &[]).await?;
        call.finish();
        let header = wire::header(&mut call).await?;
        Ok(header)
    }
    .await
    .map_err(|error| uncertain(error, mutation))?;
    match (result, output.receiving) {
        (Header::Reply { json, binary }, ApiResponseCapacity::Finite(mut capacity)) => {
            let bytes = json
                .checked_add(binary.unwrap_or(0))
                .ok_or_else(|| uncertain(wire::invalid(), mutation))?;
            let message = wire::message(
                &mut call,
                capacity
                    .split(bytes)
                    .map_err(|error| uncertain(error, mutation))?,
                &output.retained,
                json,
                binary,
                operation.maximum_response_bytes,
            )
            .await
            .map_err(|error| uncertain(error, mutation))?;
            wire::eof(&mut call)
                .await
                .map_err(|error| uncertain(error, mutation))?;
            Ok(ApiOutput::Reply(message))
        }
        (Header::Error { code, domain }, capacity) => {
            let reservation = match capacity {
                ApiResponseCapacity::Finite(mut capacity) => capacity
                    .split(domain.unwrap_or(0))
                    .map_err(|error| uncertain(error, mutation))?,
                ApiResponseCapacity::Subscription { budget, maximum } => {
                    budget.reserve(domain.unwrap_or(0).min(maximum))?
                }
            };
            let error = error::receive(
                &mut call,
                code,
                domain,
                reservation,
                &output.retained,
                operation.maximum_response_bytes,
            )
            .await
            .map_err(|error| uncertain(error, mutation))?;
            wire::eof(&mut call)
                .await
                .map_err(|error| uncertain(error, mutation))?;
            Err(error)
        }
        (Header::Stream {}, ApiResponseCapacity::Subscription { budget, maximum }) => {
            let retained = output.retained;
            Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
                loop {
                    // Admission precedes polling the next item/header, even for an
                    // unbounded source; the shared client supervises this source.
                    let reservation = budget.reserve(maximum)?;
                    match wire::header(&mut call).await? {
                        Header::Item { json, binary } => yield wire::message(&mut call, reservation, &retained, json, binary, maximum).await?,
                        Header::End {} => { wire::eof(&mut call).await?; break; },
                        Header::Error { code, domain } => { let error = error::receive(&mut call, code, domain, reservation, &retained, maximum).await?; wire::eof(&mut call).await?; Err(error)?; },
                        _ => Err(wire::invalid())?,
                    }
                }
            })))
        }
        _ => Err(uncertain(wire::invalid(), mutation)),
    }
}
fn uncertain(error: ApiError, mutation: bool) -> ApiError {
    if mutation {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}
