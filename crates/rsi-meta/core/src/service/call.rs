use super::channel::{MessageBudget, send_message};
use super::message::validate_message_bounds;
use super::{
    BufferedMessage, BufferedMessageAdmission, LeaseGuard, Message, MessageChannel, ResponseMessage,
};
use crate::runtime::{ResourceLedger, ResourceReservation};
use crate::{
    Context, ContractId, ContractVersion, FiberGeneration, FiberId, MetaError, Result, ServiceKey,
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
    _caller: LeaseGuard,
}

impl CallLease {
    pub(crate) fn new(
        runtime: LeaseGuard,
        caller: LeaseGuard,
        admission: OwnedSemaphorePermit,
        resource: ResourceReservation,
    ) -> Self {
        Self {
            _resource: resource,
            _admission: admission,
            _runtime: runtime,
            _caller: caller,
        }
    }
}

impl fmt::Debug for CallLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("CallLease").finish_non_exhaustive()
    }
}

/// Cloneable observation-only view of one service call's cancellation fact.
#[derive(Clone)]
pub struct CancellationObserver {
    cancellation: CancellationToken,
}

impl CancellationObserver {
    pub(crate) fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation }
    }

    /// Reports whether cooperative cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Waits until cooperative cancellation has been requested.
    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }
}

impl fmt::Debug for CancellationObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationObserver")
            .field("is_cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Caller half of one admitted bounded bidirectional service call.
#[derive(Debug)]
pub struct CapabilityCall {
    pub(crate) context: Context,
    pub(crate) requests: Option<mpsc::Sender<BufferedMessage>>,
    pub(crate) responses: Option<mpsc::Receiver<ResponseMessage>>,
    pub(crate) message_admission: Arc<BufferedMessageAdmission>,
    pub(crate) request_channel: Arc<MessageChannel>,
    pub(crate) byte_resources: Arc<ResourceLedger>,
    pub(crate) capability_resources: Arc<ResourceLedger>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) deadline: Instant,
    pub(crate) maximum_message_bytes: usize,
    pub(crate) maximum_capabilities_per_message: usize,
    pub(crate) lease: Option<Arc<CallLease>>,
    pub(crate) terminal_result: Option<Result<()>>,
}

impl CapabilityCall {
    /// Sends one bounded request Message.
    pub async fn send(&self, message: Message) -> Result<()> {
        validate_message_bounds(
            &message,
            self.maximum_message_bytes,
            self.maximum_capabilities_per_message,
        )?;
        let Some(requests) = self.requests.as_ref() else {
            return Err(MetaError::Cancelled);
        };
        send_message(
            requests,
            message,
            std::convert::identity,
            MessageBudget {
                sender: &self.context,
                maximum_message_bytes: self.maximum_message_bytes,
                maximum_capabilities_per_message: self.maximum_capabilities_per_message,
                message_admission: &self.message_admission,
                channel: &self.request_channel,
                byte_resources: &self.byte_resources,
                capability_resources: &self.capability_resources,
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
    /// Once observed, that terminal result is sticky: later reads repeat an
    /// error and only a clean terminal remains EOF.
    /// Cooperative cancellation joins the driver's unique terminal instead of
    /// synthesizing a competing result in the caller half.
    pub async fn recv(&mut self) -> Result<Option<Message>> {
        if let Some(result) = self.terminal_result.clone() {
            return result.map(|()| None);
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
                return self.observe_terminal(Err(MetaError::Timeout("service call")));
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
                        return self.observe_terminal(Err(MetaError::Timeout("service call")));
                    }
                }
            }
        };
        match message {
            Some(ResponseMessage::Message(message)) => {
                Ok(Some(message.into_message(&self.context)))
            }
            Some(ResponseMessage::Terminal(result)) => self.observe_terminal(result),
            None => self.observe_terminal(Err(MetaError::Service(
                "service call driver ended without a terminal".to_owned(),
            ))),
        }
    }

    /// Requests cooperative cancellation of caller and provider halves.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Reports whether this exact call has observed cooperative cancellation.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Returns an observation-only view that remains valid if this caller half moves.
    pub fn cancellation_observer(&self) -> CancellationObserver {
        CancellationObserver::new(self.cancellation.clone())
    }

    fn observe_terminal(&mut self, result: Result<()>) -> Result<Option<Message>> {
        self.terminal_result = Some(result.clone());
        self.requests.take();
        Self::drop_response_inbox(&mut self.responses);
        self.lease.take();
        result.map(|()| None)
    }

    fn drop_response_inbox(inbox: &mut Option<mpsc::Receiver<ResponseMessage>>) {
        inbox.take();
    }
}

impl Drop for CapabilityCall {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Transferable generation-fenced authority for one service endpoint.
#[derive(Clone)]
pub struct Capability {
    pub(crate) holder: Context,
    pub(crate) entry: Arc<crate::runtime::CapabilityEntry>,
}

impl Capability {
    /// Returns the logical service key.
    pub fn key(&self) -> &ServiceKey {
        &self.entry.binding.key
    }

    /// Returns the exact resolved contract identity and version.
    pub fn contract(&self) -> (&ContractId, ContractVersion) {
        (&self.entry.binding.contract, self.entry.binding.version)
    }

    /// Returns the resolved provider Fiber and generation.
    pub fn provider(&self) -> (FiberId, FiberGeneration) {
        (self.entry.binding.provider, self.entry.binding.generation)
    }

    /// Admits a new call after revalidating caller and provider generations.
    ///
    /// The Runtime-owned driver uses the caller Fiber's captured executor, so
    /// this synchronous operation does not require an ambient Tokio context.
    pub fn open(&self) -> Result<CapabilityCall> {
        self.holder.runtime().open_service(self)
    }

    /// Sends exactly one request and accepts only one response plus clean EOF.
    pub async fn invoke(&self, request: Message) -> Result<Message> {
        let mut call = self.open()?;
        call.send(request).await?;
        call.finish();
        let response = call.recv().await?.ok_or_else(|| {
            MetaError::Service("provider ended a unary call without a response".to_owned())
        })?;
        if call.recv().await?.is_some() {
            return Err(MetaError::Service(
                "provider produced more than one unary response".to_owned(),
            ));
        }
        Ok(response)
    }
}

impl fmt::Debug for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Capability")
            .field("key", &self.entry.binding.key)
            .field("contract", &self.entry.binding.contract)
            .field("version", &self.entry.binding.version)
            .field(
                "provider",
                &(self.entry.binding.provider, self.entry.binding.generation),
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_observation_drops_the_response_inbox() {
        let (sender, receiver) = mpsc::channel(1);
        let mut inbox = Some(receiver);
        CapabilityCall::drop_response_inbox(&mut inbox);
        assert!(inbox.is_none());
        assert!(sender.is_closed());
    }
}
