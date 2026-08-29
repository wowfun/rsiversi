use crate::LoaderError;
use crate::worker::CallbackCompletion;
use rsi_meta::MetaError;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub(super) fn callback_deadline(timeout: Duration) -> rsi_meta::Result<tokio::time::Instant> {
    tokio::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| MetaError::InvalidInput("native callback deadline overflow".to_owned()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Owner {
    Factory,
    Instance(u64),
}

#[derive(Default)]
struct GateState {
    owner: Option<Owner>,
    poisoned: bool,
}

/// One fail-fast admission state for idle, busy, and poisoned callbacks.
pub(super) struct CallbackGate {
    state: Mutex<GateState>,
    idle: Condvar,
}

impl CallbackGate {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            idle: Condvar::new(),
        }
    }

    pub(super) fn acquire_factory(
        self: &Arc<Self>,
        operation: &'static str,
    ) -> Result<CallbackAdmission, LoaderError> {
        self.acquire(Owner::Factory, operation, || {})
    }

    pub(super) fn acquire_instance(
        self: &Arc<Self>,
        lineage: u64,
    ) -> Result<CallbackAdmission, LoaderError> {
        self.acquire(Owner::Instance(lineage), "instance callback", || {})
    }

    fn acquire(
        self: &Arc<Self>,
        owner: Owner,
        operation: &'static str,
        after_state_read: impl FnOnce(),
    ) -> Result<CallbackAdmission, LoaderError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        after_state_read();
        if state.poisoned {
            return Err(LoaderError::Callback {
                operation,
                message: match owner {
                    Owner::Factory => {
                        "native factory was poisoned by a timed-out callback".to_owned()
                    }
                    Owner::Instance(_) => {
                        "native instance was poisoned by a timed-out callback".to_owned()
                    }
                },
            });
        }
        match state.owner {
            Some(Owner::Instance(current)) if matches!(owner, Owner::Instance(lineage) if lineage == current) => {
                Err(LoaderError::Reentrant { operation })
            }
            Some(_) => Err(LoaderError::Busy { operation }),
            None => {
                state.owner = Some(owner);
                Ok(CallbackAdmission {
                    gate: Arc::clone(self),
                    owner,
                    poison_on_drop: false,
                })
            }
        }
    }

    /// Fences later admission without releasing the current owner.
    pub(super) fn poison(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .poisoned = true;
    }

    pub(super) fn wait_idle(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.owner.is_some() {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    fn acquire_factory_with_hook(
        self: &Arc<Self>,
        operation: &'static str,
        after_state_read: impl FnOnce(),
    ) -> Result<CallbackAdmission, LoaderError> {
        self.acquire(Owner::Factory, operation, after_state_read)
    }

    #[cfg(test)]
    fn acquire_instance_with_hook(
        self: &Arc<Self>,
        lineage: u64,
        after_state_read: impl FnOnce(),
    ) -> Result<CallbackAdmission, LoaderError> {
        self.acquire(
            Owner::Instance(lineage),
            "instance callback",
            after_state_read,
        )
    }
}

/// Owns one admitted callback. Once armed on its worker thread, unwind or
/// disconnect poisons the gate. A successful completion decision disarms that
/// behavior, but the guard stays busy until result handoff finishes.
pub(super) struct CallbackAdmission {
    gate: Arc<CallbackGate>,
    owner: Owner,
    poison_on_drop: bool,
}

impl CallbackAdmission {
    pub(super) fn arm(&mut self) {
        self.poison_on_drop = true;
    }

    fn completed(&mut self) {
        self.poison_on_drop = false;
    }
}

impl Drop for CallbackAdmission {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert_eq!(state.owner, Some(self.owner));
        if self.poison_on_drop {
            state.poisoned = true;
        }
        state.owner = None;
        self.gate.idle.notify_all();
    }
}

