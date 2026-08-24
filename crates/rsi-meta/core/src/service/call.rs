use super::channel::{FrameBudget, send_frame};
use super::{
    BufferedByteAdmission, BufferedFrame, LeaseGuard, ProviderBinding, ResponseMessage,
    ServiceFrame,
};
use crate::runtime::{ResourceLedger, ResourceReservation};
use crate::{
    Context, ContractId, ContractVersion, FiberGeneration, FiberId, MetaError, Result, Runtime,
    ServiceKey,
};
use std::fmt;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) struct CallLease {
    _resource: ResourceReservation,
    _admission: OwnedSemaphorePermit,
    _runtime: LeaseGuard,
}

impl CallLease {
    pub(crate) fn new(
        runtime: LeaseGuard,
        admission: OwnedSemaphorePermit,
        resource: ResourceReservation,
    ) -> Self {
        Self {
            _resource: resource,
            _admission: admission,
            _runtime: runtime,
        }
    }
}

impl fmt::Debug for CallLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("CallLease").finish_non_exhaustive()
    }
}

/// Caller half of one admitted bounded bidirectional service call.
#[derive(Debug)]
pub struct ServiceCall {
    pub(crate) requests: Option<mpsc::Sender<BufferedFrame>>,
    pub(crate) responses: Option<mpsc::Receiver<ResponseMessage>>,
    pub(crate) byte_admission: Arc<BufferedByteAdmission>,
    pub(crate) byte_resources: Arc<ResourceLedger>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) deadline: Instant,
    pub(crate) maximum_frame_bytes: usize,
    pub(crate) lease: Option<Arc<CallLease>>,
    pub(crate) terminal_observed: bool,
}

impl ServiceCall {
    /// Sends one bounded request frame.
    pub async fn send(&self, frame: ServiceFrame) -> Result<()> {
        let Some(requests) = self.requests.as_ref() else {
            if frame.as_bytes().len() > self.maximum_frame_bytes {
                return Err(MetaError::PayloadTooLarge {
                    maximum: self.maximum_frame_bytes,
                });
            }
            return Err(MetaError::Cancelled);
        };
        send_frame(
            requests,
            frame,
            std::convert::identity,
            FrameBudget {
                maximum_frame_bytes: self.maximum_frame_bytes,
                byte_admission: &self.byte_admission,
                byte_resources: &self.byte_resources,
                cancellation: &self.cancellation,
                deadline: self.deadline,
            },
        )
        .await
    }

    /// Closes the request stream while retaining the response stream.
    pub fn finish(&mut self) {
        self.requests.take();
    }

    /// Receives the next response; a clean terminal is returned as `Ok(None)`.
    /// Cooperative cancellation joins the driver's unique terminal instead of
    /// synthesizing a competing result in the caller half.
    pub async fn recv(&mut self) -> Result<Option<ServiceFrame>> {
        if self.terminal_observed {
            return Ok(None);
        }
        // A ready response is authoritative over the driver's internal
        // cancellation wake-up; the absolute deadline is authoritative when
        // the driver has not yet published its terminal.
        let responses = self
            .responses
            .as_mut()
            .expect("a live service call retains its response inbox");
        let message = tokio::select! {
            biased;
            message = responses.recv() => message,
            () = tokio::time::sleep_until(self.deadline) => {
                self.cancellation.cancel();
                self.observe_terminal();
                return Err(MetaError::Timeout("service call"));
            }
            () = self.cancellation.cancelled() => {
                // Cancellation requests the Runtime-owned driver to publish
                // its unique terminal; it is not a competing terminal. Join
                // that publication so an endpoint, Runtime-terminal, or
                // absolute-deadline result that raced cancellation remains
                // authoritative.
                tokio::select! {
                    biased;
                    message = responses.recv() => message,
                    () = tokio::time::sleep_until(self.deadline) => {
                        self.observe_terminal();
                        return Err(MetaError::Timeout("service call"));
                    }
                }
            }
        };
        match message {
            Some(ResponseMessage::Frame(frame)) => Ok(Some(frame.into_frame())),
            Some(ResponseMessage::Terminal(result)) => {
                self.observe_terminal();
                result.map(|()| None)
            }
            None => {
                self.observe_terminal();
                Err(MetaError::Service(
                    "service call driver ended without a terminal".to_owned(),
                ))
            }
        }
    }

    /// Requests cooperative cancellation of caller and provider halves.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Sends one request and requires exactly one successful response.
    ///
    /// A terminal provider error is propagated even when it follows the
    /// response; a second successful response is a service protocol error.
    pub async fn unary(mut self, request: ServiceFrame) -> Result<ServiceFrame> {
        self.send(request).await?;
        self.finish();
        let response = self.recv().await?.ok_or_else(|| {
            MetaError::Service("provider ended a unary call without a response".to_owned())
        })?;
        if self.recv().await?.is_some() {
            return Err(MetaError::Service(
                "provider produced more than one unary response".to_owned(),
            ));
        }
        Ok(response)
    }

    fn observe_terminal(&mut self) {
        self.terminal_observed = true;
        self.requests.take();
        Self::drop_response_inbox(&mut self.responses);
        self.lease.take();
    }

    fn drop_response_inbox(inbox: &mut Option<mpsc::Receiver<ResponseMessage>>) {
        inbox.take();
    }
}

impl Drop for ServiceCall {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Generation-fenced capability for one resolved service requirement.
#[derive(Clone, Debug)]
pub struct ServiceHandle {
    pub(crate) runtime: Runtime,
    pub(crate) caller: Context,
    pub(crate) binding: Arc<ProviderBinding>,
    pub(crate) overlay: Arc<crate::runtime::InterceptLayers>,
}

impl ServiceHandle {
    /// Returns the logical service key.
    pub fn key(&self) -> &ServiceKey {
        &self.binding.key
    }

    /// Returns the exact resolved contract identity and version.
    pub fn contract(&self) -> (&ContractId, ContractVersion) {
        (&self.binding.contract, self.binding.version)
    }

    /// Returns the resolved provider Fiber and generation.
    pub fn provider(&self) -> (FiberId, FiberGeneration) {
        (self.binding.provider, self.binding.generation)
    }

    /// Admits a new call after revalidating caller and provider generations.
    ///
    /// The Runtime-owned driver uses the caller Fiber's captured executor, so
    /// this synchronous operation does not require an ambient Tokio context.
    pub fn open(&self) -> Result<ServiceCall> {
        self.runtime.open_service(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_observation_drops_the_response_inbox() {
        let (sender, receiver) = mpsc::channel(1);
        let mut inbox = Some(receiver);
        ServiceCall::drop_response_inbox(&mut inbox);
        assert!(inbox.is_none());
        assert!(sender.is_closed());
    }
}
