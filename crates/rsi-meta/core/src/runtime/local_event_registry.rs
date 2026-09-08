#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::composition_order::OrderView;
use super::ownership::RegistrationRemoval;
use super::*;
use crate::Waterfall;
use crate::local_events::LocalEventBindings;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct LocalEventSlot {
    event: TypeId,
    isolation: LocalIsolationId,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LocalListenerLocation {
    slot: LocalEventSlot,
}

struct PositionedListener {
    binding: Arc<LocalEventBinding>,
    position: ChildPosition,
    prepend: bool,
}

#[derive(Default)]
pub(super) struct LocalEventListeners {
    bindings: BTreeMap<EventListenerId, PositionedListener>,
    snapshot: Option<(Arc<()>, Arc<LocalEventBindings>)>,
}

impl LocalEventListeners {
    fn insert(
        &mut self,
        id: EventListenerId,
        binding: Arc<LocalEventBinding>,
        position: ChildPosition,
        prepend: bool,
    ) {
        // Every old snapshot member remains in the registry here; releasing the
        // cached Arc cannot run a final plugin destructor under the registry lock.
        self.snapshot = None;
        self.bindings.insert(
            id,
            PositionedListener {
                binding,
                position,
                prepend,
            },
        );
    }

    fn remove(
        &mut self,
        id: EventListenerId,
    ) -> Option<(PositionedListener, Option<Arc<LocalEventBindings>>)> {
        let removed = self.bindings.remove(&id)?;
        Some((removed, self.snapshot.take().map(|(_, bindings)| bindings)))
    }

    fn snapshot(&mut self, runtime: &Runtime, order: &OrderView<'_>) -> Arc<LocalEventBindings> {
        if let Some((revision, snapshot)) = &self.snapshot
            && Arc::ptr_eq(revision, order.revision())
        {
            return snapshot.clone();
        }
        let mut entries: Vec<_> = self
            .bindings
            .iter()
            .map(|(id, entry)| {
                (
                    entry.prepend,
                    order.key(&entry.position),
                    *id,
                    &entry.binding,
                )
            })
            .collect();
        entries.sort_by(|left, right| {
            right.0.cmp(&left.0).then_with(|| {
                let declaration = left.1.cmp(&right.1).then(left.2.cmp(&right.2));
                if left.0 {
                    declaration.reverse()
                } else {
                    declaration
                }
            })
        });
        let bindings: Vec<_> = entries
            .into_iter()
            .map(|(_, _, _, binding)| binding.clone())
            .collect();
        let snapshot = if let Some((_, previous)) = &self.snapshot
            && previous
                .bindings
                .iter()
                .zip(&bindings)
                .all(|(left, right)| Arc::ptr_eq(left, right))
            && previous.bindings.len() == bindings.len()
        {
            previous.clone()
        } else {
            runtime.local_event_bindings(bindings)
        };
        self.snapshot = Some((order.revision().clone(), snapshot.clone()));
        snapshot
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
    ownership: RegistrationOwnership,
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
    fn local_event_bindings(
        &self,
        bindings: Vec<Arc<LocalEventBinding>>,
    ) -> Arc<LocalEventBindings> {
        let runtime = Arc::downgrade(&self.inner);
        Arc::new(LocalEventBindings {
            bindings,
            on_drop_panic: Box::new(move || {
                if let Some(inner) = runtime.upgrade() {
                    Runtime { inner }
                        .mark_terminal_owned("Local event listener destructor panicked");
                }
            }),
        })
    }

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
        let removal = RegistrationRemoval::new(self, owner, id, cleanup_label.clone());
        let ownership = context.own_registration(&removal, cleanup_label)?;
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
        let position = data
            .position
            .as_ref()
            .expect("live Fiber occupies a position")
            .position
            .clone();
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
        listeners.insert(id, binding, position, prepend);
        active.local_listener_ids.insert(id, reservation);
        state
            .local_listener_events
            .insert(id, LocalListenerLocation { slot });
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
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
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
            self.inner.composition_order.snapshot(|order| {
                state.local_listeners.get_mut(&slot).map_or_else(
                    || {
                        self.inner
                            .empty_local_event_bindings
                            .get_or_init(|| self.local_event_bindings(Vec::new()))
                            .clone()
                    },
                    |listeners| listeners.snapshot(self, order),
                )
            })
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
                .remove(id)
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
