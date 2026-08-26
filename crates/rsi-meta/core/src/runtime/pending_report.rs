#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

pub(super) struct PendingReportBuilder {
    report: PendingReport,
    retained_entries: usize,
    retained_bytes: usize,
    maximum_entries: usize,
    maximum_bytes: usize,
}

impl PendingReportBuilder {
    pub(super) fn new(payloads: &PayloadLimits) -> Self {
        Self {
            report: PendingReport::default(),
            retained_entries: 0,
            retained_bytes: 0,
            maximum_entries: payloads.maximum_diagnostic_entries,
            maximum_bytes: payloads.maximum_diagnostic_bytes,
        }
    }

    pub(super) fn push_with(
        &mut self,
        retained_entries: usize,
        retained_bytes: usize,
        reason: impl FnOnce() -> PendingReason,
    ) {
        self.report.total_reasons = self.report.total_reasons.saturating_add(1);
        if self.report.truncated {
            return;
        }
        let Some(next_entries) = self.retained_entries.checked_add(retained_entries) else {
            self.report.truncated = true;
            return;
        };
        let Some(next_bytes) = self.retained_bytes.checked_add(retained_bytes) else {
            self.report.truncated = true;
            return;
        };
        if next_entries > self.maximum_entries || next_bytes > self.maximum_bytes {
            self.report.truncated = true;
            return;
        }
        self.retained_entries = next_entries;
        self.retained_bytes = next_bytes;
        self.report.reasons.push(reason());
    }

    pub(super) fn total_reasons(&self) -> usize {
        self.report.total_reasons
    }

    pub(super) fn finish(self) -> PendingReport {
        self.report
    }
}
