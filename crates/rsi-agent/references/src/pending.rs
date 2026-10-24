//! No TTL: only explicit acceptance/removal or owner retirement releases captures.
use super::*;
use rsi_agent_session_protocol::SessionId;
use rsi_agent_store_protocol::PreparedCasObject;
use std::collections::BTreeMap;
use std::sync::Weak;
use tokio::sync::OwnedSemaphorePermit;
#[derive(Debug)]
pub(super) struct Pending {
    slots: Arc<Semaphore>,
    state: Mutex<State>,
}
#[derive(Debug, Default)]
struct State {
    sessions: BTreeMap<SessionId, Weak<Semaphore>>,
    objects: BTreeMap<(SessionId, String), (PreparedCasObject, Reservation)>,
}
#[derive(Debug)]
pub(super) struct Reservation {
    pub(super) id: String,
    _global: OwnedSemaphorePermit,
    _session: OwnedSemaphorePermit,
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(256)),
            state: Mutex::default(),
        }
    }
}
impl Pending {
    pub(super) fn reserve(&self, id: &SessionId) -> Result<Reservation> {
        let global = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReferenceError::Capacity)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sessions.retain(|_, entry| entry.strong_count() != 0);
        let session = state
            .sessions
            .get(id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let slots = Arc::new(Semaphore::new(4));
                state.sessions.insert(id.clone(), Arc::downgrade(&slots));
                slots
            });
        let session = session
            .try_acquire_owned()
            .map_err(|_| ReferenceError::Capacity)?;
        let mut id = [0; 16];
        getrandom::fill(&mut id).map_err(|_| ReferenceError::WorkerFailed)?;
        Ok(Reservation {
            id: hex::encode(id),
            _global: global,
            _session: session,
        })
    }
    pub(super) fn publish(
        &self,
        id: SessionId,
        object: PreparedCasObject,
        reservation: Reservation,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.slots.is_closed() || reservation._session.semaphore().is_closed() {
            return Err(ReferenceError::Cancelled);
        }
        state
            .objects
            .insert((id, reservation.id.clone()), (object, reservation));
        Ok(())
    }
    pub(super) fn release(&self, id: &SessionId, capture: &str) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .objects
            .remove(&(id.clone(), capture.into()));
    }
    pub(super) fn release_session(&self, id: &SessionId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slots) = state.sessions.remove(id).and_then(|slots| slots.upgrade()) {
            slots.close();
        }
        state.objects.retain(|(session, _), _| session != id);
    }
    pub(super) fn clear(&self) {
        self.slots.close();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .objects
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn object(index: usize) -> PreparedCasObject {
        PreparedCasObject::new(
            rsi_agent_store_protocol::CasObjectRef {
                sha256: format!("{index:064x}"),
                byte_len: 1,
            },
            (),
        )
        .unwrap()
    }
    #[test]
    fn duplicate_capture_has_independent_owners_and_idempotent_release() {
        let pending = Pending::default();
        let id = SessionId::new("duplicate").unwrap();
        let retained = Arc::new(());
        let original =
            PreparedCasObject::new(object(0).reference().clone(), retained.clone()).unwrap();
        let first = pending.reserve(&id).unwrap();
        let first_id = first.id.clone();
        pending.publish(id.clone(), original, first).unwrap();
        let second = pending.reserve(&id).unwrap();
        let second_id = second.id.clone();
        let other_retained = Arc::new(());
        pending
            .publish(
                id.clone(),
                PreparedCasObject::new(object(0).reference().clone(), other_retained.clone())
                    .unwrap(),
                second,
            )
            .unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(pending.slots.available_permits(), 254);
        assert_eq!(Arc::strong_count(&retained), 2);
        pending.release(&id, &first_id);
        assert_eq!(Arc::strong_count(&retained), 1);
        pending.release(&id, &first_id);
        assert_eq!(Arc::strong_count(&other_retained), 2);
        assert_eq!(pending.slots.available_permits(), 255);
        pending.release(&id, &second_id);
        assert_eq!(Arc::strong_count(&other_retained), 1);
        assert_eq!(pending.slots.available_permits(), 256);
    }
    #[test]
    fn released_session_rejects_in_flight_publication_but_allows_new_captures() {
        let pending = Pending::default();
        let id = SessionId::new("released").unwrap();
        let in_flight = pending.reserve(&id).unwrap();
        pending.release_session(&id);
        let fresh = pending.reserve(&id).unwrap();
        assert!(matches!(
            pending.publish(id.clone(), object(0), in_flight),
            Err(ReferenceError::Cancelled)
        ));
        pending.publish(id.clone(), object(1), fresh).unwrap();
        assert_eq!(pending.slots.available_permits(), 255);
        pending.release_session(&id);
        assert_eq!(pending.slots.available_permits(), 256);
    }
    #[test]
    fn pending_capture_limits_release_and_owner_close_are_explicit() {
        let pending = Pending::default();
        let id = SessionId::new("draft").unwrap();
        let mut ids = Vec::new();
        for n in 0..4 {
            let reservation = pending.reserve(&id).unwrap();
            ids.push(reservation.id.clone());
            pending.publish(id.clone(), object(n), reservation).unwrap();
        }
        assert!(matches!(
            pending.reserve(&id),
            Err(ReferenceError::Capacity)
        ));
        pending.release(&id, &ids[0]);
        let reserved = pending.reserve(&id).unwrap();
        drop(reserved);
        pending.release_session(&id);
        for n in 0..256 {
            let id = SessionId::new(format!("session-{n}")).unwrap();
            pending
                .publish(id.clone(), object(n), pending.reserve(&id).unwrap())
                .unwrap();
        }
        assert!(matches!(
            pending.reserve(&id),
            Err(ReferenceError::Capacity)
        ));
        pending.release_session(&SessionId::new("session-0").unwrap());
        let in_flight = pending.reserve(&id).unwrap();
        pending.clear();
        assert!(matches!(
            pending.publish(id, object(999), in_flight),
            Err(ReferenceError::Cancelled)
        ));
        assert!(pending.state.lock().unwrap().objects.is_empty());
    }
}
