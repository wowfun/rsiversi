use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
pub(super) struct CallbackGate {
    active: AtomicUsize,
}

impl CallbackGate {
    pub(super) fn enter(&self) -> Result<CallbackAdmission<'_>, u32> {
        let mut current = self.active.load(Ordering::Acquire);
        loop {
            if current == usize::MAX {
                return Err(crate::STATUS_LIMIT_EXCEEDED);
            }
            match self.active.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(CallbackAdmission { gate: self }),
                Err(observed) => current = observed,
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.active.load(Ordering::Acquire) == 0
    }
}

pub(super) struct CallbackAdmission<'a> {
    gate: &'a CallbackGate,
}

impl Drop for CallbackAdmission<'_> {
    fn drop(&mut self) {
        let previous = self.gate.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert_ne!(previous, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_count_never_wraps_and_reuses_capacity_after_drop() {
        let gate = CallbackGate {
            active: AtomicUsize::new(usize::MAX - 1),
        };
        let last = gate.enter().expect("last callback admission");
        assert_eq!(gate.active.load(Ordering::Acquire), usize::MAX);
        assert_eq!(gate.enter().err(), Some(crate::STATUS_LIMIT_EXCEEDED));
        assert_eq!(gate.enter().err(), Some(crate::STATUS_LIMIT_EXCEEDED));
        assert_eq!(gate.active.load(Ordering::Acquire), usize::MAX);
        drop(last);
        let reused = gate.enter().expect("dropped callback frees capacity");
        assert_eq!(gate.active.load(Ordering::Acquire), usize::MAX);
        drop(reused);
        assert_eq!(gate.active.load(Ordering::Acquire), usize::MAX - 1);
    }
}
