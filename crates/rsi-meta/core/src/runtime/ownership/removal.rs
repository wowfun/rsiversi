use super::super::{Cleanup, EventListenerId, MetaError, Owner, Result, Runtime, RuntimeInner};
use futures_util::FutureExt as _;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Notify;

type LocalUndo = Box<dyn FnOnce() -> std::result::Result<(), String> + Send>;
enum RemovalAction {
    Listener(EventListenerId),
    Local(Mutex<Option<LocalUndo>>),
}

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

pub(crate) struct RegistrationRemoval {
    runtime: Weak<RuntimeInner>,
    owner: Owner,
    action: RemovalAction,
    admitting: AtomicBool,
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

impl RegistrationRemoval {
    pub(in crate::runtime) fn new(
        runtime: &Runtime,
        owner: Owner,
        id: EventListenerId,
        cleanup_label: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::downgrade(&runtime.inner),
            owner,
            action: RemovalAction::Listener(id),
            admitting: AtomicBool::new(true),
            cleanup_label,
            maximum_diagnostic_entries: runtime.inner.limits.payloads.maximum_diagnostic_entries,
            maximum_diagnostic_bytes: runtime.inner.limits.payloads.maximum_diagnostic_bytes,
            state: Mutex::new(RemovalState::default()),
            complete: Notify::new(),
        })
    }

    pub(in crate::runtime) fn for_local(
        runtime: &Runtime,
        owner: Owner,
        cleanup_label: String,
        undo: LocalUndo,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::downgrade(&runtime.inner),
            owner,
            action: RemovalAction::Local(Mutex::new(Some(undo))),
            admitting: AtomicBool::new(true),
            cleanup_label,
            maximum_diagnostic_entries: runtime.inner.limits.payloads.maximum_diagnostic_entries,
            maximum_diagnostic_bytes: runtime.inner.limits.payloads.maximum_diagnostic_bytes,
            state: Mutex::new(RemovalState::default()),
            complete: Notify::new(),
        })
    }

    pub(in crate::runtime) fn is_admitting(&self) -> bool {
        self.admitting.load(Ordering::Acquire)
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

impl fmt::Debug for RegistrationRemoval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrationRemoval")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}
