use crate::runtime::ContextExtensions;
use crate::{ContextExtension, FiberGeneration, FiberId, Result};
use std::sync::Arc;

/// Immutable listener metadata presented to one dispatch target.
///
/// The view grants no Context mutation, handler access, listener identity, or
/// removal authority. Product targeting can inspect only the exact owner and
/// typed safe-Rust extensions captured when the listener was registered.
#[derive(Clone)]
pub struct ListenerView {
    owner: FiberId,
    generation: FiberGeneration,
    extensions: Arc<ContextExtensions>,
}

impl ListenerView {
    pub(crate) fn new(
        owner: FiberId,
        generation: FiberGeneration,
        extensions: Arc<ContextExtensions>,
    ) -> Self {
        Self {
            owner,
            generation,
            extensions,
        }
    }

    /// Returns the exact generation that owns this listener.
    pub fn owner(&self) -> (FiberId, FiberGeneration) {
        (self.owner, self.generation)
    }

    /// Returns one typed extension captured by the listener Context.
    pub fn extension<K: ContextExtension>(&self) -> Option<Arc<K::Value>> {
        self.extensions.get::<K>()
    }
}

impl std::fmt::Debug for ListenerView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ListenerView")
            .field("owner", &self.owner)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Host-owned listener selector evaluated before event callback admission.
///
/// Selection is synchronous and runs outside Runtime locks on blocking work
/// bounded by dispatch admission. The event deadline can return to the caller
/// before a blocked selector, but the Runtime retains that work and starts no
/// later selector or callback from the expired dispatch. A target error or
/// panic rejects the complete dispatch before any ordinary callback or once
/// claim starts.
/// Listeners marked [`crate::EventOptions::global`] bypass this trait.
pub trait EventTarget: std::fmt::Debug + Send + Sync + 'static {
    /// Selects one immutable listener view for this dispatch.
    fn select(&self, listener: &ListenerView) -> Result<bool>;
}
