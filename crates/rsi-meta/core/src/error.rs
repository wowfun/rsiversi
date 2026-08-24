use crate::{ContractId, ContractVersion, FiberGeneration, FiberId, ServiceKey};

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
    /// A plugin requested a service absent from its declared requirements.
    #[error("service {service} was not declared as a requirement")]
    UndeclaredRequirement {
        /// Undeclared service key.
        service: ServiceKey,
    },
    /// A plugin published a service absent from its declared provisions.
    #[error("service {service} was not declared as a provision")]
    UndeclaredProvision {
        /// Undeclared service key.
        service: ServiceKey,
    },
    /// No bound provider is available for the declared requirement.
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
    /// The provider in a resolved slot does not satisfy the exact requirement contract.
    #[error(
        "service contract mismatch for {service}: expected {expected_id}@{expected_version:?}, got {actual_id}@{actual_version:?}"
    )]
    ContractMismatch {
        /// Service slot whose contracts differ.
        service: ServiceKey,
        /// Required contract identity.
        expected_id: ContractId,
        /// Required exact contract version.
        expected_version: ContractVersion,
        /// Published contract identity.
        actual_id: ContractId,
        /// Published exact contract version.
        actual_version: ContractVersion,
    },
    /// Another provider already occupies the same service and isolation slot.
    #[error("service slot already has a provider: {service}")]
    DuplicateProvider {
        /// Conflicting service key.
        service: ServiceKey,
    },
    /// An encoded frame, configuration, or value exceeds its owning byte bound.
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
    /// A preparation proof was presented to a Runtime other than its owner.
    #[error("prepared plugin belongs to a different runtime")]
    PreparedForDifferentRuntime,
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
