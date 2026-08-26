use crate::{FiberGeneration, FiberId, ServiceKey};

/// Result type returned by the `rsi-meta` foundation.
pub type Result<T> = std::result::Result<T, MetaError>;

/// Closed failure taxonomy for composition, lifecycle, service, and event operations.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MetaError {
    /// New work was rejected because shutdown admission has closed.
    #[error("runtime is shutting down")]
    RuntimeShuttingDown,
    /// New work was rejected after an unrecoverable runtime condition.
    #[error("runtime is terminal: {0}")]
    RuntimeTerminal(String),
    /// The named Fiber has completed disposal.
    #[error("fiber {fiber:?} is disposed")]
    FiberDisposed {
        /// Runtime-local Fiber identity.
        fiber: FiberId,
    },
    /// A Context names a Fiber generation that no longer owns live state.
    #[error("stale fiber context {fiber:?}/{generation:?}")]
    StaleContext {
        /// Runtime-local Fiber identity.
        fiber: FiberId,
        /// Generation captured by the stale Context.
        generation: FiberGeneration,
    },
    /// No bound provider is available for the requested service.
    #[error("service {service} is not bound")]
    ServiceUnavailable {
        /// Unavailable service key.
        service: ServiceKey,
    },
    /// A captured service handle no longer names the caller's active binding.
    #[error("service {service} handle is stale")]
    StaleService {
        /// Service key carried by the stale handle.
        service: ServiceKey,
    },
    /// A capability or Message belongs to another Runtime.
    #[error("capability belongs to a different runtime")]
    CapabilityFromDifferentRuntime,
    /// A capability entry was revoked.
    #[error("capability is stale")]
    StaleCapability,
    /// Another provider already occupies the same service and isolation slot.
    #[error("service slot already has a provider: {service}")]
    DuplicateProvider {
        /// Conflicting service key.
        service: ServiceKey,
    },
    /// An encoded Message, configuration, or value exceeds its owning byte bound.
    #[error("payload exceeds the configured {maximum}-byte limit")]
    PayloadTooLarge {
        /// Maximum permitted encoded bytes.
        maximum: usize,
    },
    /// A bounded registry or admission pool has no remaining capacity.
    #[error("bounded runtime capacity exhausted: {resource}")]
    CapacityExhausted {
        /// Stable name of the exhausted resource class.
        resource: &'static str,
    },
    /// A fail-fast operation already has its configured in-flight population.
    #[error("operation is busy: {operation}")]
    Busy {
        /// Stable name of the operation class.
        operation: &'static str,
    },
    /// The current authority recursively entered an operation that forbids its lineage.
    #[error("operation is reentrant: {operation}")]
    Reentrant {
        /// Stable name of the operation class.
        operation: &'static str,
    },
    /// A preparation proof was presented to a Runtime other than its owner.
    #[error("prepared plugin belongs to a different runtime")]
    PreparedForDifferentRuntime,
    /// The activation plan contains no unconsumed opaque prepared state.
    #[error("prepared activation state is unavailable")]
    PreparedStateUnavailable,
    /// The requested Rust type does not match the opaque prepared state.
    #[error("prepared activation state has a different type; expected {expected}")]
    PreparedStateTypeMismatch {
        /// Rust type requested by the factory.
        expected: &'static str,
    },
    /// Plugin configuration normalization rejected its input or output.
    #[error("plugin configuration is invalid: {0}")]
    InvalidConfig(String),
    /// Plugin preparation, activation, or Runtime-owned task execution failed.
    #[error("plugin activation failed: {0}")]
    Activation(String),
    /// A service stream or endpoint failed.
    #[error("service call failed: {0}")]
    Service(String),
    /// A service endpoint unwound instead of returning a bounded error.
    #[error("service endpoint panicked")]
    ServiceEndpointPanicked,
    /// Event dispatch or one of its listeners failed.
    #[error("event dispatch failed: {0}")]
    Event(String),
    /// A named bounded operation exceeded its configured deadline.
    #[error("operation timed out: {0}")]
    Timeout(&'static str),
    /// Cooperative cancellation stopped an operation before completion.
    #[error("operation was cancelled")]
    Cancelled,
    /// Caller or adapter input violates a public structural contract.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}
