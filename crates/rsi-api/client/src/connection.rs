use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiOutput, ApiResponseCapacity, ByteBudget, ConnectionDescription,
    ConnectionHello, EndpointId, FiniteResponseCapacity, HostEpoch, OperationCatalog,
    OperationClass, OperationEffect, OperationSpec, Result, RetainedBytes, describe_operation,
    operation_operation, operations_operation, supervised_stream,
};
use rsi_meta_execution::Execution;
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

/// Platform I/O for one admitted exchange; no connection admission or domain state.
#[async_trait]
pub trait ConnectionTransport: fmt::Debug + Send + Sync + 'static {
    /// Checks exact deployment/generation identity and bounds finite I/O to one minute.
    ///
    /// Read/stream drop releases local I/O. Retirement cancels the local exchange;
    /// any possibly delivered mutation then remains `OutcomeUnknown`. Returned streams
    /// are queue-free and require explicit terminal/EOF validation by the transport.
    async fn exchange(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
        endpoint: &EndpointId,
        epoch: Option<&HostEpoch>,
        output: ClientResponseCapacity,
        retiring: &CancellationToken,
    ) -> Result<(HostEpoch, ApiOutput)>;
}

/// Receive admission and the independent owner of a completed client response.
#[derive(Debug)]
pub struct ClientResponseCapacity {
    /// Pre-admitted finite storage or the bounded subscription receive pool.
    pub receiving: ApiResponseCapacity,
    /// Destination for completed finite responses and retained stream items.
    pub retained: ByteBudget,
}

/// Shared owner of one negotiated connection generation and all its admitted work.
#[derive(Debug)]
pub struct ClientConnection {
    inner: Arc<Connection>,
    description: ConnectionDescription,
}
#[derive(Debug)]
struct Connection {
    execution: Execution,
    transport: Arc<dyn ConnectionTransport>,
    endpoint: EndpointId,
    retiring: CancellationToken,
    calls: [Arc<Semaphore>; 3],
    input: [ByteBudget; 3],
    output: [ByteBudget; 3],
    retained: [ByteBudget; 3],
    work: Arc<Work>,
}
#[derive(Debug, Default)]
struct Work {
    state: Mutex<WorkState>,
    drained: Notify,
}
#[derive(Debug, Default)]
struct WorkState {
    closed: bool,
    active: usize,
}
#[derive(Debug)]
struct WorkLease(Arc<Work>);
impl Drop for WorkLease {
    fn drop(&mut self) {
        let last = {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.active -= 1;
            state.active == 0
        };
        if last {
            self.0.drained.notify_waiters();
        }
    }
}
struct Admission {
    permit: OwnedSemaphorePermit,
    capacity: Option<FiniteResponseCapacity>,
    work: WorkLease,
}
fn budgets() -> [ByteBudget; 3] {
    [
        ByteBudget::new(2 * 1024 * 1024).expect("constant budget"),
        ByteBudget::default(),
        ByteBudget::default(),
    ]
}
fn lane(class: OperationClass) -> usize {
    match class {
        OperationClass::Control => 0,
        OperationClass::Data => 1,
        OperationClass::Subscription => 2,
    }
}
impl ClientConnection {
    async fn query_operations(
        &self,
        key: Option<rsi_api_protocol::OperationKey>,
    ) -> Result<OperationCatalog> {
        let spec = if key.is_some() {
            operation_operation()
        } else {
            operations_operation()
        };
        let input = self.input_budget(spec.class).encode(
            &rsi_api_protocol::OperationsQuery { key },
            spec.maximum_request_bytes,
        )?;
        let ApiOutput::Reply(reply) = self.call(&spec, input).await? else {
            return Err(crate::invalid());
        };
        if reply.binary.is_some() {
            return Err(crate::invalid());
        }
        serde_json::from_slice(reply.json.as_bytes()).map_err(|_| crate::invalid())
    }

