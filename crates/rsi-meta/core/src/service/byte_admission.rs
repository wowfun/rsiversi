use crate::runtime::ResourceLedger;
use crate::{MetaError, Result};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const MAXIMUM_BYPASSES: u8 = 64;

#[derive(Debug)]
pub(crate) struct BufferedByteAdmission {
    limit: usize,
    state: Mutex<AdmissionState>,
}

#[derive(Debug)]
struct AdmissionState {
    available: usize,
    waiters: VecDeque<QueuedWaiter>,
}

#[derive(Debug)]
struct QueuedWaiter {
    waiter: Arc<Waiter>,
    bypasses: u8,
}

#[derive(Debug)]
struct Waiter {
    bytes: usize,
    granted: AtomicBool,
    ready: Notify,
}

#[derive(Debug)]
pub(crate) struct BufferedBytePermit {
    admission: Arc<BufferedByteAdmission>,
    bytes: usize,
}

struct WaitRegistration {
    admission: Arc<BufferedByteAdmission>,
    waiter: Arc<Waiter>,
    claimed: bool,
}

impl BufferedByteAdmission {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            state: Mutex::new(AdmissionState {
                available: limit,
                waiters: VecDeque::new(),
            }),
        }
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        bytes: usize,
        resources: &ResourceLedger,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<BufferedBytePermit> {
        debug_assert!(bytes != 0 && bytes <= self.limit);
        if let Some(permit) = self.try_acquire(bytes) {
            return Ok(permit);
        }

        let mut registration = self.register(bytes);
        let waiter = Arc::clone(&registration.waiter);
        loop {
            let notified = waiter.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if waiter.granted.load(Ordering::Acquire) {
                return Ok(registration.claim());
            }
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => {
                    resources.record_rejection();
                    cancellation.cancel();
                    return Err(MetaError::Timeout("service call"));
                }
                () = cancellation.cancelled() => {
                    resources.record_rejection();
                    return Err(MetaError::Cancelled);
                }
                () = notified.as_mut() => {}
            }
        }
    }

    fn try_acquire(self: &Arc<Self>, bytes: usize) -> Option<BufferedBytePermit> {
        let mut state = self.state.lock().expect("byte admission state poisoned");
        if !state.waiters.is_empty() || state.available < bytes {
            return None;
        }
        state.available -= bytes;
        Some(BufferedBytePermit {
            admission: Arc::clone(self),
            bytes,
        })
    }

    fn register(self: &Arc<Self>, bytes: usize) -> WaitRegistration {
        let waiter = Arc::new(Waiter {
            bytes,
            granted: AtomicBool::new(false),
            ready: Notify::new(),
        });
        let mut state = self.state.lock().expect("byte admission state poisoned");
        state.waiters.push_back(QueuedWaiter {
            waiter: Arc::clone(&waiter),
            bypasses: 0,
        });
        Self::schedule(&mut state);
        drop(state);
        WaitRegistration {
            admission: Arc::clone(self),
            waiter,
            claimed: false,
        }
    }

    fn release(&self, bytes: usize) {
        let mut state = self.state.lock().expect("byte admission state poisoned");
        state.available = state
            .available
            .checked_add(bytes)
            .expect("byte admission release cannot overflow");
        debug_assert!(state.available <= self.limit);
        Self::schedule(&mut state);
    }

    fn cancel(&self, waiter: &Arc<Waiter>) {
        let mut state = self.state.lock().expect("byte admission state poisoned");
        if waiter.granted.load(Ordering::Acquire) {
            state.available = state
                .available
                .checked_add(waiter.bytes)
                .expect("cancelled byte grant cannot overflow");
            debug_assert!(state.available <= self.limit);
        } else if let Some(index) = state
            .waiters
            .iter()
            .position(|queued| Arc::ptr_eq(&queued.waiter, waiter))
        {
            state.waiters.remove(index);
        }
        Self::schedule(&mut state);
    }

    fn schedule(state: &mut AdmissionState) {
        loop {
            let mut selected = None;
            for (index, queued) in state.waiters.iter().enumerate() {
                if queued.waiter.bytes <= state.available {
                    selected = Some(index);
                    break;
                }
                if queued.bypasses >= MAXIMUM_BYPASSES {
                    break;
                }
            }
            let Some(selected) = selected else {
                return;
            };
            for queued in state.waiters.iter_mut().take(selected) {
                queued.bypasses = queued.bypasses.saturating_add(1);
            }
            let queued = state
                .waiters
                .remove(selected)
                .expect("selected byte waiter exists");
            state.available -= queued.waiter.bytes;
            queued.waiter.granted.store(true, Ordering::Release);
            queued.waiter.ready.notify_one();
        }
    }
}

impl WaitRegistration {
    fn claim(&mut self) -> BufferedBytePermit {
        self.claimed = true;
        BufferedBytePermit {
            admission: Arc::clone(&self.admission),
            bytes: self.waiter.bytes,
        }
    }
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        if !self.claimed {
            self.admission.cancel(&self.waiter);
        }
    }
}

impl Drop for BufferedBytePermit {
    fn drop(&mut self) {
        self.admission.release(self.bytes);
    }
}
