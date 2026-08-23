use serde::{Deserialize, Serialize};
use std::fmt;

/// One labeled cleanup failure collected during best-effort teardown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupFailure {
    /// Effect, child, or runtime operation that failed.
    pub label: String,
    /// Bounded human-readable failure reported by that operation.
    pub error: String,
}

/// Complete cleanup outcome; teardown continues after individual failures.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Failures in the order in which teardown observed them.
    pub failures: Vec<CleanupFailure>,
}

impl CleanupReport {
    /// Returns whether every requested cleanup completed successfully.
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }

    pub(crate) fn push(&mut self, label: String, error: impl fmt::Display) {
        self.failures.push(CleanupFailure {
            label,
            error: error.to_string(),
        });
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.failures.extend(other.failures);
    }
}