    /// Uses identity and descriptors already negotiated by the supplied transport.
    /// The transport owns verification against its exact provider generation.
    pub fn from_negotiated(
        execution: Execution,
        transport: Arc<dyn ConnectionTransport>,
        description: ConnectionDescription,
    ) -> Result<Self> {
        if description.wire_version != rsi_api_protocol::API_WIRE_VERSION {
            return Err(ApiError::Invalid(
                "invalid negotiated API connection".into(),
            ));
        }
        Ok(Self {
            inner: Arc::new(Connection::new(
                execution,
                description.endpoint_id.clone(),
                transport,
            )),
            description,
        })
    }
    /// Negotiates one deployment over an explicitly owned transport.
    pub async fn connect(
        execution: Execution,
        endpoint: EndpointId,
        transport: Arc<dyn ConnectionTransport>,
    ) -> Result<Self> {
        let deadline = execution.deadline_after(Duration::from_secs(15));
        deadline
            .timeout(async move {
                let inner = Arc::new(Connection::new(execution, endpoint, transport));
                let operation = describe_operation();
                let input = inner.input[0].encode(
                    &ConnectionHello {
                        wire_version: rsi_api_protocol::API_WIRE_VERSION,
                        expected_endpoint: Some(inner.endpoint.clone()),
                    },
                    operation.maximum_request_bytes,
                )?;
                let admission = inner.admit(&operation, &input)?;
                let (epoch, reply) = inner.exchange(&operation, input, None, admission).await?;
                let ApiOutput::Reply(reply) = reply else {
                    return Err(crate::invalid());
                };
                if reply.binary.is_some() {
                    return Err(crate::invalid());
                }
                let description: ConnectionDescription =
                    serde_json::from_slice(reply.json.as_bytes()).map_err(|_| crate::invalid())?;
                if description.wire_version != rsi_api_protocol::API_WIRE_VERSION
                    || description.endpoint_id != inner.endpoint
                    || description.host_epoch != epoch
                {
                    return Err(crate::invalid());
                }
                Ok(Self { inner, description })
            })
            .await
            .map_err(|_| ApiError::Backend("API negotiation deadline elapsed".into()))?
    }

    /// Fences this client and releases local observations, without stopping its remote Host.
    pub fn retire(&self) {
        self.inner
            .work
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.inner.retiring.cancel();
    }
    /// Fences admission and awaits local connection cleanup, including unpolled streams.
    pub async fn close(&self) {
        self.retire();
        loop {
            let changed = self.inner.work.drained.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .inner
                .work
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                == 0
            {
                return;
            }
            changed.await;
        }
    }
}
impl Drop for ClientConnection {
    fn drop(&mut self) {
        self.retire();
    }
}
#[async_trait]
impl ApiClient for ClientConnection {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> Result<OperationCatalog> {
        self.query_operations(None).await
    }
    async fn operation(&self, key: &rsi_api_protocol::OperationKey) -> Result<OperationSpec> {
        self.query_operations(Some(key.clone()))
            .await?
            .operations()
            .first()
            .filter(|spec| spec.key() == *key)
            .cloned()
            .ok_or(ApiError::UnknownOperation)
    }
    fn input_budget(&self, class: OperationClass) -> ByteBudget {
        self.inner.input[lane(class)].clone()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        let admission = self.inner.admit(operation, &input)?;
        let operation = operation.clone();
        let epoch = self.description.host_epoch.clone();
        let inner = self.inner.clone();
        let mutation = operation.effect == OperationEffect::Mutation;
        let (mut sender, receiver) = oneshot::channel();
        drop(self.inner.execution.spawn(async move {
            let exchange = inner.exchange(&operation, input, Some(&epoch), admission);
            let result = if mutation { exchange.await } else {
                tokio::select! { biased; () = sender.closed() => return, result = exchange => result }
            };
            let _ = sender.send(result.map(|(_, output)| output));
        }));
        receiver.await.unwrap_or_else(|_| {
            Err(uncertain(
                ApiError::Backend("API client task stopped without a result".into()),
                mutation,
            ))
        })
    }
}
impl Connection {
    fn new(
        execution: Execution,
        endpoint: EndpointId,
        transport: Arc<dyn ConnectionTransport>,
    ) -> Self {
        Self {
            transport,
            execution,
            endpoint,
            retiring: CancellationToken::new(),
            calls: [
                Arc::new(Semaphore::new(4)),
                Arc::new(Semaphore::new(4)),
                Arc::new(Semaphore::new(16)),
            ],
            input: budgets(),
            output: budgets(),
            retained: budgets(),
            work: Arc::default(),
        }
    }

