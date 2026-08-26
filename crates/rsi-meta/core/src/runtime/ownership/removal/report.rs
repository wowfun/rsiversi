use super::super::super::{CleanupReport, Runtime};
use super::{EventRemoval, RemovalResult};

impl EventRemoval {
    pub(in super::super) fn report(&self, result: &RemovalResult) -> CleanupReport {
        let mut report = CleanupReport::default();
        if let Err(error) = result {
            report.push_bounded(
                self.cleanup_label.clone(),
                error,
                self.maximum_diagnostic_entries,
                self.maximum_diagnostic_bytes,
            );
        }
        report
    }

    pub(in super::super) fn retain_detached_failure(&self, error: &str) {
        let Some(inner) = self.runtime.upgrade() else {
            return;
        };
        let runtime = Runtime { inner };
        let Ok(fiber) = runtime.owner_fiber(self.owner) else {
            return;
        };
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != self.owner.generation {
            return;
        }
        let Some(active) = data.active.as_mut() else {
            return;
        };
        let mut report = CleanupReport::default();
        report.push_bounded(
            self.cleanup_label.clone(),
            error,
            self.maximum_diagnostic_entries,
            self.maximum_diagnostic_bytes,
        );
        active.retired_owned_report.extend_bounded(
            report,
            self.maximum_diagnostic_entries,
            self.maximum_diagnostic_bytes,
        );
    }
}
