use crate::runtime::ContextScope;
use crate::service::AdmissionLease;
use crate::{EventHandler, EventKey, EventListenerId, EventOptions, FiberGeneration, FiberId};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct ListenerBinding {
    pub id: EventListenerId,
    pub event: EventKey,
    pub owner: FiberId,
    pub generation: FiberGeneration,
    pub scope: ContextScope,
    pub handler: Arc<dyn EventHandler>,
    pub options: EventOptions,
    pub lease: Arc<AdmissionLease>,
}

/// Ordered per-event storage with logarithmic identity removal.
///
/// Prepend listeners run newest-first; ordinary listeners run oldest-first.
/// Splitting those order domains keeps claiming independent of vector shifts.
#[derive(Default)]
pub(crate) struct ListenerRegistry {
    prepended: BTreeMap<EventListenerId, Arc<ListenerBinding>>,
    appended: BTreeMap<EventListenerId, Arc<ListenerBinding>>,
}

impl ListenerRegistry {
    pub(crate) fn is_empty(&self) -> bool {
        self.prepended.is_empty() && self.appended.is_empty()
    }

    pub(crate) fn insert(&mut self, listener: Arc<ListenerBinding>) {
        if listener.options.prepend {
            self.prepended.insert(listener.id, listener);
        } else {
            self.appended.insert(listener.id, listener);
        }
    }

    pub(crate) fn get(&self, id: EventListenerId) -> Option<&Arc<ListenerBinding>> {
        self.prepended.get(&id).or_else(|| self.appended.get(&id))
    }

    pub(crate) fn remove(&mut self, id: EventListenerId) -> Option<Arc<ListenerBinding>> {
        self.prepended
            .remove(&id)
            .or_else(|| self.appended.remove(&id))
    }

    pub(crate) fn snapshot(&self) -> Vec<Arc<ListenerBinding>> {
        self.prepended
            .values()
            .rev()
            .chain(self.appended.values())
            .cloned()
            .collect()
    }
}
