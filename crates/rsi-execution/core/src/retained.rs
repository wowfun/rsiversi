use crate::{ExecutionOperation, ExecutionPin};
use async_trait::async_trait;
use rsi_process::{ManagedDuplexProcess, ManagedProcess, ManagedPtyProcess, ProcessError, Result};
use std::future::Future;
use tokio::sync::oneshot;

#[async_trait]
pub(crate) trait Unpublished: Send + 'static {
    fn terminate(&self);
    async fn settle(self);
}

struct Publication<T: Unpublished> {
    process: Option<T>,
    permit: Option<ExecutionOperation>,
    runtime: tokio::runtime::Handle,
}
impl<T: Unpublished> Drop for Publication<T> {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            let permit = self.permit.take();
            process.terminate();
            self.runtime.spawn(async move {
                let _permit = permit;
                process.settle().await;
            });
        }
    }
}
#[async_trait]
impl Unpublished for ManagedProcess {
    fn terminate(&self) {
        self.terminate();
    }
    async fn settle(self) {
        let _ = self.wait().await;
    }
}
#[async_trait]
impl Unpublished for ManagedDuplexProcess {
    fn terminate(&self) {
        self.terminate();
    }
    async fn settle(self) {
        let _ = self.wait_settlement().await;
    }
}
#[async_trait]
impl Unpublished for ManagedPtyProcess {
    fn terminate(&self) {
        self.terminate();
    }
    async fn settle(self) {
        let _ = self.wait().await;
    }
}

pub(crate) async fn start<T: Unpublished>(
    permit: ExecutionOperation,
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    let (reply, result) = oneshot::channel();
    tokio::spawn(async move {
        // This task owns admission before polling the backend. A lost reply never
        // silently drops a live batch process or abandons its reaping obligation.
        // The guard also covers a successfully queued reply whose receiver is
        // dropped before polling it. Sender::send success is not an acknowledgement.
        let _ = reply.send(future.await.map(|process| Publication {
            process: Some(process),
            permit: Some(permit),
            runtime: tokio::runtime::Handle::current(),
        }));
    });
    result
        .await
        .unwrap_or(Err(ProcessError::OutcomeUnknown))
        .map(|mut publication| {
            publication
                .process
                .take()
                .expect("one publication consumes one process")
        })
}

pub(crate) async fn operation<T: Send + 'static>(
    pin: ExecutionPin,
    permit: ExecutionOperation,
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    let (reply, result) = oneshot::channel();
    tokio::spawn(async move {
        let (_pin, _permit) = (pin, permit);
        let _ = reply.send(future.await);
    });
    result.await.unwrap_or(Err(ProcessError::OutcomeUnknown))
}
