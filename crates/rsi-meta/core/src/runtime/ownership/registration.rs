use super::super::{
    Context, EventHandler, EventKey, EventListenerId, EventOptions, MetaError, Ordering, Result,
    Runtime, dispatch,
};
use super::{EventEffect, EventHandle, EventOwnership, EventRemoval};
use std::sync::Arc;

impl Runtime {
    pub(crate) fn add_listener(
        &self,
        context: &Context,
        event: EventKey,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
    ) -> Result<EventHandle> {
        let _runtime_admission = self.begin_admission(false)?;
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own a listener".to_owned())
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
        let maximum_diagnostic_bytes = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let transaction_label =
            dispatch::bound_owned_diagnostic("event listener".to_owned(), maximum_diagnostic_bytes);
        let cleanup_label = dispatch::bound_owned_diagnostic(
            "remove event listener".to_owned(),
            maximum_diagnostic_bytes,
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
            let mut transaction = self.begin_effect(owner, transaction_label)?;
            transaction.defer(cleanup_label, removal.cleanup())?;
            let effect = transaction.commit()?;
            EventOwnership::new(Arc::clone(&removal), EventEffect::Dynamic(effect))
        };

        let publication = removal.publish(|| {
            self.register_listener_entry(
                context,
                owner,
                id,
                event,
                handler,
                options,
                ownership.registry_clone(),
            )
        });
        if let Err(error) = publication {
            ownership.rollback_failed_publication(&executor);
            return Err(error);
        }
        Ok(EventHandle::new(id, ownership))
    }
}