    fn admit(&self, operation: &OperationSpec, input: &RetainedBytes) -> Result<Admission> {
        let mut state = self
            .work
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if input.len() > operation.maximum_request_bytes {
            return Err(ApiError::Invalid(
                "API input exceeds operation bound".into(),
            ));
        }
        let lane = lane(operation.class);
        let permit = self.calls[lane]
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let capacity = if operation.class == OperationClass::Subscription {
            None
        } else {
            Some(if operation.effect == OperationEffect::Mutation {
                self.output[lane]
                    .reserve(operation.maximum_response_bytes)?
                    .into()
            } else {
                FiniteResponseCapacity::Measured {
                    budget: self.output[lane].clone(),
                    maximum: operation.maximum_response_bytes,
                }
            })
        };
        state.active += 1;
        Ok(Admission {
            permit,
            capacity,
            work: WorkLease(self.work.clone()),
        })
    }
    async fn exchange(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
        epoch: Option<&HostEpoch>,
        admission: Admission,
    ) -> Result<(HostEpoch, ApiOutput)> {
        let Admission {
            permit,
            capacity,
            work,
        } = admission;
        let output = match capacity {
            Some(capacity) => ApiResponseCapacity::Finite(capacity),
            None => ApiResponseCapacity::Subscription {
                budget: self.output[lane(operation.class)].clone(),
                maximum: operation.maximum_response_bytes,
            },
        };
        let (epoch, output) = self
            .transport
            .exchange(
                operation,
                input,
                &self.endpoint,
                epoch,
                ClientResponseCapacity {
                    receiving: output,
                    retained: self.retained[lane(operation.class)].clone(),
                },
                &self.retiring,
            )
            .await?;
        match output {
            ApiOutput::Stream(source) if operation.class == OperationClass::Subscription => {
                let (stream, driver) = supervised_stream(
                    source,
                    operation.maximum_response_bytes,
                    self.retiring.clone(),
                    CancellationToken::new(),
                );
                drop(self.execution.spawn(async move {
                    let _permit = permit;
                    let _work = work;
                    driver.await;
                }));
                Ok((epoch, ApiOutput::Stream(stream)))
            }
            ApiOutput::Reply(reply) if operation.class != OperationClass::Subscription => {
                Ok((epoch, ApiOutput::Reply(reply)))
            }
            _ => Err(uncertain(
                ApiError::Backend("transport response disagrees with operation class".into()),
                operation.effect == OperationEffect::Mutation,
            )),
        }
    }
}
fn uncertain(error: ApiError, mutation: bool) -> ApiError {
    if mutation {
        ApiError::OutcomeUnknown
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct UnusedTransport;
    #[async_trait]
    impl ConnectionTransport for UnusedTransport {
        async fn exchange(
            &self,
            _: &OperationSpec,
            _: RetainedBytes,
            _: &EndpointId,
            _: Option<&HostEpoch>,
            _: ClientResponseCapacity,
            _: &CancellationToken,
        ) -> Result<(HostEpoch, ApiOutput)> {
            unreachable!("admission test")
        }
    }
    #[tokio::test]
    async fn data_saturation_does_not_block_policy_discovery_or_subscriptions() {
        let connection = Connection::new(
            Execution::native(tokio::runtime::Handle::current()),
            EndpointId::from_bytes([1; 16]),
            Arc::new(UnusedTransport),
        );
        let discovery = operation_operation();
        discovery.validate().unwrap();
        let mut data = discovery.clone();
        data.class = OperationClass::Data;
        let input = ByteBudget::default()
            .encode(&serde_json::json!({}), data.maximum_request_bytes)
            .unwrap();
        let held: Vec<_> = (0..4)
            .map(|_| connection.admit(&data, &input).unwrap())
            .collect();
        assert!(matches!(
            connection.admit(&data, &input),
            Err(ApiError::Capacity)
        ));
        let policy = connection.admit(&discovery, &input).unwrap();
        let control = connection.admit(&describe_operation(), &input).unwrap();
        let mut subscription = data;
        subscription.class = OperationClass::Subscription;
        let subscription = connection.admit(&subscription, &input).unwrap();
        drop((held, policy, control, subscription));
        assert_eq!(connection.work.state.lock().unwrap().active, 0);
    }
}
