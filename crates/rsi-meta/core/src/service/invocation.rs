use super::CancellationObserver;
use crate::runtime::CallbackLease;
use crate::{CallId, CallerEffect, Context, FiberGeneration, FiberId, MetaError, Result};
use std::fmt;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Immutable call facts and scoped capabilities passed to a provider callback.
#[derive(Clone)]
pub struct InvocationContext {
    call_id: CallId,
    lineage_call_id: CallId,
    parent_call_id: Option<CallId>,
    origin: FiberId,
    immediate_caller: FiberId,
    provider: FiberId,
    provider_generation: FiberGeneration,
    caller: CallerView,
    caller_effect: Option<CallerEffect>,
    callback: Arc<CallbackLease>,
    provider_context: Context,
    cancellation: CancellationToken,
}

/// Callback-lifetime immutable view of one invocation's exact caller.
///
/// The view deliberately exposes no Runtime or mutable [`Context`] authority.
/// Its reads fail after callback cancellation or caller-generation retirement.
#[derive(Clone)]
pub struct CallerView {
    context: Context,
    cancellation: CancellationToken,
    callback: Arc<CallbackLease>,
}

impl CallerView {
    fn callback_closed(&self) -> MetaError {
        self.context
            .owner()
            .map_or(MetaError::Cancelled, |(fiber, generation)| {
                MetaError::StaleContext { fiber, generation }
            })
    }

    /// Returns the caller generation, or `None` for an unowned root event.
    pub fn owner(&self) -> Result<Option<(FiberId, FiberGeneration)>> {
        self.callback.with_open(self.callback_closed(), || {
            self.context.validate_callback_view(&self.cancellation)?;
            Ok(self.context.owner())
        })
    }
}

impl fmt::Debug for CallerView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallerView")
            .field("owner", &self.context.owner())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for InvocationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InvocationContext")
            .field("call_id", &self.call_id)
            .field("lineage_call_id", &self.lineage_call_id)
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
        lineage_call_id: CallId,
        parent_call_id: Option<CallId>,
        origin: FiberId,
        immediate_caller: FiberId,
        provider: FiberId,
        provider_generation: FiberGeneration,
        caller_context: Context,
        provider_context: Context,
        cancellation: CancellationToken,
    ) -> Self {
        let callback = Arc::new(CallbackLease::new());
        let caller_effect =
            caller_context.callback_caller_effect(cancellation.clone(), Arc::clone(&callback));
        let caller = CallerView {
            context: caller_context,
            cancellation: cancellation.clone(),
            callback: Arc::clone(&callback),
        };
        Self {
            call_id,
            lineage_call_id,
            parent_call_id,
            origin,
            immediate_caller,
            provider,
            provider_generation,
            caller,
            caller_effect,
            callback,
            provider_context,
            cancellation,
        }
    }

    /// Returns this Runtime-local call identity.
    pub fn call_id(&self) -> CallId {
        self.call_id
    }

    /// Returns the Runtime-issued nonzero activation seed for this complete
    /// nested call chain.
    pub fn lineage_call_id(&self) -> CallId {
        self.lineage_call_id
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

    /// Returns the callback-lifetime immutable caller view.
    pub fn caller(&self) -> &CallerView {
        &self.caller
    }

    /// Returns narrow caller-owned cleanup authority when the caller is a Fiber generation.
    pub fn caller_effect(&self) -> Option<&CallerEffect> {
        self.caller_effect.as_ref()
    }

    pub(crate) fn callback_lease(&self) -> Arc<CallbackLease> {
        Arc::clone(&self.callback)
    }

    /// Returns the generation-fenced provider Context.
    pub fn provider_context(&self) -> &Context {
        &self.provider_context
    }

    /// Returns an observation-only view of cancellation for the complete service call.
    pub fn cancellation(&self) -> CancellationObserver {
        CancellationObserver::new(self.cancellation.clone())
    }
}
