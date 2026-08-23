use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Debug, Default)]
pub(crate) struct AdmissionLease {
    state: AtomicUsize,
    drained: Notify,
}

impl AdmissionLease {
    const CLOSED: usize = 1 << (usize::BITS - 1);
    const ACTIVE: usize = Self::CLOSED - 1;

    pub(crate) fn acquire(self: &Arc<Self>, retiring_consumer: bool) -> Option<LeaseGuard> {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if !retiring_consumer && current & Self::CLOSED != 0 {
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
        if previous & Self::ACTIVE == 0 {
            self.drained.notify_waiters();
        }
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
