use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
pub(crate) struct AdmissionLease {
    state: AtomicUsize,
    drained: Notify,
    closed: CancellationToken,
}

impl AdmissionLease {
    const CLOSED: usize = 1 << (usize::BITS - 1);
    const ACTIVE: usize = Self::CLOSED - 1;

    pub(crate) fn acquire(self: &Arc<Self>) -> Option<LeaseGuard> {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if current & Self::CLOSED != 0 {
                return None;
            }
            if current & Self::ACTIVE == Self::ACTIVE {
                return None;
            }
            match self.state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        Some(LeaseGuard {
            lease: Arc::clone(self),
        })
    }

    pub(crate) fn close(&self) {
        let previous = self.state.fetch_or(Self::CLOSED, Ordering::AcqRel);
        self.retire();
        if previous & Self::ACTIVE == 0 {
            self.drained.notify_waiters();
        }
    }

    pub(crate) fn retire(&self) {
        self.closed.cancel();
    }

    pub(crate) async fn wait_drained(&self) {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.load(Ordering::Acquire) & Self::ACTIVE == 0 {
                return;
            }
            notified.as_mut().await;
        }
    }

    pub(crate) fn retirement_observer(&self) -> super::CancellationObserver {
        super::CancellationObserver::new(self.closed.clone())
    }

    fn release(&self) {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert_ne!(previous & Self::ACTIVE, 0);
        if previous & Self::ACTIVE == 1 {
            self.drained.notify_waiters();
        }
    }
}

#[derive(Debug)]
pub(crate) struct LeaseGuard {
    lease: Arc<AdmissionLease>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.lease.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retirement_notifies_before_closure_fences_all_new_acquisitions() {
        let lease = Arc::new(AdmissionLease::default());
        let observer = lease.retirement_observer();
        lease.retire();
        assert!(observer.is_cancelled());
        let admitted_before_seal = lease
            .acquire()
            .expect("existing holders can call until closure");
        lease.close();

        assert!(lease.acquire().is_none());
        drop(admitted_before_seal);
    }
}
