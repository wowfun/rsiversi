use super::{CleanupFailure, CleanupReport};
use std::fmt;

mod wire;

impl CleanupReport {
    /// Returns whether every requested cleanup completed successfully.
    pub fn is_clean(&self) -> bool {
        self.total_failures == 0
    }

    /// Returns the retained failures in teardown observation order.
    pub fn failures(&self) -> &[CleanupFailure] {
        &self.failures
    }

    /// Returns the total observed failure count, including omitted entries.
    pub fn total_failures(&self) -> usize {
        self.total_failures
    }

    /// Returns whether any failure entry or diagnostic content was omitted.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub(crate) fn push_bounded(
        &mut self,
        mut label: String,
        error: impl fmt::Display,
        maximum_entries: usize,
        maximum_bytes: usize,
    ) {
        self.total_failures = self.total_failures.saturating_add(1);
        if self.failures.len() >= maximum_entries {
            self.truncated = true;
            return;
        }
        let Some(available) = maximum_bytes.checked_sub(self.retained_bytes) else {
            self.truncated = true;
            return;
        };
        if available == 0 {
            self.truncated = true;
            return;
        }
        if label.len() > available {
            label.truncate(utf8_prefix(&label, available));
            self.truncated = true;
        }
        let error_limit = available.saturating_sub(label.len());
        let (error, error_truncated) = format_bounded(error, error_limit);
        self.truncated |= error_truncated;
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(label.len().saturating_add(error.len()));
        self.failures.push(CleanupFailure { label, error });
    }

    pub(crate) fn extend_bounded(
        &mut self,
        other: Self,
        maximum_entries: usize,
        maximum_bytes: usize,
    ) {
        let Self {
            failures,
            total_failures,
            truncated,
            ..
        } = other;
        let mut retaining_prefix = self.total_failures == self.failures.len();
        self.total_failures = self.total_failures.saturating_add(total_failures);
        self.truncated |= truncated;
        for failure in failures {
            let failure_bytes = failure.label.len().saturating_add(failure.error.len());
            let next_bytes = self.retained_bytes.checked_add(failure_bytes);
            if retaining_prefix
                && self.failures.len() < maximum_entries
                && next_bytes.is_some_and(|bytes| bytes <= maximum_bytes)
            {
                self.retained_bytes = next_bytes.expect("checked retained cleanup bytes");
                self.failures.push(failure);
            } else {
                self.truncated = true;
                retaining_prefix = false;
            }
        }
    }
}

fn utf8_prefix(value: &str, maximum: usize) -> usize {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn format_bounded(value: impl fmt::Display, maximum: usize) -> (String, bool) {
    struct Writer {
        value: String,
        maximum: usize,
        truncated: bool,
    }

    impl fmt::Write for Writer {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            if self.truncated {
                return Ok(());
            }
            let remaining = self.maximum.saturating_sub(self.value.len());
            let retained = utf8_prefix(value, remaining);
            self.value.push_str(&value[..retained]);
            self.truncated |= retained != value.len();
            Ok(())
        }
    }

    let mut writer = Writer {
        value: String::new(),
        maximum,
        truncated: false,
    };
    fmt::write(&mut writer, format_args!("{value}"))
        .expect("bounded cleanup diagnostic formatting cannot fail");
    (writer.value, writer.truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_cardinality_collection_is_linear_and_preserves_wire_semantics() {
        const FAILURES: usize = 65_536;
        const RETAINED_BYTES: usize = FAILURES * 2;
        let mut report = CleanupReport::default();
        for _ in 0..FAILURES {
            report.push_bounded("l".to_owned(), "e", FAILURES, RETAINED_BYTES);
        }
        assert_eq!(report.failures.len(), FAILURES);
        assert_eq!(report.total_failures, FAILURES);
        assert!(!report.truncated);

        let encoded = serde_json::to_value(&report).unwrap();
        assert_eq!(encoded.as_object().unwrap().len(), 3);
        let decoded: CleanupReport = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, report);

        let mut joined = decoded;
        let mut additional = CleanupReport::default();
        additional.push_bounded("l".to_owned(), "e", 1, 2);
        joined.extend_bounded(additional, FAILURES + 1, RETAINED_BYTES);
        assert_eq!(joined.total_failures, FAILURES + 1);
        assert_eq!(joined.failures.len(), FAILURES);
        assert!(joined.truncated);
    }
}
