use crate::{error, wire};
use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, OperationCatalog, OperationClass,
    OperationEffect, OperationKey, OperationSpec, Result, RetainedBytes, describe_operation,
    operation_operation, operations_operation,
    portable::{Description, Header, MAXIMUM_DESCRIPTION_BYTES},
};
use rsi_meta::{Execution, InvocationContext, MetaError, ProviderChannel, ServiceEndpoint};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug)]
pub(crate) struct Export {
    api: Mutex<Option<Arc<dyn ApiClient>>>,
    description: Description,
    selected: Arc<[OperationKey]>,
    execution: Execution,
    scratch: ByteBudget,
    stop: CancellationToken,
    tasks: TaskTracker,
    slots: [Arc<Semaphore>; 3],
}
impl Export {
    pub fn new(
        api: Arc<dyn ApiClient>,
        selected: &[OperationKey],
        execution: Execution,
    ) -> Result<Self> {
        let description = Description {
            connection: api.description().clone(),
        };
        if description.connection.wire_version != rsi_api_protocol::API_WIRE_VERSION {
            return Err(wire::invalid());
        }
        Ok(Self {
            api: Mutex::new(Some(api)),
            description,
            selected: selected.into(),
            execution,
            scratch: ByteBudget::default(),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            slots: [
                Arc::new(Semaphore::new(16)),
                Arc::new(Semaphore::new(16)),
                Arc::new(Semaphore::new(64)),
            ],
        })
    }
    pub fn retire(&self) {
        self.stop.cancel();
    }
    pub async fn close(&self) {
        self.retire();
        self.tasks.close();
        self.tasks.wait().await;
        self.api
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
    async fn request(&self, channel: &mut ProviderChannel<'_>) -> Result<()> {
        let cancellation = channel.cancellation();
        match wire::header(channel).await? {
            Header::Describe {} => {
                let _permit = self.slots[0]
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ApiError::Capacity)?;
                wire::eof(channel).await?;
                if cancellation.is_cancelled() {
                    return Err(ApiError::ShuttingDown);
                }
                let bytes = self
                    .scratch
                    .encode(&self.description, MAXIMUM_DESCRIPTION_BYTES)?;
                wire::send_header(
                    channel,
                    &self.scratch,
                    &Header::Description { bytes: bytes.len() },
                )
                .await?;
                wire::send_payload(channel, &self.scratch, bytes.as_bytes(), &[]).await
            }
            Header::Call { operation, bytes } => {
                let lookup = self.slots[0]
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ApiError::Capacity)?;
                let api = self
                    .api
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .ok_or(ApiError::ShuttingDown)?;
                let spec = if operation.key == describe_operation().key() {
                    describe_operation()
                } else if operation.key == operations_operation().key() {
                    operations_operation()
                } else if operation.key == operation_operation().key() {
                    operation_operation()
                } else {
                    if !self.selected.contains(&operation.key) {
                        return Err(ApiError::Unauthorized);
                    }
                    api.operation(&operation.key).await?
                };
                if operation.shape != spec.shape() {
                    return Err(ApiError::CallShapeMismatch);
                }
                let operation = spec;
                if bytes > operation.maximum_request_bytes {
                    return Err(wire::invalid());
                }
                let lane = match operation.class {
                    OperationClass::Control => 0,
                    OperationClass::Data => 1,
                    OperationClass::Subscription => 2,
                };
                drop(lookup);
                let permit = self.slots[lane]
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| ApiError::Capacity)?;
                let input = wire::payload(
                    channel,
                    api.input_budget(operation.class).reserve(bytes)?,
                    bytes,
                )
                .await?;
                wire::eof(channel).await?;
                if cancellation.is_cancelled() {
                    return Err(ApiError::ShuttingDown);
                }
                let (permit, result) = self.call(api, operation.clone(), input, permit).await;
                let _permit = permit;
                match result {
                    Ok(output) => self
                        .output(channel, &operation, output)
                        .await
                        .map_err(|error| {
                            if operation.effect == OperationEffect::Mutation {
                                ApiError::OutcomeUnknown
                            } else {
                                error
                            }
                        }),
                    Err(error) => {
                        error::send(
                            channel,
                            &self.scratch,
                            error,
                            operation.maximum_response_bytes,
                        )
                        .await
                    }
                }
            }
            _ => Err(wire::invalid()),
        }
    }
    async fn call(
        &self,
        api: Arc<dyn ApiClient>,
        operation: OperationSpec,
        input: RetainedBytes,
        permit: OwnedSemaphorePermit,
    ) -> (Option<OwnedSemaphorePermit>, Result<ApiOutput>) {
        if operation == describe_operation()
            || operation == operations_operation()
            || operation == operation_operation()
        {
            return (
                Some(permit),
                self.negotiation(api.as_ref(), &operation, &input).await,
            );
        }
        if operation.effect == OperationEffect::Read {
            return (Some(permit), api.call(&operation, input).await);
        }
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let result = api.call(&operation, input).await;
            (Some(permit), result)
        }));
        task.await.unwrap_or((None, Err(ApiError::OutcomeUnknown)))
    }
    async fn negotiation(
        &self,
        api: &dyn ApiClient,
        operation: &OperationSpec,
        input: &RetainedBytes,
    ) -> Result<ApiOutput> {
        let json = if operation == &describe_operation() {
            let hello: rsi_api_protocol::ConnectionHello =
                serde_json::from_slice(input.as_bytes()).map_err(|_| wire::invalid())?;
            if hello.wire_version != rsi_api_protocol::API_WIRE_VERSION
                || hello
                    .expected_endpoint
                    .as_ref()
                    .is_some_and(|endpoint| *endpoint != self.description.connection.endpoint_id)
            {
                return Err(ApiError::Unavailable);
            }
            self.scratch.encode(
                &self.description.connection,
                operation.maximum_response_bytes,
            )?
        } else {
            let query: rsi_api_protocol::OperationsQuery =
                serde_json::from_slice(input.as_bytes()).map_err(|_| wire::invalid())?;
            if operation == &operation_operation() && query.key.is_none() {
                return Err(wire::invalid());
            }
            let mut operations = vec![
                describe_operation(),
                operation_operation(),
                operations_operation(),
            ];
            if let Some(key) = query.key {
                key.validate()?;
                operations.retain(|spec| spec.key() == key);
                if operations.is_empty() {
                    if !self.selected.contains(&key) {
                        return Err(ApiError::Unauthorized);
                    }
                    operations.push(api.operation(&key).await?);
                }
            } else {
                for spec in api.operations().await?.operations() {
                    if self.selected.contains(&spec.key())
                        && !operations.iter().any(|own| own.key() == spec.key())
                    {
                        operations.push(spec.clone());
                    }
                }
            }
            self.scratch.encode(
                &OperationCatalog::new(operations)?,
                operation.maximum_response_bytes,
            )?
        };
        Ok(ApiOutput::Reply(ApiMessage { json, binary: None }))
    }
    async fn output(
        &self,
        channel: &mut ProviderChannel<'_>,
        operation: &OperationSpec,
        output: ApiOutput,
    ) -> Result<()> {
        match output {
            ApiOutput::Reply(message) if operation.class != OperationClass::Subscription => {
                self.send_message(channel, &message, false, operation.maximum_response_bytes)
                    .await
            }
            ApiOutput::Stream(mut source) if operation.class == OperationClass::Subscription => {
                wire::send_header(channel, &self.scratch, &Header::Stream {}).await?;
                while let Some(message) = source.next().await {
                    match message {
                        Ok(message) => {
                            self.send_message(
                                channel,
                                &message,
                                true,
                                operation.maximum_response_bytes,
                            )
                            .await?;
                        }
                        Err(error) => {
                            return error::send(
                                channel,
                                &self.scratch,
                                error,
                                operation.maximum_response_bytes,
                            )
                            .await;
                        }
                    }
                }
                wire::send_header(channel, &self.scratch, &Header::End {}).await
            }
            _ => Err(if operation.effect == OperationEffect::Mutation {
                ApiError::OutcomeUnknown
            } else {
                ApiError::Backend("API response class mismatch".into())
            }),
        }
    }
    async fn send_message(
        &self,
        channel: &mut ProviderChannel<'_>,
        message: &ApiMessage,
        stream: bool,
        maximum: usize,
    ) -> Result<()> {
        if message.encoded_len() > maximum {
            return Err(wire::invalid());
        }
        let json = message.json.len();
        let binary = message.binary.as_ref().map(RetainedBytes::len);
        let header = if stream {
            Header::Item { json, binary }
        } else {
            Header::Reply { json, binary }
        };
        wire::send_header(channel, &self.scratch, &header).await?;
        wire::send_payload(
            channel,
            &self.scratch,
            message.json.as_bytes(),
            message.binary.as_ref().map_or(&[], RetainedBytes::as_bytes),
        )
        .await
    }
}
#[async_trait]
impl ServiceEndpoint for Export {
    async fn serve(
        &self,
        _: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let cancellation = channel.cancellation();
        let result = tokio::select! {
            biased;
            () = self.stop.cancelled() => return Err(MetaError::Cancelled),
            () = cancellation.cancelled() => return Err(MetaError::Cancelled),
            result = self.request(&mut channel) => result,
        };
        if let Err(error) = result {
            error::send(&mut channel, &self.scratch, error, 0)
                .await
                .map_err(|_| MetaError::Cancelled)?;
        }
        Ok(())
    }
}
