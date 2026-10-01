//! Live authority follows one pending input into its accepted Turn.
use super::*;
use rsi_execution::{ExecutionLease, ExecutionLocation, ExecutionOperation};

const MAXIMUM_PENDING_EXECUTION_LEASES: usize = 4096;
type Key = (SessionId, MessageId);
#[derive(Clone)]
pub(super) struct Pending {
    execution: ExecutionLease,
    slot: Arc<OwnedSemaphorePermit>,
}
pub(super) struct Messages {
    entries: Mutex<BTreeMap<SessionId, BTreeMap<MessageId, Arc<Pending>>>>,
    slots: Arc<Semaphore>,
}
impl Default for Messages {
    fn default() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            slots: Arc::new(Semaphore::new(MAXIMUM_PENDING_EXECUTION_LEASES)),
        }
    }
}
impl Messages {
    pub(super) fn clear(&self) {
        self.entries.lock().expect("execution messages").clear();
    }
    pub(super) fn reserve(
        &self,
        session: &SessionId,
        message: &MessageId,
        execution: Option<&ExecutionLease>,
    ) -> TurnResult<Option<Reservation>> {
        let Some(execution) = execution else {
            return Ok(None);
        };
        let key = (session.clone(), message.clone());
        let entries = self.entries.lock().expect("execution messages");
        let slot = match entries
            .get(session)
            .and_then(|messages| messages.get(message))
        {
            Some(pending) => pending.slot.clone(),
            None => Arc::new(
                self.slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| TurnError::Capacity)?,
            ),
        };
        Ok(Some(Reservation {
            key,
            pending: Pending {
                execution: execution.clone(),
                slot,
            },
        }))
    }
    pub(super) fn publish(&self, reservation: Option<Reservation>) {
        if let Some(reservation) = reservation {
            self.entries
                .lock()
                .expect("execution messages")
                .entry(reservation.key.0)
                .or_default()
                .insert(reservation.key.1, Arc::new(reservation.pending));
        }
    }
    pub(super) fn get(&self, session: &SessionId, message: &MessageId) -> Option<ExecutionLease> {
        self.entries
            .lock()
            .expect("execution messages")
            .get(session)
            .and_then(|messages| messages.get(message))
            .map(|pending| pending.execution.clone())
    }
    pub(super) fn remove(&self, session: &SessionId, message: &MessageId) {
        let mut entries = self.entries.lock().expect("execution messages");
        if let Some(messages) = entries.get_mut(session) {
            messages.remove(message);
            if messages.is_empty() {
                entries.remove(session);
            }
        }
    }
    pub(super) fn snapshot(&self, session: &SessionId) -> BTreeMap<MessageId, Arc<Pending>> {
        self.entries
            .lock()
            .expect("execution messages")
            .get(session)
            .cloned()
            .unwrap_or_default()
    }
    pub(super) fn retain_pending(
        &self,
        session: &SessionId,
        pending: &[DurableMessageEntry],
        observed: &BTreeMap<MessageId, Arc<Pending>>,
    ) {
        let mut entries = self.entries.lock().expect("execution messages");
        if let Some(messages) = entries.get_mut(session) {
            let ids = pending
                .iter()
                .map(|entry| &entry.message.message_id)
                .collect::<BTreeSet<_>>();
            messages.retain(|message, current| {
                ids.contains(message)
                    || !observed
                        .get(message)
                        .is_some_and(|old| Arc::ptr_eq(old, current))
            });
            if messages.is_empty() {
                entries.remove(session);
            }
        }
    }
}
pub(super) struct Reservation {
    key: Key,
    pending: Pending,
}

