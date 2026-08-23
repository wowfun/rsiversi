//! Process-local foundation for composable plugins.
//!
//! The public interface is intentionally smaller than the implementation it
//! controls: [`Runtime`] owns convergence, [`Context`] carries scope, and each
//! call to [`Context::apply`] creates one independently owned [`FiberHandle`].
//! Package discovery, persistence, file watching, and product semantics belong
//! in plugins or callers.
//!
//! Async operations must be polled inside a Tokio runtime with time enabled;
//! they use Tokio-owned tasks, channels, cancellation, and deadlines. Cloned
//! contexts and handles retain the runtime allocation, so dropping another
//! [`Runtime`] clone does not invalidate them. Call [`Runtime::shutdown`] when
//! deterministic teardown and its [`CleanupReport`] are required.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod cleanup;
mod error;
mod events;
mod ids;
mod listener_registry;
mod plugin;
mod runtime;
mod service;

pub use cleanup::{CleanupFailure, CleanupReport};
pub use error::{MetaError, Result};
pub use events::{DispatchMode, EventHandler, EventOptions, EventOutcome, EventReceipt};
pub use ids::{
    CallId, ContractId, ContractVersion, EventKey, EventListenerId, FiberGeneration, FiberId,
    IsolationId, ServiceKey,
};
pub use plugin::{
    Cleanup, CleanupFuture, ConfigValue, FactoryIdentity, PluginDescriptor, PluginFactory,
    Provision, Requirement,
};
pub use runtime::{
    Context, FiberHandle, FiberSnapshot, FiberState, PendingReason, PreparedPlugin, Runtime,
    RuntimeLimits, RuntimeSnapshot,
};
pub use service::{
    InvocationContext, ProviderChannel, ServiceCall, ServiceEndpoint, ServiceFrame, ServiceHandle,
};
