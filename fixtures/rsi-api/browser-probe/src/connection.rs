use async_trait::async_trait;
use futures_util::{FutureExt, Stream};
use rsi_api_client::{ClientConnection, ConnectionTransport};
use rsi_api_protocol::*;
use rsi_meta::Execution;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Transport {
    entered: Semaphore,
    release: Semaphore,
    finished: Semaphore,
    completions: AtomicUsize,
    streams: Arc<AtomicUsize>,
}
struct LiveStream(Arc<AtomicUsize>);
impl Stream for LiveStream {
    type Item = Result<ApiMessage>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
impl Drop for LiveStream {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn observation() -> OperationSpec {
    OperationSpec {
        id: OperationId::new("worker", "observe", 1).unwrap(),
        class: OperationClass::Subscription,
        effect: OperationEffect::Read,
        ..super::spec(OperationEffect::Read)
    }
}
#[async_trait]
impl ConnectionTransport for Transport {
    async fn exchange(
        &self,
        operation: &OperationSpec,
        _: RetainedBytes,
        endpoint: &EndpointId,
        epoch: Option<&HostEpoch>,
        output: rsi_api_client::ClientResponseCapacity,
        retiring: &CancellationToken,
    ) -> Result<(HostEpoch, ApiOutput)> {
        let rsi_api_client::ClientResponseCapacity { receiving: output, retained } = output;
        let current = HostEpoch::from_bytes([9; 16]);
        assert!(epoch.is_none_or(|epoch| epoch == &current));
        if operation.id == observation().id {
            assert!(matches!(output, ApiResponseCapacity::Subscription { .. }));
            self.streams.fetch_add(1, Ordering::AcqRel);
            return Ok((
                current,
                ApiOutput::Stream(Box::pin(LiveStream(self.streams.clone()))),
            ));
        }
        let ApiResponseCapacity::Finite(capacity) = output else {
            panic!("finite admission")
        };
        let body = if operation.id == describe_operation().id {
            capacity.encode(&ConnectionDescription {
                wire_version: 1,
                endpoint_id: endpoint.clone(),
                host_epoch: current.clone(),
            })?
        } else if operation.id == operations_operation().id {
            capacity.encode(&OperationCatalog::new(vec![
                describe_operation(),
                operations_operation(),
                super::spec(OperationEffect::Mutation),
                observation(),
            ])?)?
        } else {
            self.entered.add_permits(1);
            tokio::select! { biased;
                () = retiring.cancelled() => return Err(ApiError::OutcomeUnknown),
                permit = self.release.acquire() => permit.unwrap().forget(),
            }
            self.completions.fetch_add(1, Ordering::AcqRel);
            self.finished.add_permits(1);
            capacity.encode(&true)?
        };
        Ok((
            current,
            ApiOutput::Reply(ApiMessage {
                json: retained.copy(body.as_bytes())?,
                binary: None,
            }),
        ))
    }
}

pub(super) async fn probe(execution: Execution) {
    let transport = Arc::new(Transport {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        finished: Semaphore::new(0),
        completions: AtomicUsize::new(0),
        streams: Arc::default(),
    });
    let client = ClientConnection::connect(
        execution,
        EndpointId::from_bytes([8; 16]),
        transport.clone(),
    )
    .await
    .unwrap();
    let operation = super::spec(OperationEffect::Mutation);
    for _ in 0..4 {
        let mut waiter = Box::pin(client.call(
            &operation,
            client.input_budget(operation.class).copy(b"{}").unwrap(),
        ));
        assert!(waiter.as_mut().now_or_never().is_none());
        transport.entered.acquire().await.unwrap().forget();
        drop(waiter);
    }
    assert!(matches!(
        client
            .call(
                &operation,
                client.input_budget(operation.class).copy(b"{}").unwrap()
            )
            .await,
        Err(ApiError::Capacity)
    ));
    transport.release.add_permits(4);
    transport.finished.acquire_many(4).await.unwrap().forget();
    let subscription = observation();
    let unpolled = client
        .call(
            &subscription,
            client.input_budget(subscription.class).copy(b"{}").unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(unpolled, ApiOutput::Stream(_)));
    assert_eq!(transport.streams.load(Ordering::Acquire), 1);
    client.close().await;
    assert_eq!(transport.streams.load(Ordering::Acquire), 0);
    assert_eq!(transport.completions.load(Ordering::Acquire), 4);
    assert!(matches!(
        client
            .call(
                &operation,
                client.input_budget(operation.class).copy(b"{}").unwrap()
            )
            .await,
        Err(ApiError::ShuttingDown)
    ));
    drop(unpolled);
}
