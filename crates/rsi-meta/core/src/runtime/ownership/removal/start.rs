use super::super::super::{Runtime, drop_catching_unwind};
use super::{RegistrationRemoval, RemovalAction};
use std::sync::atomic::Ordering;

impl RegistrationRemoval {
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

    pub(in crate::runtime) fn start(&self) -> bool {
        let won = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.started {
                false
            } else {
                state.started = true;
                self.admitting.store(false, Ordering::Release);
                true
            }
        };
        if !won {
            return false;
        }
        let runtime = self.runtime.upgrade().map(|inner| Runtime { inner });
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &self.action {
                RemovalAction::Listener(id) => Ok(runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.remove_local_listener_entry(self.owner, *id))),
                RemovalAction::Local(undo) => {
                    let undo = undo
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    undo.map_or(Ok(false), |undo| undo().map(|()| true))
                }
            }))
            .map_err(|payload| {
                if drop_catching_unwind(payload) {
                    "Local registration removal and panic payload destruction panicked".to_owned()
                } else {
                    "Local registration removal panicked".to_owned()
                }
            })
            .and_then(std::convert::identity)
            .map_err(|error| {
                crate::runtime::diagnostics::bound_owned(error, self.maximum_diagnostic_bytes)
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
            runtime.mark_terminal_owned("Local registration removal failed");
        }
        true
    }
}
