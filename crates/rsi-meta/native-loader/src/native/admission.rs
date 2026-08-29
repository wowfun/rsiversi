use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

const CLOSED: usize = 1 << (usize::BITS - 1);
const ACTIVE_MASK: usize = CLOSED - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionError {
    Closed,
    Saturated,
    Finalized,
}

/// One linearizable close/admission word in front of every plugin-table access.
pub(super) struct AdmissionGate {
    state: AtomicUsize,
    wait_lock: Mutex<()>,
    drained: Condvar,
    finalizer: Mutex<FinalizerState>,
}

#[derive(Default)]
struct FinalizerState {
    finalized: bool,
}

impl AdmissionGate {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            drained: Condvar::new(),
            finalizer: Mutex::new(FinalizerState { finalized: false }),
        }
    }

    pub(super) fn try_enter(self: &Arc<Self>) -> Result<Admission, AdmissionError> {
        let result = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if state & CLOSED != 0 || state & ACTIVE_MASK == ACTIVE_MASK {
                    None
                } else {
                    Some(state + 1)
                }
            });
        match result {
            Ok(_) => Ok(Admission {
                gate: Arc::clone(self),
            }),
            Err(state) if state & CLOSED != 0 => Err(AdmissionError::Closed),
            Err(_) => Err(AdmissionError::Saturated),
        }
    }

    fn close_and_wait(&self) {
        let previous = self.state.fetch_or(CLOSED, Ordering::AcqRel);
        if previous & ACTIVE_MASK == 0 {
            return;
        }
        let mut wait = self
            .wait_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while self.state.load(Ordering::Acquire) & ACTIVE_MASK != 0 {
            wait = self
                .drained
                .wait(wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn reopen(&self) -> bool {
        self.state
            .compare_exchange(CLOSED, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Claims the sole finalizer lane and drains every ordinary admission.
    ///
    /// Dropping the returned claim reopens ordinary admission unless
    /// [`ExclusiveAdmission::finish`] permanently finalized the transport.
    pub(super) fn begin_exclusive(&self) -> Result<ExclusiveAdmission<'_>, AdmissionError> {
        let state = self
            .finalizer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finalized {
            return Err(AdmissionError::Finalized);
        }
        self.close_and_wait();
        Ok(ExclusiveAdmission {
            gate: self,
            state,
            finished: false,
        })
    }

    pub(super) fn is_finalized(&self) -> bool {
        self.finalizer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finalized
    }

    #[cfg(test)]
    fn state_for_test(&self) -> usize {
        self.state.load(Ordering::Acquire)
    }
}

pub(super) struct ExclusiveAdmission<'gate> {
    gate: &'gate AdmissionGate,
    state: MutexGuard<'gate, FinalizerState>,
    finished: bool,
}

impl ExclusiveAdmission<'_> {
    /// Permanently closes the gate after a successful foreign finalization.
    pub(super) fn finish(mut self) {
        self.state.finalized = true;
        self.finished = true;
    }
}

impl Drop for ExclusiveAdmission<'_> {
    fn drop(&mut self) {
        if !self.finished {
            assert!(
                self.gate.reopen(),
                "exclusive admission must own the drained closed state"
            );
        }
    }
}

pub(super) struct Admission {
    gate: Arc<AdmissionGate>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        let previous = self.gate.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert_ne!(previous & ACTIVE_MASK, 0);
        if previous == CLOSED | 1 {
            let _wait = self
                .gate
                .wait_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.gate.drained.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn close_waits_for_existing_access_and_rejects_every_later_beginning() {
        let gate = Arc::new(AdmissionGate::new());
        let access = gate.try_enter().unwrap();
        let closer_gate = Arc::clone(&gate);
        let (closed_tx, closed_rx) = std::sync::mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            closed_tx.send(()).unwrap();
            closer_gate.close_and_wait();
        });
        closed_rx.recv().unwrap();
        while gate.state_for_test() & CLOSED == 0 {
            std::thread::yield_now();
        }
        assert_eq!(gate.try_enter().err(), Some(AdmissionError::Closed));
        assert!(!closer.is_finished());
        drop(access);
        closer.join().unwrap();
        assert_eq!(gate.state_for_test(), CLOSED);
    }

    #[test]
    fn failed_exclusive_operation_reopens_only_after_the_gate_drained() {
        let gate = Arc::new(AdmissionGate::new());
        drop(gate.begin_exclusive().unwrap());
        assert!(gate.try_enter().is_ok());
    }

    #[test]
    fn successful_exclusive_operation_permanently_rejects_every_later_owner() {
        let gate = Arc::new(AdmissionGate::new());
        gate.begin_exclusive().unwrap().finish();
        assert_eq!(gate.try_enter().err(), Some(AdmissionError::Closed));
        assert_eq!(
            gate.begin_exclusive().err(),
            Some(AdmissionError::Finalized)
        );
    }

    #[test]
    fn concurrent_exclusive_owners_are_serialized() {
        let gate = Arc::new(AdmissionGate::new());
        let first = gate.begin_exclusive().unwrap();
        let second_gate = Arc::clone(&gate);
        let second = std::thread::spawn(move || {
            let _claim = second_gate.begin_exclusive().unwrap();
        });
        assert!(!second.is_finished());
        drop(first);
        second.join().unwrap();
    }
}
