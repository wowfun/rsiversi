use super::message_waiter::{ChannelState, MessageChannel, Waiter, WaiterId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

const MAXIMUM_BYPASSES: u8 = 64;
const READY_WINDOW: usize = MAXIMUM_BYPASSES as usize + 1;

#[derive(Debug)]
pub(super) struct AdmissionState {
    available_bytes: usize,
    available_capabilities: usize,
    next_waiter: u64,
    ready: BTreeMap<WaiterId, Arc<Waiter>>,
    #[cfg(test)]
    candidate_visits: usize,
}

impl AdmissionState {
    pub(super) fn new(byte_limit: usize, capability_limit: usize) -> Self {
        Self {
            available_bytes: byte_limit,
            available_capabilities: capability_limit,
            next_waiter: 0,
            ready: BTreeMap::new(),
            #[cfg(test)]
            candidate_visits: 0,
        }
    }

    pub(super) fn next_waiter_id(&mut self) -> Option<WaiterId> {
        let next = self.next_waiter.checked_add(1)?;
        self.next_waiter = next;
        Some(WaiterId(next))
    }

    pub(super) fn blocked_resources(&self, bytes: usize, capabilities: usize) -> (bool, bool) {
        (
            bytes != 0 && self.available_bytes < bytes,
            capabilities != 0 && self.available_capabilities < capabilities,
        )
    }

    pub(super) fn try_take_budget(&mut self, bytes: usize, capabilities: usize) -> bool {
        if !self.ready.is_empty()
            || self.available_bytes < bytes
            || self.available_capabilities < capabilities
        {
            return false;
        }
        self.available_bytes -= bytes;
        self.available_capabilities -= capabilities;
        true
    }

    pub(super) fn refill_ready(&mut self, channel: &mut ChannelState) -> bool {
        if channel.available == 0 {
            return false;
        }
        let mut exposed_fitting_candidate = false;
        let remaining = READY_WINDOW.saturating_sub(channel.ready.len());
        let candidates = channel
            .pending
            .iter()
            .filter(|(id, _)| !channel.ready.contains(id))
            .take(remaining)
            .map(|(&id, waiter)| (id, Arc::clone(waiter)))
            .collect::<Vec<_>>();
        for (id, waiter) in candidates {
            channel.ready.insert(id);
            exposed_fitting_candidate |= waiter.bytes <= self.available_bytes
                && waiter.capabilities <= self.available_capabilities;
            self.ready.insert(id, waiter);
        }
        exposed_fitting_candidate
    }

    pub(super) fn register_waiter(
        &mut self,
        channel: &mut ChannelState,
        waiter: &Arc<Waiter>,
    ) -> bool {
        channel.pending.insert(waiter.id, Arc::clone(waiter));
        if self.refill_ready(channel) {
            return true;
        }
        if channel.available == 0
            || channel.ready.len() < READY_WINDOW
            || waiter.bytes > self.available_bytes
            || waiter.capabilities > self.available_capabilities
        {
            return false;
        }

        let Some(displaced) = channel.ready.iter().rev().copied().find(|id| {
            let candidate = channel
                .pending
                .get(id)
                .expect("a ready message waiter belongs to its channel");
            candidate.bypasses.load(Ordering::Relaxed) < MAXIMUM_BYPASSES
                && (candidate.bytes > self.available_bytes
                    || candidate.capabilities > self.available_capabilities)
        }) else {
            return false;
        };
        channel.ready.remove(&displaced);
        self.ready.remove(&displaced);
        channel.ready.insert(waiter.id);
        self.ready.insert(waiter.id, Arc::clone(waiter));
        true
    }

    fn demote_ready(&mut self, channel: &mut ChannelState) {
        for id in std::mem::take(&mut channel.ready) {
            self.ready.remove(&id);
        }
    }

    pub(super) fn cancel_waiter(&mut self, waiter: &Arc<Waiter>) -> bool {
        let removed_fairness_barrier = self
            .ready
            .remove(&waiter.id)
            .is_some_and(|queued| queued.bypasses.load(Ordering::Relaxed) >= MAXIMUM_BYPASSES);
        let mut channel = waiter
            .channel
            .state
            .lock()
            .expect("message channel state poisoned");
        channel.pending.remove(&waiter.id);
        channel.ready.remove(&waiter.id);
        let exposed_fitting_candidate = self.refill_ready(&mut channel);
        removed_fairness_barrier || exposed_fitting_candidate
    }

    pub(super) fn return_capacity(
        &mut self,
        channel: &Arc<MessageChannel>,
        bytes: usize,
        capabilities: usize,
    ) {
        self.available_bytes = self
            .available_bytes
            .checked_add(bytes)
            .expect("message byte admission release cannot overflow");
        self.available_capabilities = self
            .available_capabilities
            .checked_add(capabilities)
            .expect("message capability admission release cannot overflow");
        let mut channel = channel
            .state
            .lock()
            .expect("message channel state poisoned");
        channel.available = channel
            .available
            .checked_add(1)
            .expect("message channel admission release cannot overflow");
        debug_assert!(channel.available <= channel.capacity);
        let _ = self.refill_ready(&mut channel);
    }

    pub(super) fn schedule(&mut self) {
        loop {
            let mut selected = None;
            for (&id, waiter) in &self.ready {
                #[cfg(test)]
                {
                    self.candidate_visits += 1;
                }
                if waiter.bytes <= self.available_bytes
                    && waiter.capabilities <= self.available_capabilities
                {
                    selected = Some(id);
                    break;
                }
                if waiter.bypasses.load(Ordering::Relaxed) >= MAXIMUM_BYPASSES {
                    break;
                }
            }
            let Some(selected) = selected else {
                return;
            };
            for waiter in self.ready.range_mut(..selected).map(|(_, waiter)| waiter) {
                let bypasses = waiter.bypasses.load(Ordering::Relaxed);
                waiter
                    .bypasses
                    .store(bypasses.saturating_add(1), Ordering::Relaxed);
            }
            let queued = self
                .ready
                .remove(&selected)
                .expect("selected message waiter exists");
            let mut channel = queued
                .channel
                .state
                .lock()
                .expect("message channel state poisoned");
            channel.ready.remove(&selected);
            channel
                .pending
                .remove(&selected)
                .expect("a ready message waiter belongs to its channel");
            channel.available -= 1;
            if channel.available == 0 {
                self.demote_ready(&mut channel);
            } else {
                let _ = self.refill_ready(&mut channel);
            }
            self.available_bytes -= queued.bytes;
            self.available_capabilities -= queued.capabilities;
            queued.granted.store(true, Ordering::Release);
            queued.ready.notify_one();
        }
    }

    pub(super) fn available_within(&self, byte_limit: usize, capability_limit: usize) -> bool {
        self.available_bytes <= byte_limit && self.available_capabilities <= capability_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU8};
    use tokio::sync::Notify;

    #[test]
    fn channel_candidate_window_is_constant_at_default_scale() {
        let channel = MessageChannel::new(1);
        let mut channel_state = channel.state.lock().unwrap();
        for raw in 1..=65_536 {
            let id = WaiterId(raw);
            channel_state.pending.insert(
                id,
                Arc::new(Waiter {
                    id,
                    bytes: 1,
                    capabilities: 0,
                    channel: Arc::clone(&channel),
                    bypasses: AtomicU8::new(0),
                    granted: AtomicBool::new(false),
                    ready: Notify::new(),
                }),
            );
        }
        let mut admission = AdmissionState::new(0, 1);

        let _ = admission.refill_ready(&mut channel_state);

        assert_eq!(channel_state.ready.len(), READY_WINDOW);
        assert_eq!(admission.ready.len(), READY_WINDOW);
        assert_eq!(channel_state.pending.len(), 65_536);
    }

    #[test]
    fn fitting_registration_displaces_a_nonfitting_full_window_candidate() {
        let channel = MessageChannel::new(1);
        let mut admission = AdmissionState::new(1, 0);
        let fitting = Arc::new(Waiter {
            id: WaiterId(READY_WINDOW as u64 + 1),
            bytes: 1,
            capabilities: 0,
            channel: Arc::clone(&channel),
            bypasses: AtomicU8::new(0),
            granted: AtomicBool::new(false),
            ready: Notify::new(),
        });
        {
            let mut channel_state = channel.state.lock().unwrap();
            for raw in 1..=READY_WINDOW as u64 {
                let waiter = Arc::new(Waiter {
                    id: WaiterId(raw),
                    bytes: 2,
                    capabilities: 0,
                    channel: Arc::clone(&channel),
                    bypasses: AtomicU8::new(0),
                    granted: AtomicBool::new(false),
                    ready: Notify::new(),
                });
                assert!(!admission.register_waiter(&mut channel_state, &waiter));
            }
            assert_eq!(channel_state.ready.len(), READY_WINDOW);

            assert!(admission.register_waiter(&mut channel_state, &fitting));
            assert_eq!(channel_state.ready.len(), READY_WINDOW);
            assert!(channel_state.ready.contains(&fitting.id));
        }

        admission.schedule();
        assert!(
            fitting.granted.load(Ordering::Acquire),
            "the newly fitting waiter was not granted"
        );
    }

    #[test]
    fn cancelling_nonfitting_waiters_does_not_rescan_unchanged_candidates() {
        let mut admission = AdmissionState::new(0, 0);
        let mut waiters = Vec::with_capacity(65_536);
        for raw in 1..=65_536 {
            let id = WaiterId(raw);
            let channel = MessageChannel::new(1);
            let waiter = Arc::new(Waiter {
                id,
                bytes: 1,
                capabilities: 0,
                channel: Arc::clone(&channel),
                bypasses: AtomicU8::new(0),
                granted: AtomicBool::new(false),
                ready: Notify::new(),
            });
            {
                let mut channel_state = channel.state.lock().unwrap();
                channel_state.pending.insert(id, Arc::clone(&waiter));
                channel_state.ready.insert(id);
            }
            admission.ready.insert(id, Arc::clone(&waiter));
            waiters.push(waiter);
        }

        for waiter in &waiters {
            if admission.cancel_waiter(waiter) {
                admission.schedule();
            }
        }

        assert_eq!(admission.candidate_visits, 0);
    }

    #[test]
    fn same_channel_demotion_preserves_the_bounded_bypass_barrier() {
        let channel = MessageChannel::new(1);
        let oldest = Arc::new(Waiter {
            id: WaiterId(1),
            bytes: 4,
            capabilities: 0,
            channel: Arc::clone(&channel),
            bypasses: AtomicU8::new(0),
            granted: AtomicBool::new(false),
            ready: Notify::new(),
        });
        let younger = (2..=66)
            .map(|raw| {
                Arc::new(Waiter {
                    id: WaiterId(raw),
                    bytes: 1,
                    capabilities: 0,
                    channel: Arc::clone(&channel),
                    bypasses: AtomicU8::new(0),
                    granted: AtomicBool::new(false),
                    ready: Notify::new(),
                })
            })
            .collect::<Vec<_>>();
        {
            let mut channel_state = channel.state.lock().unwrap();
            channel_state.pending.insert(oldest.id, Arc::clone(&oldest));
            for waiter in &younger {
                channel_state.pending.insert(waiter.id, Arc::clone(waiter));
            }
        }
        let mut admission = AdmissionState::new(1, 0);
        {
            let mut channel_state = channel.state.lock().unwrap();
            let _ = admission.refill_ready(&mut channel_state);
        }

        admission.schedule();
        for _ in 0..MAXIMUM_BYPASSES {
            admission.return_capacity(&channel, 1, 0);
            admission.schedule();
        }

        assert!(!oldest.granted.load(Ordering::Acquire));
        assert!(
            younger[..MAXIMUM_BYPASSES as usize]
                .iter()
                .all(|waiter| waiter.granted.load(Ordering::Acquire))
        );
        assert!(
            younger[MAXIMUM_BYPASSES as usize..]
                .iter()
                .all(|waiter| !waiter.granted.load(Ordering::Acquire)),
            "a same-channel demotion reset the oldest waiter's bypass barrier",
        );
    }
}
