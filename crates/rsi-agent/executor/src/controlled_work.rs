use rsi_agent_turn_protocol::{ControlledWork, ControlledWorkReporter};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct State {
    guards: usize,
    failed: bool,
    finalized: bool,
    reporter: Option<ControlledWorkReporter>,
}

#[derive(Debug)]
pub(super) struct Tracker(Mutex<State>);

impl Tracker {
    pub(super) fn new() -> (Arc<Self>, ControlledWork) {
        let (observation, reporter) = ControlledWork::new();
        (
            Arc::new(Self(Mutex::new(State {
                guards: 0,
                failed: false,
                finalized: false,
                reporter: Some(reporter),
            }))),
            observation,
        )
    }

    pub(super) fn guard(self: &Arc<Self>) -> Guard {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .guards += 1;
        Guard {
            tracker: Arc::clone(self),
            confirmed: false,
        }
    }

    pub(super) fn finalized(&self, success: bool) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.finalized = true;
        state.failed |= !success;
    }
}

#[derive(Debug)]
pub(super) struct Guard {
    tracker: Arc<Tracker>,
    confirmed: bool,
}
impl Guard {
    pub(super) fn confirm(mut self) {
        self.confirmed = true;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut state = self
            .tracker
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failed |= !self.confirmed;
        state.guards -= 1;
        if state.guards == 0
            && let Some(reporter) = state.reporter.take()
        {
            reporter.finish(state.finalized && !state.failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_turn_protocol::ControlledWorkStatus::{Running, Settled, Unsettled};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn a_terminal_driver_does_not_prove_retained_work_settled() {
        let (tracker, view) = Tracker::new();
        let driver = tracker.guard();
        let retained = tracker.guard();
        tracker.finalized(true);
        driver.confirm();
        assert_eq!(view.status(), Running);
        let waiter = tokio::spawn({
            let view = view.clone();
            async move { view.wait(CancellationToken::new()).await }
        });
        retained.confirm();
        assert_eq!(waiter.await.unwrap(), Settled);
    }

    #[tokio::test]
    async fn lost_cleanup_or_unconfirmed_finalization_is_never_settled() {
        for finalize in [None, Some(false), Some(true)] {
            let (tracker, view) = Tracker::new();
            let driver = tracker.guard();
            let retained = tracker.guard();
            if let Some(success) = finalize {
                tracker.finalized(success);
            }
            driver.confirm();
            drop(retained);
            assert_eq!(view.wait(CancellationToken::new()).await, Unsettled);
        }
        let (tracker, view) = Tracker::new();
        let driver = tracker.guard();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(view.wait(cancellation).await, Running);
        drop(driver);
        assert_eq!(view.status(), Unsettled);
    }
}
