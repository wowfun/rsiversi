use super::diagnostics::BoundedDiagnostic;
use super::panic_boundary::caught_panic;
use super::{LayerSlot, ScopeLayer, ScopedCell, ScopedLayersInner};
use crate::{ScopeKey, ScopeUndo};
use rsi_meta::Cleanup;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(super) struct CleanupPlan {
    undos: Mutex<Vec<ScopeUndo>>,
    compensating: AtomicBool,
    compensation: Mutex<Option<BoundedDiagnostic>>,
}

impl CleanupPlan {
    pub(super) fn new() -> Self {
        Self {
            undos: Mutex::new(Vec::new()),
            compensating: AtomicBool::new(false),
            compensation: Mutex::new(None),
        }
    }

    pub(super) fn replace_undos(&self, undos: Vec<ScopeUndo>) {
        *self.undos.lock().expect("scope cleanup plan poisoned") = undos;
    }

    pub(super) fn begin_compensation(&self) {
        self.compensating.store(true, Ordering::Release);
    }

    pub(super) fn compensation(&self) -> Option<BoundedDiagnostic> {
        self.compensation
            .lock()
            .expect("scope cleanup plan poisoned")
            .clone()
    }
}

pub(super) fn scoped_cleanup<L: ScopeLayer>(
    inner: Arc<ScopedLayersInner<L>>,
    scope: Option<ScopeKey>,
    cell: Option<Arc<ScopedCell<L>>>,
    slot: Arc<LayerSlot<L>>,
    plan: Arc<CleanupPlan>,
    maximum: usize,
) -> Cleanup {
    Box::new(move || {
        Box::pin(async move {
            let undos =
                std::mem::take(&mut *plan.undos.lock().expect("scope cleanup plan poisoned"));
            if undos.is_empty() {
                return Ok(());
            }

            let mutation = slot.begin();
            let undo_failures = undos
                .into_iter()
                .rev()
                .filter_map(|undo| {
                    match std::panic::catch_unwind(AssertUnwindSafe(|| undo.run())) {
                        Ok(()) => None,
                        Err(payload) => Some(caught_panic(
                            payload,
                            "scope entry undo panicked",
                            "scope entry undo panic payload destruction panicked",
                            maximum,
                        )),
                    }
                })
                .collect::<Vec<_>>();
            drop(mutation);

            let reclaim_failure = match (scope.as_ref(), cell.as_ref()) {
                (Some(scope), Some(cell)) => {
                    match std::panic::catch_unwind(AssertUnwindSafe(|| {
                        inner.try_reclaim(scope, cell, &slot);
                    })) {
                        Ok(()) => None,
                        Err(payload) => Some(caught_panic(
                            payload,
                            "scope layer reclamation panicked",
                            "scope layer reclamation panic payload destruction panicked",
                            maximum,
                        )),
                    }
                }
                _ => None,
            };

            let notification = inner.notify(maximum).await.err();
            if plan.compensating.load(Ordering::Acquire) {
                plan.compensation
                    .lock()
                    .expect("scope cleanup plan poisoned")
                    .clone_from(&notification);
            }

            let failures = undo_failures
                .into_iter()
                .chain(reclaim_failure)
                .chain(notification)
                .map(|failure| failure.message)
                .collect::<Vec<_>>();
            if failures.is_empty() {
                Ok(())
            } else {
                let joined = failures.join("; ");
                Err(BoundedDiagnostic::from_string(joined, maximum).message)
            }
        })
    })
}
