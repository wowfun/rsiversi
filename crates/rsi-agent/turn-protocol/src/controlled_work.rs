//! Process-local proof that an Executor's controlled work has settled.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

/// Observation independent of the durable Turn outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlledWorkStatus {
    /// A claim driver or tracked effect still owns a settlement guard.
    Running,
    /// Every guard confirmed settlement and finalization succeeded.
    Settled,
    /// Work stopped being observable without complete settlement proof.
    Unsettled,
}

/// Read-only exact-generation observation; retaining it confers no authority.
#[derive(Clone, Debug)]
pub struct ControlledWork {
    done: CancellationToken,
    settled: Arc<AtomicBool>,
}

impl ControlledWork {
    /// Creates one observation and its sole publication owner.
    pub fn new() -> (Self, ControlledWorkReporter) {
        let observation = Self {
            done: CancellationToken::new(),
            settled: Arc::new(AtomicBool::new(false)),
        };
        let reporter = ControlledWorkReporter(observation.clone());
        (observation, reporter)
    }
    /// Samples the current state without I/O.
    pub fn status(&self) -> ControlledWorkStatus {
        if !self.done.is_cancelled() {
            ControlledWorkStatus::Running
        } else if self.settled.load(Ordering::Acquire) {
            ControlledWorkStatus::Settled
        } else {
            ControlledWorkStatus::Unsettled
        }
    }
    /// Waits for non-running state. Cancellation returns the current sample.
    pub async fn wait(&self, cancellation: CancellationToken) -> ControlledWorkStatus {
        tokio_util::sync::CancellationToken::run_until_cancelled(
            &cancellation,
            self.done.cancelled(),
        )
        .await;
        self.status()
    }
}

/// Sole owner of completion publication. Dropping it reports Unsettled.
#[derive(Debug)]
pub struct ControlledWorkReporter(ControlledWork);
impl ControlledWorkReporter {
    /// Publishes complete evidence exactly once; false records an uncertain exit.
    pub fn finish(self, settled: bool) {
        self.0.settled.store(settled, Ordering::Release);
    }
}
impl Drop for ControlledWorkReporter {
    fn drop(&mut self) {
        self.0.done.cancel();
    }
}