pub(super) fn admit(
    header: &SessionHeader,
    execution: Option<&ExecutionLease>,
) -> TurnResult<Option<ExecutionOperation>> {
    match execution {
        Some(lease) if lease.binding().location() == header.coordinates().location() => {
            lease.admit().map(Some).map_err(admission_error)
        }
        None if *header.coordinates().location() == ExecutionLocation::Local => Ok(None),
        _ => Err(TurnError::ExecutionUnavailable),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err transfers the provider failure at this boundary"
)]
fn admission_error(error: rsi_process::ProcessError) -> TurnError {
    match error {
        rsi_process::ProcessError::Capacity
        | rsi_process::ProcessError::Api(rsi_api_protocol::ApiError::Capacity) => {
            TurnError::Capacity
        }
        rsi_process::ProcessError::OutcomeUnknown
        | rsi_process::ProcessError::Api(rsi_api_protocol::ApiError::OutcomeUnknown) => {
            TurnError::ExecutionOutcomeUnknown
        }
        _ => TurnError::ExecutionUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_store_scan_cannot_prune_a_new_or_replaced_publication() {
        let owner = Messages::default();
        let session = SessionId::new("child").unwrap();
        let old = MessageId::new("old").unwrap();
        let replaced = MessageId::new("replaced").unwrap();
        let fresh = MessageId::new("fresh").unwrap();
        let lease = crate::execution_fixture::lease(
            ExecutionLocation::Local,
            Arc::new(crate::execution_fixture::Gate::default()),
            0,
        );
        for id in [&old, &replaced] {
            owner.publish(owner.reserve(&session, id, Some(&lease)).unwrap());
        }
        // The store read is now in flight. Child commit publishes through its parent's gate.
        let observed = owner.snapshot(&session);
        for id in [&replaced, &fresh] {
            owner.publish(owner.reserve(&session, id, Some(&lease)).unwrap());
        }
        owner.retain_pending(&session, &[], &observed);
        drop(observed);
        assert!(owner.get(&session, &old).is_none());
        assert_eq!(owner.get(&session, &replaced), Some(lease.clone()));
        assert_eq!(owner.get(&session, &fresh), Some(lease));
        assert_eq!(
            owner.slots.available_permits(),
            MAXIMUM_PENDING_EXECUTION_LEASES - 2
        );
    }

    #[test]
    fn admission_preserves_contention_and_uncertainty() {
        use rsi_api_protocol::ApiError;
        use rsi_process::ProcessError;
        for error in [
            ProcessError::Capacity,
            ProcessError::Api(ApiError::Capacity),
        ] {
            assert!(matches!(admission_error(error), TurnError::Capacity));
        }
        for error in [
            ProcessError::OutcomeUnknown,
            ProcessError::Api(ApiError::OutcomeUnknown),
        ] {
            assert!(matches!(
                admission_error(error),
                TurnError::ExecutionOutcomeUnknown
            ));
        }
        assert!(matches!(
            admission_error(ProcessError::ShuttingDown),
            TurnError::ExecutionUnavailable
        ));
    }
    #[test]
    fn pending_capacity_reuses_exact_keys_and_releases_cancelled_or_unpublished_reservations() {
        let owner = Messages::default();
        let session = SessionId::new("capacity").unwrap();
        let lease = crate::execution_fixture::lease(
            ExecutionLocation::Local,
            Arc::new(crate::execution_fixture::Gate::default()),
            0,
        );
        for index in 0..MAXIMUM_PENDING_EXECUTION_LEASES {
            let message = MessageId::new(format!("message-{index}")).unwrap();
            owner.publish(owner.reserve(&session, &message, Some(&lease)).unwrap());
        }
        let extra = MessageId::new("extra").unwrap();
        assert!(matches!(
            owner.reserve(&session, &extra, Some(&lease)),
            Err(TurnError::Capacity)
        ));
        let existing = MessageId::new("message-0").unwrap();
        let replacement = crate::execution_fixture::lease(
            ExecutionLocation::Local,
            Arc::new(crate::execution_fixture::Gate::default()),
            0,
        );
        owner.publish(
            owner
                .reserve(&session, &existing, Some(&replacement))
                .unwrap(),
        );
        assert_eq!(owner.get(&session, &existing), Some(replacement));
        owner.remove(&session, &existing);
        let unpublished = owner.reserve(&session, &extra, Some(&lease)).unwrap();
        drop(unpublished);
        assert!(owner.get(&session, &extra).is_none());
        owner.publish(owner.reserve(&session, &extra, Some(&lease)).unwrap());
        owner.retain_pending(&session, &[], &owner.snapshot(&session));
        assert_eq!(
            owner.slots.available_permits(),
            MAXIMUM_PENDING_EXECUTION_LEASES
        );
    }

    #[test]
    fn session_reconciliation_preserves_other_sessions_and_reserved_capacity() {
        let owner = Messages::default();
        let a = SessionId::new("a").unwrap();
        let b = SessionId::new("b").unwrap();
        let message = MessageId::new("same-message-id").unwrap();
        let lease = crate::execution_fixture::lease(
            ExecutionLocation::Local,
            Arc::new(crate::execution_fixture::Gate::default()),
            0,
        );
        owner.publish(owner.reserve(&a, &message, Some(&lease)).unwrap());
        owner.publish(owner.reserve(&b, &message, Some(&lease)).unwrap());
        let reserved = owner.reserve(&a, &message, Some(&lease)).unwrap();
        owner.retain_pending(&a, &[], &owner.snapshot(&a));
        assert!(owner.get(&a, &message).is_none());
        assert_eq!(owner.get(&b, &message), Some(lease.clone()));
        assert_eq!(
            owner.slots.available_permits(),
            MAXIMUM_PENDING_EXECUTION_LEASES - 2
        );
        owner.publish(reserved);
        assert_eq!(owner.get(&a, &message), Some(lease));
        owner.remove(&b, &message);
        assert!(owner.get(&a, &message).is_some());
        owner.remove(&a, &message);
        assert!(owner.entries.lock().unwrap().is_empty());
        assert_eq!(
            owner.slots.available_permits(),
            MAXIMUM_PENDING_EXECUTION_LEASES
        );
    }
}
