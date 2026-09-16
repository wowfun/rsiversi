//! Validation proofs and bounded in-use ownership, independent of payload admission.

use super::*;
use rsi_agent_store_protocol::SessionValidationLease;
use tokio::sync::OwnedSemaphorePermit;

#[derive(Debug)]
pub(super) struct ValidatedSessionProof;

pub(super) struct PinnedProof {
    proof: Arc<ValidatedSessionProof>,
    slot: Option<OwnedSemaphorePermit>,
    owner: Arc<StoreInner>,
}

impl Drop for PinnedProof {
    fn drop(&mut self) {
        // An upgrade may be the last strong reference while the registry is locked.
        // Never relock it here. Release capacity before notifying waiting sessions.
        drop(self.slot.take());
        self.owner.pin_changed.notify_waiters();
    }
}

impl StoreInner {
    fn pinned(&self, id: &SessionId) -> Option<Arc<PinnedProof>> {
        let mut pins = self
            .pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pin = pins.get(id).and_then(|proof| {
            #[cfg(test)]
            self.pin_entries_examined.fetch_add(1, Ordering::Relaxed);
            proof.upgrade()
        });
        if pin.is_none() {
            pins.remove(id);
        }
        pin
    }

    pub(super) fn session_proof(&self, id: &SessionId) -> Option<Arc<ValidatedSessionProof>> {
        if let Some(pin) = self.pinned(id) {
            return Some(Arc::clone(&pin.proof));
        }
        let mut cache = self.validated_sessions.lock().ok()?;
        if !cache.touch(id) {
            return None;
        }
        cache.proofs.get(id).cloned()
    }
}

impl SqliteStore {
    pub(super) async fn pin_session(&self, id: &SessionId) -> Result<SessionValidationLease> {
        // Validation never waits for a pin slot while holding the validation lane.
        let proof = self.session_proof(id).await?;
        let acquire = Arc::clone(&self.inner.pin_admission).acquire_owned();
        tokio::pin!(acquire);
        loop {
            let changed = self.inner.pin_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(pin) = self.inner.pinned(id) {
                return Ok(SessionValidationLease::new(pin));
            }
            let slot = tokio::select! {
                result = &mut acquire =>
                    result.map_err(|_| StoreError::Io("SQLite pin admission closed".into()))?,
                () = &mut changed => continue,
            };
            let mut pins = self
                .inner
                .pins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pins.retain(|_, proof| proof.strong_count() > 0);
            if let Some(pin) = pins.get(id).and_then(std::sync::Weak::upgrade) {
                return Ok(SessionValidationLease::new(pin));
            }
            let pin = Arc::new(PinnedProof {
                proof,
                slot: Some(slot),
                owner: Arc::clone(&self.inner),
            });
            pins.insert(id.clone(), Arc::downgrade(&pin));
            drop(pins);
            self.inner.pin_changed.notify_waiters();
            return Ok(SessionValidationLease::new(pin));
        }
    }
}
