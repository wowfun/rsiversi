//! Opt-in deterministic validation instrumentation for adapter consumers.

use super::*;

/// Releases the paused Store worker on explicit or unwinding drop.
#[derive(Debug)]
pub struct WorkerPause(Option<std::sync::mpsc::Sender<()>>);

impl Drop for WorkerPause {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

impl SqliteStore {
    /// Pauses the next cold validation after dispatch; dropping the guard releases it.
    ///
    /// # Panics
    /// Panics if another pause is installed or the test barrier mutex is poisoned.
    pub fn pause_next_validation(&self) -> (tokio::sync::oneshot::Receiver<()>, WorkerPause) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let previous = self
            .inner
            .validation_barrier
            .lock()
            .unwrap()
            .replace((entered_tx, release_rx));
        assert!(
            previous.is_none(),
            "only one validation pause may be installed"
        );
        (entered_rx, WorkerPause(Some(release_tx)))
    }

    /// Pauses the next materialized Fact page after its database snapshot closes.
    ///
    /// # Panics
    /// Panics if a Fact-page pause is already installed or its mutex is poisoned.
    pub fn pause_next_fact_page(&self) -> (tokio::sync::oneshot::Receiver<()>, WorkerPause) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let previous = self
            .inner
            .fact_page_barrier
            .lock()
            .unwrap()
            .replace((entered_tx, release_rx));
        assert!(
            previous.is_none(),
            "only one Fact-page pause may be installed"
        );
        (entered_rx, WorkerPause(Some(release_tx)))
    }

    /// Returns completed/entered validations, materialized Facts, and decoded controls.
    pub fn validation_counts(&self) -> (u64, u64, u64) {
        (
            self.inner.validation_runs.load(Ordering::Relaxed),
            self.inner.fact_materializations.load(Ordering::Relaxed),
            self.inner.control_decodes.load(Ordering::Relaxed),
        )
    }
}
