//! Validation proofs and bounded in-use ownership, independent of payload admission.

use super::*;
use rsi_agent_store_protocol::SessionValidationLease;
use tokio::sync::OwnedSemaphorePermit;

#[derive(Debug)]
pub(super) struct ValidatedSessionProof;

pub(super) struct PinnedProof {
    proof: Arc<ValidatedSessionProof>,
    _slot: OwnedSemaphorePermit,
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
        let proof = self.session_proof(id).await?;
        self.ensure_not_retired(id).await?;
        let mut pins = self
            .inner
            .pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pin) = pins.get(id).and_then(std::sync::Weak::upgrade) {
            return Ok(SessionValidationLease::new(pin));
        }
        // Nested captures retain earlier ancestors. Waiting here can make every
        // partial capture wait for capacity owned by another partial capture.
        let slot = Arc::clone(&self.inner.pin_admission)
            .try_acquire_owned()
            .map_err(|_| StoreError::ValidationBusy)?;
        pins.retain(|_, proof| proof.strong_count() > 0);
        let pin = Arc::new(PinnedProof { proof, _slot: slot });
        pins.insert(id.clone(), Arc::downgrade(&pin));
        Ok(SessionValidationLease::new(pin))
    }
}
