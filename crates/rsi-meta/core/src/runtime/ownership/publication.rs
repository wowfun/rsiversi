use super::super::{
    Context, ContextScope, EventHandler, EventKey, EventListenerId, EventOptions, FiberState,
    ListenerBinding, MetaError, Owner, Result, Runtime,
};
use super::EventOwnership;
use std::sync::Arc;

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn register_listener_entry(
        &self,
        context: &Context,
        owner: Owner,
        id: EventListenerId,
        event: EventKey,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
        ownership: EventOwnership,
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
        if data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
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
            owner: owner.fiber,
            generation: owner.generation,
            scope: ContextScope {
                isolation: Arc::clone(&context.isolation),
                intercepts: Arc::clone(&context.intercepts),
                extensions: Arc::clone(&context.extensions),
                entries: context.entries,
                encoded_bytes: context.encoded_bytes,
                trace: context.trace.clone(),
            },
            handler,
            options,
            lease: Arc::clone(&active.lease),
            ownership,
        });
        state.listener_events.insert(id, event.clone());
        state.listeners.entry(event).or_default().insert(listener);
        state.advance_revision();
        Ok(())
    }
}
