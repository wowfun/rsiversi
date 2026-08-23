use crate::{
    CallId, Context, ContractId, ContractVersion, FiberGeneration, FiberId, MetaError, Result,
    Runtime, ServiceKey,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod admission;
pub(crate) use admission::{AdmissionLease, LeaseGuard};

/// Immutable call facts and scoped capabilities passed to a provider callback.
#[derive(Clone)]
pub struct InvocationContext {
    call_id: CallId,
    parent_call_id: Option<CallId>,
    origin: FiberId,
    immediate_caller: FiberId,
    provider: FiberId,
    provider_generation: FiberGeneration,
    edge_overlay: Arc<crate::runtime::InterceptLayers>,
    caller_context: Context,
    provider_context: Context,
    cancellation: CancellationToken,
}

impl fmt::Debug for InvocationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InvocationContext")
            .field("call_id", &self.call_id)
            .field("parent_call_id", &self.parent_call_id)
            .field("origin", &self.origin)
            .field("immediate_caller", &self.immediate_caller)
            .field("provider", &self.provider)
            .field("provider_generation", &self.provider_generation)
            .finish_non_exhaustive()
    }
}

impl InvocationContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        call_id: CallId,
        parent_call_id: Option<CallId>,
        origin: FiberId,
        immediate_caller: FiberId,
        provider: FiberId,
        provider_generation: FiberGeneration,
        edge_overlay: Arc<crate::runtime::InterceptLayers>,
        caller_context: Context,
        provider_context: Context,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            call_id,
            parent_call_id,
            origin,
            immediate_caller,
            provider,
            provider_generation,
            edge_overlay,
            caller_context,
            provider_context,
            cancellation,
        }
    }

    /// Returns this Runtime-local call identity.
    pub fn call_id(&self) -> CallId {
        self.call_id
    }

    /// Returns the enclosing call when this provider called another service.
    pub fn parent_call_id(&self) -> Option<CallId> {
        self.parent_call_id
    }

    /// Returns the Fiber that originated the complete nested call chain.
    pub fn origin(&self) -> FiberId {
        self.origin
    }

    /// Returns the Fiber that directly opened this call.
    pub fn immediate_caller(&self) -> FiberId {
        self.immediate_caller
    }

    /// Returns the provider Fiber and generation admitted for this call.
    pub fn provider(&self) -> (FiberId, FiberGeneration) {
        (self.provider, self.provider_generation)
    }

    /// Returns immutable intercept layers attached to this direct requirement edge.
    pub fn edge_overlay(&self) -> &[Value] {
        self.edge_overlay.as_slice()
    }

    /// Returns the generation-fenced caller Context.
    pub fn caller_context(&self) -> &Context {
        &self.caller_context
    }

    /// Returns the generation-fenced provider Context.
    pub fn provider_context(&self) -> &Context {
        &self.provider_context
    }

    /// Returns cooperative cancellation for the complete service call.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

/// One bounded opaque byte frame in a service stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceFrame {
    bytes: Vec<u8>,
}

impl ServiceFrame {
    /// Creates a frame; the owning send operation enforces its byte bound.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    /// Borrows the exact frame bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the frame and returns its bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Provider half of one bounded bidirectional service call.
#[derive(Debug)]
pub struct ProviderChannel {
    pub(crate) requests: mpsc::Receiver<ServiceFrame>,
    pub(crate) responses: mpsc::Sender<Result<ServiceFrame>>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) maximum_frame_bytes: usize,
}

impl ProviderChannel {
    /// Receives the next caller frame, or `None` after finish or cancellation.
    pub async fn recv(&mut self) -> Option<ServiceFrame> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            frame = self.requests.recv() => frame,
        }
    }

    /// Sends one bounded successful response frame.
    pub async fn send(&self, frame: ServiceFrame) -> Result<()> {
        if frame.as_bytes().len() > self.maximum_frame_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: self.maximum_frame_bytes,
            });
        }
        self.responses
            .send(Ok(frame))
            .await
            .map_err(|_| MetaError::Cancelled)
    }

    /// Returns cooperative cancellation for this call.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

/// Async byte-stream provider implemented at the service seam.
#[async_trait]
pub trait ServiceEndpoint: std::fmt::Debug + Send + Sync + 'static {
    /// Serves one admitted generation-fenced call to completion.
    async fn serve(&self, invocation: InvocationContext, channel: ProviderChannel) -> Result<()>;
}

/// Caller half of one admitted bounded bidirectional service call.
#[derive(Debug)]
pub struct ServiceCall {
    pub(crate) requests: Option<mpsc::Sender<ServiceFrame>>,
    pub(crate) responses: mpsc::Receiver<Result<ServiceFrame>>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) maximum_frame_bytes: usize,
}

impl ServiceCall {
    /// Sends one bounded request frame.
    pub async fn send(&self, frame: ServiceFrame) -> Result<()> {
        if frame.as_bytes().len() > self.maximum_frame_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: self.maximum_frame_bytes,
            });
        }
        let requests = self.requests.as_ref().ok_or(MetaError::Cancelled)?;
        requests.send(frame).await.map_err(|_| MetaError::Cancelled)
    }

    /// Closes the request stream while retaining the response stream.
    pub fn finish(&mut self) {
        self.requests.take();
    }

    /// Receives the next response or terminal provider error.
    pub async fn recv(&mut self) -> Option<Result<ServiceFrame>> {
        self.responses.recv().await
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
        let response = self.recv().await.ok_or_else(|| {
            MetaError::Service("provider ended a unary call without a response".to_owned())
        })??;
        if let Some(terminal) = self.recv().await {
            match terminal {
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(MetaError::Service(
                        "provider produced more than one unary response".to_owned(),
                    ));
                }
            }
        }
        Ok(response)
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
    pub fn open(&self) -> Result<ServiceCall> {
        self.runtime.open_service(self)
    }
}

#[derive(Debug)]
pub(crate) struct ProviderBinding {
    pub key: ServiceKey,
    pub contract: ContractId,
    pub version: ContractVersion,
    pub provider: FiberId,
    pub generation: FiberGeneration,
    pub endpoint: Arc<dyn ServiceEndpoint>,
    pub lease: Arc<AdmissionLease>,
}
