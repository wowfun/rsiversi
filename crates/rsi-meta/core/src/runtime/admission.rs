#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::service::LeaseGuard;

impl Runtime {
    /// Linearizes one operation against Runtime shutdown.
    pub(super) fn begin_admission(&self) -> Result<LeaseGuard> {
        let admission = self
            .inner
            .runtime_admission
            .acquire()
            .ok_or_else(|| self.closed_admission_error())?;
        if let Some(error) = self.admission_error() {
            return Err(error);
        }
        Ok(admission)
    }

    fn admission_error(&self) -> Option<MetaError> {
        let state = self.inner.state.lock().expect("runtime state poisoned");
        if let Some(reason) = state.terminal.clone() {
            return Some(MetaError::RuntimeTerminal(reason));
        }
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Some(MetaError::RuntimeShuttingDown);
        }
        None
    }

    fn closed_admission_error(&self) -> MetaError {
        self.admission_error()
            .unwrap_or(MetaError::RuntimeShuttingDown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn shutdown_drains_admission_that_precedes_every_resource_reservation() {
        let runtime = Runtime::new(RuntimeLimits {
            deadlines: DeadlineLimits {
                shutdown_wait: Duration::from_millis(10),
                ..DeadlineLimits::default()
            },
            ..RuntimeLimits::default()
        })
        .unwrap();
        let admission = runtime.begin_admission().unwrap();
        let before = runtime.resource_snapshot();
        assert_eq!(before.preparations.current, 0);
        assert_eq!(before.fibers.current, 0);
        assert_eq!(before.service_calls.current, 0);

        let first = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.shutdown().await }
        });
        while !runtime.snapshot().shutting_down {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            runtime.begin_admission(),
            Err(MetaError::RuntimeShuttingDown)
        ));
        tokio::time::advance(Duration::from_millis(11)).await;
        assert!(matches!(
            first.await.unwrap(),
            ShutdownOutcome::TimedOut { .. }
        ));

        drop(admission);
        assert!(runtime.shutdown().await.is_complete());
        let complete = runtime.resource_snapshot();
        assert_eq!(complete.preparations.current, 0);
        assert_eq!(complete.fibers.current, 0);
        assert_eq!(complete.service_calls.current, 0);
    }

    #[tokio::test]
    async fn cached_complete_rejects_admission() {
        let runtime = Runtime::default();
        assert!(runtime.shutdown().await.is_complete());
        let resources = runtime.resource_snapshot();
        let revision = runtime.snapshot().revision;

        assert!(matches!(
            runtime.begin_admission(),
            Err(MetaError::RuntimeShuttingDown)
        ));
        assert_eq!(runtime.resource_snapshot(), resources);
        assert_eq!(runtime.snapshot().revision, revision);
    }
}
