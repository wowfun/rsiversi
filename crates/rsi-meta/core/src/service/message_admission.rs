use super::message_scheduler::AdmissionState;
use super::message_waiter::{MessageChannel, WaitRegistration, Waiter};
use crate::runtime::ResourceLedger;
use crate::{MetaError, Result};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct BufferedMessageAdmission {
    byte_limit: usize,
    capability_limit: usize,
    pending_resources: Arc<ResourceLedger>,
    state: Mutex<AdmissionState>,
}

#[derive(Debug)]
pub(crate) struct BufferedMessagePermit {
    admission: Arc<BufferedMessageAdmission>,
    channel: Arc<MessageChannel>,
    bytes: usize,
    capabilities: usize,
}

impl BufferedMessageAdmission {
    pub(crate) fn new(
        byte_limit: usize,
        capability_limit: usize,
        pending_resources: Arc<ResourceLedger>,
    ) -> Self {
        Self {
            byte_limit,
            capability_limit,
            pending_resources,
            state: Mutex::new(AdmissionState::new(byte_limit, capability_limit)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        channel: &Arc<MessageChannel>,
        bytes: usize,
        capabilities: usize,
        byte_resources: &ResourceLedger,
        capability_resources: &ResourceLedger,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<BufferedMessagePermit> {
        debug_assert!(bytes <= self.byte_limit && capabilities <= self.capability_limit);
        if let Some(permit) = self.try_acquire(channel, bytes, capabilities) {
            return Ok(permit);
        }
        let mut registration = self.register(channel, bytes, capabilities)?;
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
                    registration.record_rejection(byte_resources, capability_resources);
                    cancellation.cancel();
                    return Err(MetaError::Timeout("service call"));
                }
                () = cancellation.cancelled() => {
                    registration.record_rejection(byte_resources, capability_resources);
                    return Err(MetaError::Cancelled);
                }
                () = notified.as_mut() => {}
            }
        }
    }

    fn try_acquire(
        self: &Arc<Self>,
        channel: &Arc<MessageChannel>,
        bytes: usize,
        capabilities: usize,
    ) -> Option<BufferedMessagePermit> {
        let mut state = self.state.lock().expect("message admission state poisoned");
        let mut channel_state = channel
            .state
            .lock()
            .expect("message channel state poisoned");
        if channel_state.available == 0 || !state.try_take_budget(bytes, capabilities) {
            return None;
        }
        channel_state.available -= 1;
        Some(BufferedMessagePermit::new(
            Arc::clone(self),
            Arc::clone(channel),
            bytes,
            capabilities,
        ))
    }

    fn register(
        self: &Arc<Self>,
        channel: &Arc<MessageChannel>,
        bytes: usize,
        capabilities: usize,
    ) -> Result<WaitRegistration> {
        let pending_resource =
            self.pending_resources
                .try_reserve(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "pending message sends",
                })?;
        let mut state = self.state.lock().expect("message admission state poisoned");
        let Some(id) = state.next_waiter_id() else {
            self.pending_resources.record_rejection();
            return Err(MetaError::CapacityExhausted {
                resource: "pending message send identities",
            });
        };
        let waiter = Arc::new(Waiter {
            id,
            bytes,
            capabilities,
            channel: Arc::clone(channel),
            bypasses: AtomicU8::new(0),
            granted: AtomicBool::new(false),
            ready: tokio::sync::Notify::new(),
        });
        let (blocked_bytes, blocked_capabilities) = state.blocked_resources(bytes, capabilities);
        let mut channel_state = channel
            .state
            .lock()
            .expect("message channel state poisoned");
        let exposed_fitting_candidate = state.register_waiter(&mut channel_state, &waiter);
        drop(channel_state);
        if exposed_fitting_candidate {
            state.schedule();
        }
        drop(state);
        Ok(WaitRegistration::new(
            Arc::clone(self),
            waiter,
            blocked_bytes,
            blocked_capabilities,
            pending_resource,
        ))
    }

    fn release(&self, channel: &Arc<MessageChannel>, bytes: usize, capabilities: usize) {
        let mut state = self.state.lock().expect("message admission state poisoned");
        state.return_capacity(channel, bytes, capabilities);
        debug_assert!(state.available_within(self.byte_limit, self.capability_limit));
        state.schedule();
    }

    pub(super) fn cancel(&self, waiter: &Arc<Waiter>) {
        let mut state = self.state.lock().expect("message admission state poisoned");
        if waiter.granted.load(Ordering::Acquire) {
            state.return_capacity(&waiter.channel, waiter.bytes, waiter.capabilities);
            state.schedule();
        } else {
            let scheduling_changed = state.cancel_waiter(waiter);
            if scheduling_changed {
                state.schedule();
            }
        }
    }
}

impl BufferedMessagePermit {
    pub(super) fn new(
        admission: Arc<BufferedMessageAdmission>,
        channel: Arc<MessageChannel>,
        bytes: usize,
        capabilities: usize,
    ) -> Self {
        Self {
            admission,
            channel,
            bytes,
            capabilities,
        }
    }
}

impl Drop for BufferedMessagePermit {
    fn drop(&mut self) {
        self.admission
            .release(&self.channel, self.bytes, self.capabilities);
    }
}