/// Keeps admission busy across result delivery. Rejected results are destroyed
/// before the gate reopens, including a sender whose receiver disappeared.
pub(super) struct CallbackHandoff<T> {
    value: Option<T>,
    admission: Option<CallbackAdmission>,
    completion: Arc<CallbackCompletion>,
}

impl<T> CallbackHandoff<T> {
    pub(super) fn new(
        value: T,
        admission: CallbackAdmission,
        completion: Arc<CallbackCompletion>,
    ) -> Self {
        Self::new_inner(value, admission, completion, || {})
    }

    fn new_inner(
        value: T,
        admission: CallbackAdmission,
        completion: Arc<CallbackCompletion>,
        before_handoff: impl FnOnce(),
    ) -> Self {
        before_handoff();
        Self {
            value: Some(value),
            admission: Some(admission),
            completion,
        }
    }

    pub(super) fn into_inner(mut self) -> Result<T, CallbackHandoffError> {
        if !self.completion.complete() {
            debug_assert!(self.completion.is_timed_out());
            return Err(CallbackHandoffError::TimedOut);
        }
        self.admission
            .as_mut()
            .expect("callback handoff owns admission")
            .completed();
        let value = self.value.take().expect("callback handoff owns its value");
        drop(self.admission.take());
        Ok(value)
    }

    #[cfg(test)]
    fn new_with_before_completion(
        value: T,
        admission: CallbackAdmission,
        completion: Arc<CallbackCompletion>,
        before_handoff: impl FnOnce(),
    ) -> Self {
        Self::new_inner(value, admission, completion, before_handoff)
    }
}

impl<T> Drop for CallbackHandoff<T> {
    fn drop(&mut self) {
        drop(self.value.take());
        if self.completion.complete() {
            self.admission
                .as_mut()
                .expect("callback handoff owns admission")
                .completed();
        }
        drop(self.admission.take());
    }
}

pub(super) enum CallbackHandoffError {
    TimedOut,
}

pub(super) enum BlockingCallbackWaitError {
    TimedOut,
    Disconnected,
}

