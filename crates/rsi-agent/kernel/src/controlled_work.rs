use rsi_agent_session_protocol::{SessionId, TurnId};
use rsi_agent_turn_protocol::{ControlledWork, ControlledWorkStatus, Result, TurnClaim, TurnError};
use std::collections::VecDeque;

const MAXIMUM_OBSERVATIONS: usize = 1024;
struct Entry {
    key: (SessionId, TurnId),
    claim: u64,
    source: ControlledWork,
}

#[derive(Default)]
pub(super) struct Registry(VecDeque<Entry>);
impl Registry {
    pub(super) fn publish(&mut self, claim: &TurnClaim, source: ControlledWork) -> Result<()> {
        self.insert(
            (claim.session_id().clone(), claim.turn_id().clone()),
            claim.claim_id(),
            source,
        )
    }
    fn insert(
        &mut self,
        key: (SessionId, TurnId),
        claim: u64,
        source: ControlledWork,
    ) -> Result<()> {
        if let Some(index) = self.0.iter().position(|entry| entry.key == key) {
            if self.0[index].claim == claim
                || self.0[index].source.status() == ControlledWorkStatus::Running
            {
                return Err(TurnError::Invalid(
                    "controlled work already published or still running".into(),
                ));
            }
            self.0.remove(index);
        }
        if self.0.len() == MAXIMUM_OBSERVATIONS {
            let index = self
                .0
                .iter()
                .position(|entry| entry.source.status() != ControlledWorkStatus::Running)
                .ok_or(TurnError::Capacity)?;
            self.0.remove(index);
        }
        self.0.push_back(Entry { key, claim, source });
        Ok(())
    }
    pub(super) fn get(&self, session: &SessionId, turn: &TurnId) -> Option<ControlledWork> {
        self.0.iter().find_map(|entry| {
            (&entry.key.0 == session && &entry.key.1 == turn).then(|| entry.source.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_never_evicts_running_work_and_eviction_preserves_detached_observers() {
        let key = |index| {
            (
                SessionId::new("s").unwrap(),
                TurnId::new(format!("t{index}")).unwrap(),
            )
        };
        let mut registry = Registry::default();
        let mut reporters = Vec::new();
        for index in 0..MAXIMUM_OBSERVATIONS {
            let (view, reporter) = ControlledWork::new();
            registry.insert(key(index), 1, view).unwrap();
            reporters.push(reporter);
        }
        let (view, reporter) = ControlledWork::new();
        assert!(matches!(
            registry.insert(key(1024), 1, view.clone()),
            Err(TurnError::Capacity)
        ));
        let first = registry.get(&key(0).0, &key(0).1).unwrap();
        reporters.remove(0).finish(true);
        registry.insert(key(1024), 1, view).unwrap();
        assert!(registry.get(&key(0).0, &key(0).1).is_none());
        assert_eq!(first.status(), ControlledWorkStatus::Settled);
        assert_eq!(
            registry.get(&key(1).0, &key(1).1).unwrap().status(),
            ControlledWorkStatus::Running
        );
        drop(reporter);
        let (replacement, _owner) = ControlledWork::new();
        assert!(
            registry.insert(key(1024), 1, replacement.clone()).is_err(),
            "one publication per claim even after completion"
        );
        registry.insert(key(1024), 2, replacement).unwrap();
    }
}
