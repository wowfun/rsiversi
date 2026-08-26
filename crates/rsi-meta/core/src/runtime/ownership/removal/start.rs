use super::super::super::{Runtime, drop_catching_unwind};
use super::EventRemoval;

impl EventRemoval {
    pub(in super::super) fn claim_detached_report(&self) {
        let failure = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.detached_report = true;
            let failure = match &state.result {
                Some(Err(error)) if !state.report_retained => Some(error.clone()),
                _ => None,
            };
            state.report_retained |= failure.is_some();
            failure
        };
        if let Some(error) = failure {
            self.retain_detached_failure(&error);
        }
    }

    pub(in super::super) fn start(&self) -> bool {
        let won = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.started {
                false
            } else {
                state.started = true;
                true
            }
        };
        if !won {
            return false;
        }
        let runtime = self.runtime.upgrade().map(|inner| Runtime { inner });
        let result = runtime.as_ref().map_or(Ok(false), |runtime| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.remove_listener_entry(self.owner, self.id)
            }))
            .map_err(|payload| {
                if drop_catching_unwind(payload) {
                    "event listener removal and panic payload destruction panicked".to_owned()
                } else {
                    "event listener removal panicked".to_owned()
                }
            })
        });
        let detached_failure = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let failure = match &result {
                Err(error) if state.detached_report && !state.report_retained => {
                    Some(error.clone())
                }
                _ => None,
            };
            state.report_retained |= failure.is_some();
            state.result = Some(result.clone());
            failure
        };
        self.complete.notify_waiters();
        if let Some(error) = detached_failure {
            self.retain_detached_failure(&error);
        }
        if result.is_err()
            && let Some(runtime) = runtime
        {
            runtime.mark_terminal_owned("event listener removal panicked");
        }
        true
    }
}
