use crate::{CallId, Context, FiberGeneration, FiberId};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
