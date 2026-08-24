use crate::{
    ContractId, ContractVersion, FactoryIdentity, FiberGeneration, FiberId, IsolationId, ServiceKey,
};
use serde::{Deserialize, Serialize};

/// Why a non-active Fiber cannot currently resolve and activate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingReason {
    /// No provider is published in the selected isolation slot.
    MissingService {
        /// Missing logical service key.
        service: ServiceKey,
        /// Isolation slot selected by the Fiber Context.
        isolation: IsolationId,
    },
    /// A provider exists but does not match the exact declared contract.
    ContractMismatch {
        /// Logical service key.
        service: ServiceKey,
        /// Required contract identity.
        expected: ContractId,
        /// Required exact version.
        expected_version: ContractVersion,
        /// Published contract identity.
        actual: ContractId,
        /// Published exact version.
        actual_version: ContractVersion,
    },
    /// Reachable pending declarations form a dependency cycle.
    DependencyCycle {
        /// Ordered service path that closes the cycle.
        services: Vec<ServiceKey>,
    },
}

/// Bounded dependency-convergence diagnostics retained by one Fiber.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingReport {
    /// Deterministic retained prefix within the Runtime diagnostic policy.
    pub reasons: Vec<PendingReason>,
    /// Total reasons observed, including reasons omitted from `reasons`.
    pub total_reasons: usize,
    /// Whether a reason or nested dependency-cycle service was omitted.
    pub truncated: bool,
}

/// Observable lifecycle state of one Fiber.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiberState {
    /// Dependency convergence has not produced an activatable snapshot.
    Pending(PendingReport),
    /// One generation is staging owned resources.
    Loading,
    /// The staged generation is published.
    Active,
    /// The latest activation or retirement transaction failed.
    Failed(String),
    /// Publications are withdrawn and owned resources are retiring.
    Unloading,
    /// Final teardown completed and the Fiber left the registry.
    Disposed,
}

impl FiberState {
    pub(super) fn is_transitioning(&self) -> bool {
        matches!(self, Self::Loading | Self::Unloading)
    }
}

/// Immutable observation of one Fiber at one Runtime revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FiberSnapshot {
    /// Runtime-local Fiber identity.
    pub id: FiberId,
    /// Latest assigned activation generation.
    pub generation: FiberGeneration,
    /// Factory identity captured during preparation.
    pub factory: FactoryIdentity,
    /// Current lifecycle state.
    pub state: FiberState,
}

/// Point-in-time observation of Runtime lifecycle and registered Fibers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    /// Monotonic registry revision.
    pub revision: u64,
    /// Whether shutdown admission has closed.
    pub shutting_down: bool,
    /// First terminal reason, when the Runtime has fenced new work.
    pub terminal: Option<String>,
    /// Fiber snapshots ordered by Fiber identity.
    pub fibers: Vec<FiberSnapshot>,
}
