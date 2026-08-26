use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub(super) struct ExchangeGate {
    state: AtomicUsize,
}

impl ExchangeGate {
    const CLOSED: usize = 1 << (usize::BITS - 1);
    const ACTIVE: usize = Self::CLOSED - 1;

    pub(super) fn enter(&self) -> Option<ExchangeAdmission<'_>> {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if current & Self::CLOSED != 0 || current & Self::ACTIVE == Self::ACTIVE {
                return None;
            }
            match self.state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ExchangeAdmission {
                        gate: self,
                        active: true,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(super) fn close_if_sole(&self) -> Option<FinalizeFence<'_>> {
        self.state
            .compare_exchange(1, Self::CLOSED | 1, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| FinalizeFence {
                gate: self,
                reopen: true,
            })
    }

    fn reopen(&self) {
        self.state
            .compare_exchange(Self::CLOSED | 1, 1, Ordering::AcqRel, Ordering::Acquire)
            .expect("finalize fence exclusively owns the closed state");
    }

    fn leave(&self) {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert_ne!(previous & Self::ACTIVE, 0);
    }
}

pub(super) struct ExchangeAdmission<'a> {
    gate: &'a ExchangeGate,
    active: bool,
}

impl ExchangeAdmission<'_> {
    pub(super) fn finish_final(mut self) {
        debug_assert_eq!(
            self.gate.state.load(Ordering::Acquire),
            ExchangeGate::CLOSED | 1
        );
        self.gate.leave();
        self.active = false;
    }
}

impl Drop for ExchangeAdmission<'_> {
    fn drop(&mut self) {
        if self.active {
            self.gate.leave();
        }
    }
}

pub(super) struct FinalizeFence<'a> {
    gate: &'a ExchangeGate,
    reopen: bool,
}

impl FinalizeFence<'_> {
    pub(super) fn commit(mut self) {
        self.reopen = false;
    }
}

impl Drop for FinalizeFence<'_> {
    fn drop(&mut self) {
        if self.reopen {
            self.gate.reopen();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn existing_exchange_blocks_close_and_failed_close_can_retry() {
        let gate = ExchangeGate::default();
        let finalizer = gate.enter().expect("finalizer admission");
        let existing = gate.enter().expect("existing exchange admission");
        assert!(gate.close_if_sole().is_none());
        drop(existing);
        let retry = gate.close_if_sole().expect("retry closes as sole exchange");
        drop(retry);
        drop(finalizer);
        assert!(
            gate.enter().is_some(),
            "failed finalization reopened admission"
        );
    }

    #[test]
    fn closed_gate_rejects_a_concurrent_begin_before_control_block_drop() {
        let gate = Arc::new(ExchangeGate::default());
        let finalizer = gate.enter().expect("finalizer admission");
        let fence = gate.close_if_sole().expect("sole finalizer closes gate");
        let start = Arc::new(Barrier::new(2));
        let worker_gate = Arc::clone(&gate);
        let worker_start = Arc::clone(&start);
        let worker = std::thread::spawn(move || {
            worker_start.wait();
            worker_gate.enter().is_none()
        });
        start.wait();
        assert!(worker.join().expect("closed-gate worker joins"));
        fence.commit();
        finalizer.finish_final();
    }
}
