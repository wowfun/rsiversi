use super::super::{Cleanup, EventListenerId, MetaError, Owner, Result, Runtime, RuntimeInner};
use futures_util::FutureExt as _;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Notify;

type RemovalResult = std::result::Result<bool, String>;

mod report;
mod start;

#[derive(Default)]
struct RemovalState {
    started: bool,
    detached_report: bool,
    report_retained: bool,
    result: Option<RemovalResult>,
}

pub(crate) struct EventRemoval {
    runtime: Weak<RuntimeInner>,
    owner: Owner,
    id: EventListenerId,
    cleanup_label: String,
    maximum_diagnostic_entries: usize,
    maximum_diagnostic_bytes: usize,
    // This one-shot state intentionally recovers poison: a panic while publishing
    // removal must still converge to a terminal result and wake every joiner.
    // Global Runtime and Fiber registry mutexes instead fail on poison because
    // their cross-registry invariants cannot be reconstructed locally.
    state: Mutex<RemovalState>,
    complete: Notify,
}

impl EventRemoval {
    pub(in crate::runtime) fn new(
        runtime: &Runtime,
        owner: Owner,
        id: EventListenerId,
        cleanup_label: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::downgrade(&runtime.inner),
            owner,
            id,
            cleanup_label,
            maximum_diagnostic_entries: runtime.inner.limits.payloads.maximum_diagnostic_entries,
            maximum_diagnostic_bytes: runtime.inner.limits.payloads.maximum_diagnostic_bytes,
            state: Mutex::new(RemovalState::default()),
            complete: Notify::new(),
        })
    }

    pub(super) fn owner(&self) -> Owner {
        self.owner
    }

    pub(in crate::runtime) fn publish<T>(
        &self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.started {
            return Err(MetaError::StaleContext {
                fiber: self.owner.fiber,
                generation: self.owner.generation,
            });
        }
        let result = operation();
        drop(state);
        result
    }

    pub(in crate::runtime) fn cleanup(self: &Arc<Self>) -> Cleanup {
        let removal = Arc::clone(self);
        Box::new(move || {
            async move {
                removal.start();
                removal.join().await.map(|_| ())
            }
            .boxed()
        })
    }

    pub(super) async fn join(&self) -> RemovalResult {
        loop {
            let notified = self.complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .result
                .clone()
            {
                return result;
            }
            notified.as_mut().await;
        }
    }
}

impl fmt::Debug for EventRemoval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventRemoval")
            .field("owner", &self.owner)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