pub(super) fn run_bounded_blocking_callback<T>(
    receiver: &Receiver<CallbackHandoff<T>>,
    completion: &CallbackCompletion,
    timeout: Duration,
    on_timeout: &(dyn Fn() + Send + Sync),
) -> Result<T, BlockingCallbackWaitError> {
    match receiver.recv_timeout(timeout) {
        Ok(handoff) if completion.is_timed_out() => {
            drop(handoff);
            Err(BlockingCallbackWaitError::TimedOut)
        }
        Ok(handoff) => handoff
            .into_inner()
            .map_err(|CallbackHandoffError::TimedOut| BlockingCallbackWaitError::TimedOut),
        Err(RecvTimeoutError::Timeout) => {
            if completion.time_out(on_timeout) {
                Err(BlockingCallbackWaitError::TimedOut)
            } else {
                receiver
                    .recv()
                    .map_err(|_| BlockingCallbackWaitError::Disconnected)?
                    .into_inner()
                    .map_err(|CallbackHandoffError::TimedOut| BlockingCallbackWaitError::TimedOut)
            }
        }
        Err(RecvTimeoutError::Disconnected) => {
            if completion.complete() {
                Err(BlockingCallbackWaitError::Disconnected)
            } else {
                Err(BlockingCallbackWaitError::TimedOut)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn factory_poison_and_busy_share_one_admission_linearization() {
        let gate = Arc::new(CallbackGate::new());
        let active = gate.acquire_factory("prepare").unwrap();
        let checked = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let contender = {
            let gate = Arc::clone(&gate);
            let checked = Arc::clone(&checked);
            let resume = Arc::clone(&resume);
            std::thread::spawn(move || {
                gate.acquire_factory_with_hook("prepare", || {
                    checked.wait();
                    resume.wait();
                })
            })
        };

        checked.wait();
        let transition = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.poison();
                drop(active);
            })
        };
        resume.wait();

        assert!(matches!(
            contender.join().unwrap(),
            Err(LoaderError::Busy {
                operation: "prepare"
            })
        ));
        transition.join().unwrap();
        assert!(matches!(
            gate.acquire_factory("prepare"),
            Err(LoaderError::Callback { .. })
        ));
    }

    #[test]
    fn instance_poison_and_busy_share_one_admission_linearization() {
        let gate = Arc::new(CallbackGate::new());
        let active = gate.acquire_instance(41).unwrap();
        let checked = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let contender = {
            let gate = Arc::clone(&gate);
            let checked = Arc::clone(&checked);
            let resume = Arc::clone(&resume);
            std::thread::spawn(move || {
                gate.acquire_instance_with_hook(73, || {
                    checked.wait();
                    resume.wait();
                })
            })
        };

        checked.wait();
        let transition = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.poison();
                drop(active);
            })
        };
        resume.wait();

        assert!(matches!(
            contender.join().unwrap(),
            Err(LoaderError::Busy {
                operation: "instance callback"
            })
        ));
        transition.join().unwrap();
        assert!(matches!(
            gate.acquire_instance(73),
            Err(LoaderError::Callback { .. })
        ));
    }

    #[test]
    fn callback_return_holds_admission_until_timeout_publication() {
        let gate = Arc::new(CallbackGate::new());
        let mut admission = gate.acquire_factory("prepare").unwrap();
        let completion = Arc::new(CallbackCompletion::new());
        let worker_completion = Arc::clone(&completion);
        let callback_returned = Arc::new(Barrier::new(2));
        let publish_completion = Arc::new(Barrier::new(2));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = {
            let callback_returned = Arc::clone(&callback_returned);
            let publish_completion = Arc::clone(&publish_completion);
            std::thread::spawn(move || {
                admission.arm();
                let handoff = CallbackHandoff::new_with_before_completion(
                    7_u8,
                    admission,
                    worker_completion,
                    || {
                        callback_returned.wait();
                        publish_completion.wait();
                    },
                );
                let _ = sender.send(handoff);
            })
        };

        callback_returned.wait();
        let timeout_gate = Arc::clone(&gate);
        let result = run_bounded_blocking_callback(
            &receiver,
            &completion,
            Duration::from_millis(1),
            &move || timeout_gate.poison(),
        );
        assert!(matches!(result, Err(BlockingCallbackWaitError::TimedOut)));
        assert!(matches!(
            gate.acquire_factory("prepare"),
            Err(LoaderError::Callback { .. })
        ));

        publish_completion.wait();
        worker.join().unwrap();
    }

    #[test]
    fn successful_result_keeps_admission_until_receiver_claims_handoff() {
        let gate = Arc::new(CallbackGate::new());
        let mut admission = gate.acquire_factory("prepare").unwrap();
        admission.arm();
        let completion = Arc::new(CallbackCompletion::new());
        let handoff = CallbackHandoff::new(7_u8, admission, completion);

        assert!(matches!(
            gate.acquire_factory("prepare"),
            Err(LoaderError::Busy {
                operation: "prepare"
            })
        ));
        let Ok(value) = handoff.into_inner() else {
            panic!("receiver must win completion before the deadline");
        };
        assert_eq!(value, 7);
        drop(
            gate.acquire_factory("prepare")
                .expect("completed handoff must reopen admission"),
        );
    }

    #[test]
    fn instance_gate_rejects_same_lineage_as_reentrant() {
        let gate = Arc::new(CallbackGate::new());
        let _active = gate.acquire_instance(41).unwrap();
        assert!(matches!(
            gate.acquire_instance(41),
            Err(LoaderError::Reentrant {
                operation: "instance callback"
            })
        ));
    }

    #[test]
    fn instance_gate_rejects_other_lineage_as_busy() {
        let gate = Arc::new(CallbackGate::new());
        let _active = gate.acquire_instance(41).unwrap();
        assert!(matches!(
            gate.acquire_instance(73),
            Err(LoaderError::Busy {
                operation: "instance callback"
            })
        ));
    }
}
