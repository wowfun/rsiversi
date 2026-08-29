#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::ownership::{EventEffect, EventRemoval};
use super::*;
use crate::Waterfall;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct LocalEventSlot {
    event: TypeId,
    isolation: LocalIsolationId,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LocalListenerLocation {
    slot: LocalEventSlot,
    order: i64,
}

#[derive(Default)]
pub(super) struct LocalEventListeners {
    next_prepend: i64,
    next_append: i64,
    bindings: BTreeMap<i64, Arc<LocalEventBinding>>,
}

impl LocalEventListeners {
    fn insert(&mut self, binding: Arc<LocalEventBinding>, prepend: bool) -> Result<i64> {
        let order = if prepend {
            self.next_prepend =
                self.next_prepend
                    .checked_sub(1)
                    .ok_or(MetaError::CapacityExhausted {
                        resource: "Local listener ordering keys",
                    })?;
            self.next_prepend
        } else {
            let order = self.next_append;
            self.next_append =
                self.next_append
                    .checked_add(1)
                    .ok_or(MetaError::CapacityExhausted {
                        resource: "Local listener ordering keys",
                    })?;
            order
        };
        let previous = self.bindings.insert(order, binding);
        debug_assert!(previous.is_none(), "Local listener order keys are unique");
        Ok(order)
    }

    fn remove(&mut self, order: i64) -> Option<Arc<LocalEventBinding>> {
        self.bindings.remove(&order)
    }

    fn snapshot(&self) -> Vec<Arc<LocalEventBinding>> {
        self.bindings.values().cloned().collect()
    }

    fn len(&self) -> usize {
        self.bindings.len()
    }

    fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// Cloneable generation-owned handle for one exact typed Local listener.
#[derive(Clone)]
pub struct LocalEventHandle {
    id: EventListenerId,
    ownership: EventOwnership,
}

impl LocalEventHandle {
    /// Returns the exact Runtime-local listener identity.
    pub fn id(&self) -> EventListenerId {
        self.id
    }

    /// Removes this exact listener once and joins its effect cleanup.
    pub async fn dispose(&self) -> CleanupReport {
        self.ownership.dispose().await.0
    }
}

impl fmt::Debug for LocalEventHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalEventHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    pub(super) fn add_local_listener<E, H>(
        &self,
        context: &Context,
        handler: Arc<H>,
        options: LocalEventOptions,
    ) -> Result<LocalEventHandle>
    where
        E: LocalEvent,
        H: ?Sized + Send + Sync + 'static,
    {
        let _runtime_admission = self.begin_admission(false)?;
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own a Local listener".to_owned())
        })?;
        let executor = self.owner_fiber(owner)?.executor.clone();
        let id = EventListenerId(
            self.inner
                .next_listener
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| MetaError::CapacityExhausted {
                    resource: "event listener identities",
                })?
                + 1,
        );
        let cleanup_label = diagnostics::bound_owned(
            "remove Local event listener".to_owned(),
            self.inner.limits.payloads.maximum_diagnostic_bytes,
        );
        let removal = EventRemoval::new(self, owner, id, cleanup_label.clone());
        let ownership = if let Some(setup) = context
            .setup_effect
            .as_ref()
            .filter(|setup| setup.is_open())
        {
            let effect = setup.defer_owned(cleanup_label, removal.cleanup())?;
            EventOwnership::new(Arc::clone(&removal), EventEffect::Setup(effect))
        } else {
            let mut transaction = self.begin_effect(owner, "Local event listener".to_owned())?;
            transaction.defer(cleanup_label, removal.cleanup())?;
            EventOwnership::new(
                Arc::clone(&removal),
                EventEffect::Dynamic(transaction.commit()?),
            )
        };
        // The registry must not retain a strong Runtime through the dynamic
        // effect handle; otherwise Runtime -> binding -> once closure -> Runtime
        // forms a last-owner cycle even when the public listener handle drops.
        let once_ownership = ownership.registry_clone();
        let once_executor = executor.clone();
        let claim_once: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            let Some(claim) = once_ownership.begin_once_claim() else {
                return false;
            };
            once_executor.spawn(async move {
                claim.finish().await;
            });
            true
        });
        let binding = Arc::new(LocalEventBinding::new(
            id,
            handler,
            options.once,
            claim_once,
        ));
        let slot = LocalEventSlot {
            event: TypeId::of::<E>(),
            isolation: context
                .event_isolation
                .get(&TypeId::of::<E>())
                .copied()
                .unwrap_or(LocalIsolationId(0)),
        };
        let publication = removal.publish(|| {
            let maximum_slot_listeners = (TypeId::of::<E::Mode>() == TypeId::of::<Waterfall>())
                .then_some(
                    self.inner
                        .limits
                        .topology
                        .maximum_waterfall_listeners_per_slot,
                );
            self.register_local_listener_entry(
                owner,
                id,
                slot,
                binding,
                options.prepend,
                maximum_slot_listeners,
            )
        });
        if let Err(error) = publication {
            ownership.rollback_failed_publication(&executor);
            return Err(error);
        }
        Ok(LocalEventHandle { id, ownership })
    }

    fn register_local_listener_entry(
        &self,
        owner: Owner,
        id: EventListenerId,
        slot: LocalEventSlot,
        binding: Arc<LocalEventBinding>,
        prepend: bool,
        maximum_slot_listeners: Option<usize>,
    ) -> Result<()> {
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
        Runtime::validate_live_owner_data(owner, &data)?;
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        if maximum_slot_listeners.is_some_and(|maximum| {
            state
                .local_listeners
                .get(&slot)
                .is_some_and(|listeners| listeners.len() >= maximum)
        }) {
            return Err(MetaError::CapacityExhausted {
                resource: "Waterfall listeners in one event slot",
            });
        }
        let reservation =
            self.inner
                .resources
                .listeners
                .try_reserve(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "event listeners",
                })?;
        let listeners = state.local_listeners.entry(slot).or_default();
        let order = listeners.insert(binding, prepend)?;
        active.local_listener_ids.insert(id, reservation);
        state
            .local_listener_events
            .insert(id, LocalListenerLocation { slot, order });
        state.advance_revision();
        Ok(())
    }

    pub(super) fn snapshot_local_event<E: LocalEvent>(
        &self,
        context: &Context,
    ) -> Result<LocalEventSnapshot> {
        let _runtime_admission = self.begin_admission(false)?;
        let slot = LocalEventSlot {
            event: TypeId::of::<E>(),
            isolation: context
                .event_isolation
                .get(&TypeId::of::<E>())
                .copied()
                .unwrap_or(LocalIsolationId(0)),
        };
        let bindings = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            if let Some(owner) = context.owner {
                let fiber = state
                    .fibers
                    .get(&owner.fiber)
                    .ok_or(MetaError::StaleContext {
                        fiber: owner.fiber,
                        generation: owner.generation,
                    })?;
                let data = fiber.data.lock().expect("fiber state poisoned");
                Runtime::validate_live_owner_data(owner, &data)?;
            }
            state
                .local_listeners
                .get(&slot)
                .map(LocalEventListeners::snapshot)
                .unwrap_or_default()
        };
        Ok(LocalEventSnapshot::new(self.clone(), bindings))
    }

    pub(super) fn remove_local_listener_entry(&self, owner: Owner, id: EventListenerId) -> bool {
        let removed = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let Some(location) = state.local_listener_events.get(&id).copied() else {
                return false;
            };
            let Some(fiber) = state.fibers.get(&owner.fiber).cloned() else {
                return false;
            };
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            if data.generation != owner.generation {
                return false;
            }
            let Some(active) = data.active.as_mut() else {
                return false;
            };
            if active.generation != owner.generation || !active.local_listener_ids.contains_key(&id)
            {
                return false;
            }
            let listeners = state
                .local_listeners
                .get_mut(&location.slot)
                .expect("Local listener identity retains its exact slot");
            let removed = listeners
                .remove(location.order)
                .expect("Local listener identity retains its exact binding");
            let empty = listeners.is_empty();
            if empty {
                state.local_listeners.remove(&location.slot);
            }
            active.local_listener_ids.remove(&id);
            state.local_listener_events.remove(&id);
            state.advance_revision();
            removed
        };
        drop(removed);
        true
    }
}
