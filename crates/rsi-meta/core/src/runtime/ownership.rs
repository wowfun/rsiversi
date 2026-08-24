#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Runtime {
    pub(super) fn add_effect(&self, owner: Owner, label: String, cleanup: Cleanup) -> Result<()> {
        let _runtime_admission = self.begin_admission(false)?;
        let fiber = self.owner_fiber(owner)?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        Self::validate_owner_data(owner, &data, true)?;
        if !matches!(data.state, FiberState::Loading | FiberState::Active) {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        if active.effects.len() >= self.inner.limits.topology.maximum_effects_per_fiber {
            return Err(MetaError::CapacityExhausted {
                resource: "effects",
            });
        }
        let reservation =
            self.inner
                .resources
                .effects
                .try_reserve(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "effects",
                })?;
        active.effects.push(EffectRecord {
            label,
            cleanup,
            _reservation: reservation,
        });
        Ok(())
    }

    pub(super) fn provide(
        &self,
        context: &Context,
        key: ServiceKey,
        contract: ContractId,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<()> {
        let _runtime_admission = self.begin_admission(false)?;
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot provide a service".to_owned())
        })?;
        let fiber = self.owner_fiber(owner)?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        Self::validate_owner_data(owner, &data, true)?;
        if !matches!(data.state, FiberState::Loading) {
            return Err(MetaError::InvalidInput(
                "services may only be provided during plugin activation".to_owned(),
            ));
        }
        let provision = data
            .descriptor
            .as_deref()
            .expect("registered Fiber retains its descriptor")
            .provision(&key)
            .ok_or_else(|| MetaError::UndeclaredProvision {
                service: key.clone(),
            })?;
        if provision.contract != contract || provision.version != version {
            return Err(MetaError::ContractMismatch {
                service: key,
                expected_id: provision.contract.clone(),
                expected_version: provision.version,
                actual_id: contract,
                actual_version: version,
            });
        }
        let active = data.active.as_mut().expect("loading generation exists");
        if active.services.contains_key(&key) {
            return Err(MetaError::DuplicateProvider { service: key });
        }
        let reservation =
            self.inner
                .resources
                .services
                .try_reserve(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "services",
                })?;
        let binding = Arc::new(ProviderBinding {
            key,
            contract,
            version,
            provider: owner.fiber,
            generation: owner.generation,
            endpoint,
            lease: Arc::clone(&active.lease),
        });
        active.services.insert(
            binding.key.clone(),
            StagedService {
                binding,
                _reservation: reservation,
            },
        );
        Ok(())
    }

    pub(super) fn add_listener(
        &self,
        context: &Context,
        event: EventKey,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
    ) -> Result<EventListenerId> {
        let _runtime_admission = self.begin_admission(false)?;
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own a listener".to_owned())
        })?;
        let id = EventListenerId(self.inner.next_listener.fetch_add(1, Ordering::AcqRel) + 1);
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let fiber = state
            .fibers
            .get(&owner.fiber)
            .cloned()
            .ok_or(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            })?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        let loading = matches!(data.state, FiberState::Loading);
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        let reservation =
            self.inner
                .resources
                .listeners
                .try_reserve(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "event listeners",
                })?;
        active.listeners.insert(id, reservation);
        let listener = Arc::new(ListenerBinding {
            id,
            event: event.clone(),
            owner: owner.fiber,
            generation: owner.generation,
            scope: ContextScope {
                isolation: Arc::clone(&context.isolation),
                intercepts: Arc::clone(&context.intercepts),
                entries: context.entries,
                encoded_bytes: context.encoded_bytes,
                trace: context.trace.clone(),
            },
            handler,
            options,
            lease: Arc::clone(&active.lease),
        });
        state.listener_events.insert(id, event.clone());
        if loading {
            state
                .staged_listeners
                .entry((owner.fiber, owner.generation))
                .or_default()
                .insert(id, listener);
        } else {
            state.listeners.entry(event).or_default().insert(listener);
        }
        state.revision += 1;
        Ok(id)
    }

    pub(super) fn remove_listener(&self, context: &Context, id: EventListenerId) -> bool {
        let Ok(_runtime_admission) = self.begin_admission(false) else {
            return false;
        };
        let Some(owner) = context.owner else {
            return false;
        };
        self.remove_listener_owned(owner, id, ListenerRemovalCause::Explicit)
    }
}
