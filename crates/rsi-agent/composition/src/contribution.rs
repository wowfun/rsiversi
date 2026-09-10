//! Exact registration lifecycle for one unpublished execution catalog.

use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionError, ContributionRegistrar, ContributionRegistration,
    ContributionResult, MAXIMUM_AGENT_CONTRIBUTIONS,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_meta::{
    MetaError, RegistrationContext, RegistrationLease, RegistrationPosition, RuntimeIdentity,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct Entry {
    token: Arc<()>,
    contribution: ContributionRegistration,
    position: RegistrationPosition,
}

#[derive(Debug)]
struct Registrar {
    runtime: RuntimeIdentity,
    entries: Arc<Mutex<Option<BTreeMap<ContributionId, Entry>>>>,
}

/// Closing the owner also closes a registrar retained by a failed candidate.
#[derive(Debug)]
pub(super) struct ContributionStage {
    registrar: Arc<Registrar>,
}

impl ContributionStage {
    pub(super) fn new(runtime: RuntimeIdentity) -> Self {
        Self {
            registrar: Arc::new(Registrar {
                runtime,
                entries: Arc::new(Mutex::new(Some(BTreeMap::new()))),
            }),
        }
    }

    pub(super) fn registrar(&self) -> Arc<dyn ContributionRegistrar> {
        self.registrar.clone()
    }

    pub(super) fn seal(&self) -> ContributionResult<ContributionCatalog> {
        let entries = self
            .registrar
            .entries
            .lock()
            .expect("contribution stage poisoned")
            .take()
            .ok_or(ContributionError::Closed)?;
        ContributionCatalog::freeze(
            entries
                .into_values()
                .map(|entry| (entry.contribution, entry.position))
                .collect(),
        )
    }
}

impl Drop for ContributionStage {
    fn drop(&mut self) {
        let entries = self
            .registrar
            .entries
            .lock()
            .expect("contribution stage poisoned")
            .take();
        // User callback destructors run outside the registrar lock.
        drop(entries);
    }
}

impl ContributionRegistrar for Registrar {
    fn register(
        &self,
        context: &RegistrationContext,
        contribution: ContributionRegistration,
    ) -> ContributionResult<RegistrationLease> {
        if context.runtime_identity() != self.runtime {
            return Err(ContributionError::RegistrationUnavailable);
        }
        if self
            .entries
            .lock()
            .expect("contribution stage poisoned")
            .is_none()
        {
            return Err(ContributionError::Closed);
        }
        let id = contribution.id().clone();
        let undo_id = id.clone();
        let token = Arc::new(());
        let undo_token = token.clone();
        let owner = Arc::downgrade(&self.entries);
        let mut contribution = Some(contribution);
        let mut admission_error = None;
        let result = context.register(
            "withdraw Agent execution contribution",
            move || {
                let removed = owner.upgrade().and_then(|owner| {
                    let mut state = owner.lock().expect("contribution stage poisoned");
                    let entries = state.as_mut()?;
                    if entries
                        .get(&undo_id)
                        .is_some_and(|entry| Arc::ptr_eq(&entry.token, &undo_token))
                    {
                        entries.remove(&undo_id)
                    } else {
                        None
                    }
                });
                drop(removed);
                Ok(())
            },
            |position| {
                let mut state = self.entries.lock().expect("contribution stage poisoned");
                let result = state
                    .as_mut()
                    .ok_or(ContributionError::Closed)
                    .and_then(|entries| {
                        if entries.contains_key(&id) {
                            return Err(ContributionError::Duplicate(id.clone()));
                        }
                        if entries.len() >= MAXIMUM_AGENT_CONTRIBUTIONS {
                            return Err(ContributionError::Capacity);
                        }
                        entries.insert(
                            id,
                            Entry {
                                token,
                                contribution: contribution.take().expect("one registration"),
                                position,
                            },
                        );
                        Ok(())
                    });
                result.map_err(|error| {
                    let message = error.to_string();
                    admission_error = Some(error);
                    MetaError::InvalidInput(message)
                })
            },
        );
        result
            .map(|((), lease)| lease)
            .map_err(|_| admission_error.unwrap_or(ContributionError::RegistrationUnavailable))
    }
}
