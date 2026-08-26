use super::message_admission::{BufferedMessageAdmission, BufferedMessagePermit};
use crate::runtime::{ResourceLedger, ResourceReservation};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct WaiterId(pub(super) u64);

#[derive(Debug)]
pub(crate) struct MessageChannel {
    pub(super) state: Mutex<ChannelState>,
}

#[derive(Debug)]
pub(super) struct ChannelState {
    pub(super) capacity: usize,
    pub(super) available: usize,
    pub(super) pending: BTreeMap<WaiterId, Arc<Waiter>>,
    pub(super) ready: BTreeSet<WaiterId>,
}

impl MessageChannel {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ChannelState {
                capacity,
                available: capacity,
                pending: BTreeMap::new(),
                ready: BTreeSet::new(),
            }),
        })
    }
}

#[derive(Debug)]
pub(super) struct Waiter {
    pub(super) id: WaiterId,
    pub(super) bytes: usize,
    pub(super) capabilities: usize,
    pub(super) channel: Arc<MessageChannel>,
    pub(super) bypasses: AtomicU8,
    pub(super) granted: AtomicBool,
    pub(super) ready: Notify,
}

pub(super) struct WaitRegistration {
    admission: Arc<BufferedMessageAdmission>,
    pub(super) waiter: Arc<Waiter>,
    blocked_bytes: bool,
    blocked_capabilities: bool,
    pending_resource: Option<ResourceReservation>,
    claimed: bool,
}

impl WaitRegistration {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        admission: Arc<BufferedMessageAdmission>,
        waiter: Arc<Waiter>,
        blocked_bytes: bool,
        blocked_capabilities: bool,
        pending_resource: ResourceReservation,
    ) -> Self {
        Self {
            admission,
            waiter,
            blocked_bytes,
            blocked_capabilities,
            pending_resource: Some(pending_resource),
            claimed: false,
        }
    }

    pub(super) fn record_rejection(
        &self,
        byte_resources: &ResourceLedger,
        capability_resources: &ResourceLedger,
    ) {
        if self.blocked_bytes {
            byte_resources.record_rejection();
        }
        if self.blocked_capabilities {
            capability_resources.record_rejection();
        }
    }

    pub(super) fn claim(&mut self) -> BufferedMessagePermit {
        self.claimed = true;
        self.pending_resource.take();
        BufferedMessagePermit::new(
            Arc::clone(&self.admission),
            Arc::clone(&self.waiter.channel),
            self.waiter.bytes,
            self.waiter.capabilities,
        )
    }
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        if !self.claimed {
            self.admission.cancel(&self.waiter);
        }
    }
}
