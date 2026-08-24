use crate::{FiberGeneration, FiberId};
use serde::{Deserialize, Serialize};

mod report;

/// One labeled cleanup failure collected during best-effort teardown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupFailure {
    /// Effect, child, or runtime operation that failed.
    pub label: String,
    /// Bounded human-readable failure reported by that operation.
    pub error: String,
}

/// Complete cleanup outcome; teardown continues after individual failures.
///
/// Report state is observation-only so callers cannot invalidate the relationship
/// between retained failures, the total count, truncation, and [`Self::is_clean`].
///
/// ```compile_fail
/// use rsi_meta::CleanupReport;
///
/// let mut report = CleanupReport::default();
/// report.total_failures = 1;
/// ```
#[derive(Clone, Default, Serialize)]
pub struct CleanupReport {
    /// Failures in the order in which teardown observed them.
    pub(crate) failures: Vec<CleanupFailure>,
    /// Total observed failures, including entries omitted by a diagnostic bound.
    /// This is always at least `failures.len()`.
    pub(crate) total_failures: usize,
    /// Whether failure entries or diagnostic content were omitted or truncated.
    /// A larger `total_failures` therefore always requires this flag.
    pub(crate) truncated: bool,
    #[serde(skip)]
    retained_bytes: usize,
}

/// Observable phase of Runtime-owned teardown that has not reached quiescence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPhase {
    /// Teardown was admitted but has not started withdrawing ownership.
    Scheduled,
    /// Publications are being withdrawn from the Runtime registry.
    Withdrawing,
    /// Retirement is waiting for dependent Fibers to converge.
    WaitingForDependents,
    /// Provider calls and listener callbacks are draining.
    DrainingAdmissions,
    /// Owned child Fibers are being disposed.
    DisposingChildren,
    /// Reverse-ordered cleanup effects are running.
    RunningEffects,
}

/// One bounded diagnostic sample for teardown still owned by the Runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedCleanup {
    /// Runtime-local Fiber identity.
    pub fiber: FiberId,
    /// Latest generation observed for the Fiber.
    pub generation: FiberGeneration,
    /// Current best-effort lifecycle phase.
    pub phase: CleanupPhase,
}

/// Bounded snapshot of Runtime-owned teardown that outlived one shutdown waiter.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedCleanupReport {
    /// Total unresolved Fibers at snapshot time.
    pub total: usize,
    /// Deterministic prefix bounded by the Runtime diagnostic policy.
    pub samples: Vec<UnresolvedCleanup>,
    /// Whether additional unresolved Fibers were omitted.
    pub truncated: bool,
}

/// Result of waiting once for Runtime shutdown to reach quiescence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ShutdownOutcome {
    /// Every admitted teardown completed and no Runtime-owned mutation remains.
    Complete(CleanupReport),
    /// The waiter deadline elapsed while teardown remained Runtime-owned.
    TimedOut {
        /// Failures observed before this waiter deadline.
        report: CleanupReport,
        /// Bounded snapshot of work that is still tracked.
        unresolved: UnresolvedCleanupReport,
    },
    /// The persistent shutdown driver failed before it could prove quiescence.
    Failed {
        /// Failures observed before or during the driver failure.
        report: CleanupReport,
        /// Bounded snapshot of work whose completion is not proven.
        unresolved: UnresolvedCleanupReport,
    },
}

impl ShutdownOutcome {
    /// Returns the cleanup report observed by this waiter.
    pub fn report(&self) -> &CleanupReport {
        match self {
            Self::Complete(report)
            | Self::TimedOut { report, .. }
            | Self::Failed { report, .. } => report,
        }
    }

    /// Returns whether shutdown reached quiescence without cleanup failures.
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Complete(report) if report.is_clean())
    }

    /// Returns whether shutdown reached quiescence before this waiter returned.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }
}
