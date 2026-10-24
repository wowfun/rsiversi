use super::{Arc, Result, SessionError, SessionId};
use rsi_session_protocol::{SessionEpoch, SessionLifecycle};
use std::{
    collections::BTreeMap,
    sync::{Mutex as StdMutex, Weak},
};

#[derive(Debug, Default)]
pub(super) struct Lifecycles(StdMutex<BTreeMap<SessionId, Weak<SessionLifecycle>>>);
impl Lifecycles {
    pub(super) fn has_hard_owners(&self, session: &SessionId) -> bool {
        self.0
            .lock()
            .expect("Session lifecycles poisoned")
            .get(session)
            .and_then(Weak::upgrade)
            .is_some_and(|gate| gate.has_hard_owners())
    }
    pub(super) fn gate(&self, session: &SessionId) -> Result<Arc<SessionLifecycle>> {
        let mut entries = self.0.lock().expect("Session lifecycles poisoned");
        if let Some(gate) = entries.get(session).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        entries.retain(|_, gate| gate.strong_count() != 0);
        if entries.len() >= 1024 {
            return Err(SessionError::Capacity);
        }
        let gate = Arc::new(SessionLifecycle::default());
        entries.insert(session.clone(), Arc::downgrade(&gate));
        Ok(gate)
    }
    pub(super) fn attach(&self, session: &SessionId) -> Result<SessionEpoch> {
        self.gate(session)?.attach()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advisory_ownership_does_not_allocate_or_require_registry_capacity() {
        let registry = Lifecycles::default();
        let gates: Vec<_> = (0..1024)
            .map(|n| {
                registry
                    .gate(&SessionId::new(format!("session-{n}")).unwrap())
                    .unwrap()
            })
            .collect();
        let absent = SessionId::new("absent").unwrap();
        assert!(!registry.has_hard_owners(&absent));
        assert!(matches!(
            registry.gate(&absent),
            Err(SessionError::Capacity)
        ));
        let owner = gates[0].attach().unwrap().hard().unwrap();
        assert!(registry.has_hard_owners(&SessionId::new("session-0").unwrap()));
        drop(owner);
        assert!(!registry.has_hard_owners(&SessionId::new("session-0").unwrap()));
    }
}
