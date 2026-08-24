use super::{CleanupFailure, CleanupReport};
use serde::{Deserialize, Deserializer, de::Error as _};
use std::fmt;

#[allow(clippy::missing_fields_in_debug)] // Private byte bookkeeping is not report value.
impl fmt::Debug for CleanupReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CleanupReport")
            .field("failures", &self.failures)
            .field("total_failures", &self.total_failures)
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl PartialEq for CleanupReport {
    fn eq(&self, other: &Self) -> bool {
        self.failures == other.failures
            && self.total_failures == other.total_failures
            && self.truncated == other.truncated
    }
}

impl Eq for CleanupReport {}

impl<'de> Deserialize<'de> for CleanupReport {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireReport {
            failures: Vec<CleanupFailure>,
            total_failures: usize,
            truncated: bool,
        }

        let WireReport {
            failures,
            total_failures,
            truncated,
        } = WireReport::deserialize(deserializer)?;
        if total_failures < failures.len() {
            return Err(D::Error::custom(
                "cleanup total_failures is smaller than the retained failure count",
            ));
        }
        if total_failures > failures.len() && !truncated {
            return Err(D::Error::custom(
                "cleanup report omitted failures without setting truncated",
            ));
        }
        if total_failures == 0 && truncated {
            return Err(D::Error::custom(
                "a clean cleanup report cannot be truncated",
            ));
        }
        let retained_bytes = failures.iter().fold(0_usize, |total, failure| {
            total.saturating_add(failure.label.len().saturating_add(failure.error.len()))
        });
        Ok(CleanupReport {
            failures,
            total_failures,
            truncated,
            retained_bytes,
        })
    }
}
