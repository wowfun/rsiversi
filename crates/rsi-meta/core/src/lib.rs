//! Process-local foundation for composable plugins.
//!
//! The public interface is intentionally smaller than the implementation it
//! controls: [`Runtime`] owns convergence, [`Context`] carries scope, and each
//! call to [`Context::apply`] creates one independently owned [`FiberHandle`].
//! Package discovery, persistence, file watching, and product semantics belong
//! in plugins or callers.
//!
//! Async operations use the Runtime's explicit execution dependency for owned
//! tasks and deadlines. Native convenience constructors capture Tokio. Cloned
//! contexts and handles retain the runtime allocation, so dropping another
//! [`Runtime`] clone does not invalidate them. Call [`Runtime::shutdown`] when
//! deterministic teardown and its [`ShutdownOutcome`] are required.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod cleanup;
mod error;
mod ids;
mod local_events;
mod plugin;
mod runtime;
mod service;

pub use cleanup::{
    CleanupFailure, CleanupPhase, CleanupReport, ShutdownOutcome, UnresolvedCleanup,
    UnresolvedCleanupReport,
};
pub use error::{MetaError, Result};
pub use ids::{
    CallId, ContractId, ContractVersion, EventListenerId, FiberGeneration, FiberId, IsolationId,
    LocalIsolationId, LocalSupplyId, ServiceKey, SupplyId,
};
pub use local_events::{
    Bail, BailEventHandler, Emit, EmitEventHandler, LocalEvent, LocalEventMode, LocalEventOptions,
    LocalEventSnapshot, MAXIMUM_PARALLEL_EVENT_CALLBACKS, Parallel, ParallelEventHandler, Serial,
    SerialEventHandler, Waterfall, WaterfallEventHandler,
};
pub use plugin::{
    ActivationPlan, Cleanup, CleanupFuture, ConfigValue, FactoryIdentity, InstanceId,
    LocalContract, LocalContractKey, PluginFactory, PluginId, PreparedActivation, Requirement,
    ResolvedFactory, UpdateMode,
};
pub use rsi_meta_contract::LocalEventKey;
pub use rsi_meta_execution::{Deadline, Execution, Task};
pub use runtime::{
    CallerEffect, ChildPosition, Context, DeadlineLimits, DetachedCapability, EffectHandle,
    EffectTxn, ExecutionLimits, FiberHandle, FiberSnapshot, FiberState, LocalEventHandle,
    LocalSupplyHandle, MAXIMUM_JSON_DEPTH, MAXIMUM_OPERATION_DEADLINE,
    MAXIMUM_WATERFALL_LISTENERS_PER_SLOT, PayloadLimits, PendingReason, PendingReport,
    PreparedPlugin, RegistrationContext, RegistrationLease, RegistrationOrderSnapshot,
    RegistrationPosition, RegistrationRank, ResourceUsageSnapshot, Runtime, RuntimeIdentity,
    RuntimeLimits, RuntimeResourceSnapshot, RuntimeSnapshot, SupplyHandle, TopologyLimits,
};
pub use service::{
    CallerView, CancellationObserver, Capability, CapabilityCall, InvocationContext, Message,
    ProviderChannel, ServiceEndpoint,
};
