//! Lifecycle adapter for one unpublished domain catalog. No callback runs under this lock.

use rsi_agent_composition_protocol::{
    DomainBinding, DomainCatalog, DomainCatalogBuilder, DomainError, DomainRegistrar,
    DomainRegistration,
};
use rsi_meta::{
    MetaError, RegistrationContext, RegistrationLease, RegistrationPosition, RuntimeIdentity,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct State {
    builder: Option<DomainCatalogBuilder>,
    positions: BTreeMap<String, (DomainBinding, RegistrationPosition)>,
}

#[derive(Debug)]
struct Registrar {
    runtime: RuntimeIdentity,
    state: Arc<Mutex<State>>,
}

/// Owner guard closes admission even if a candidate leaks a registrar during rollback.
#[derive(Debug)]
pub(super) struct DomainStage {
    registrar: Arc<Registrar>,
}

impl DomainStage {
    pub(super) fn new(runtime: RuntimeIdentity) -> Self {
        Self {
            registrar: Arc::new(Registrar {
                runtime,
                state: Arc::new(Mutex::new(State {
                    builder: Some(DomainCatalogBuilder::new()),
                    positions: BTreeMap::new(),
                })),
            }),
        }
    }
    pub(super) fn registrar(&self) -> Arc<dyn DomainRegistrar> {
        self.registrar.clone()
    }
    pub(super) fn seal(&self) -> Result<DomainCatalog, DomainError> {
        let mut state = self.registrar.state.lock().expect("domain stage poisoned");
        let mut builder = state.builder.take().ok_or(DomainError::Closed)?;
        for (_, (binding, position)) in std::mem::take(&mut state.positions) {
            if !position.is_admitting() {
                builder.withdraw(&binding);
            }
        }
        drop(state);
        builder.finish()
    }
}

impl Drop for DomainStage {
    fn drop(&mut self) {
        let mut state = self.registrar.state.lock().expect("domain stage poisoned");
        let builder = state.builder.take();
        let positions = std::mem::take(&mut state.positions);
        drop(state);
        drop((builder, positions));
    }
}

impl DomainRegistrar for Registrar {
    fn register(
        &self,
        context: &RegistrationContext,
        definition: DomainRegistration,
    ) -> Result<(DomainBinding, RegistrationLease), DomainError> {
        if context.runtime_identity() != self.runtime {
            return Err(DomainError::RegistrationUnavailable);
        }
        if self
            .state
            .lock()
            .expect("domain stage poisoned")
            .builder
            .is_none()
        {
            return Err(DomainError::Closed);
        }
        let slot = Arc::new(Mutex::new(None::<DomainBinding>));
        let undo_slot = slot.clone();
        let owner = Arc::downgrade(&self.state);
        let mut admission_error = None;
        let result = context.register(
            "withdraw Agent domain definition",
            move || {
                let binding = undo_slot.lock().expect("domain binding poisoned").take();
                if let (Some(owner), Some(binding)) = (owner.upgrade(), binding) {
                    let mut state = owner.lock().expect("domain stage poisoned");
                    if state
                        .builder
                        .as_mut()
                        .is_some_and(|builder| builder.withdraw(&binding))
                    {
                        state.positions.remove(binding.identity().id());
                    }
                }
                Ok(())
            },
            |position| {
                let mut state = self.state.lock().expect("domain stage poisoned");
                let binding = state
                    .builder
                    .as_mut()
                    .ok_or(DomainError::Closed)
                    .and_then(|builder| builder.register(definition))
                    .map_err(|error| {
                        let diagnostic = error.to_string();
                        admission_error = Some(error);
                        MetaError::InvalidInput(diagnostic)
                    })?;
                state
                    .positions
                    .insert(binding.identity().id().into(), (binding.clone(), position));
                *slot.lock().expect("domain binding poisoned") = Some(binding.clone());
                Ok(binding)
            },
        );
        result.map_err(|_| admission_error.unwrap_or(DomainError::RegistrationUnavailable))
    }
}
