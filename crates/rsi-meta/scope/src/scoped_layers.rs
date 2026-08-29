use crate::store::ScopeUndoCapture;
use crate::{NamedEntries, ScopeError, ScopeKey, ScopeRoot, ScopeUndo, ScopedContext};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use rsi_meta::{Context, EffectHandle};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

mod sealed {
    pub trait Sealed {}
}

/// Sealed explicit selector for a global or scoped layer mutation.
pub trait LayerContext: sealed::Sealed {
    /// Returns the Meta Context that owns the mutation effect.
    #[doc(hidden)]
    fn meta_context(&self) -> &Context;

    /// Returns the explicit scope selection, or `None` for the global layer.
    #[doc(hidden)]
    fn selected_scope(&self) -> Option<&ScopeKey>;
}

impl sealed::Sealed for Context {}

impl LayerContext for Context {
    fn meta_context(&self) -> &Context {
        self
    }

    fn selected_scope(&self) -> Option<&ScopeKey> {
        None
    }
}

impl sealed::Sealed for ScopedContext {}

impl LayerContext for ScopedContext {
    fn meta_context(&self) -> &Context {
        self.meta()
    }

    fn selected_scope(&self) -> Option<&ScopeKey> {
        Some(self.scope())
    }
}

mod change;
mod cleanup;
mod diagnostics;
mod panic_boundary;
#[cfg(test)]
mod tests;

use change::run_change_future;
use cleanup::{CleanupPlan, scoped_cleanup};
use diagnostics::BoundedDiagnostic;
pub use diagnostics::MutationError;
use panic_boundary::{caught_panic, drop_caught_payload};

type ChangeFuture = BoxFuture<'static, Result<(), String>>;
type ChangeCallback = dyn Fn() -> ChangeFuture + Send + Sync;
type LayerFactory<L> = dyn Fn(Option<ScopeKey>) -> L + Send + Sync;
type ScopedCell<L> = OnceCell<Arc<LayerSlot<L>>>;
type SelectedMutation<L> = (
    Option<Arc<ScopedCell<L>>>,
    Arc<LayerSlot<L>>,
    LayerMutation<L>,
);

/// Aggregate layer contract used solely for exact empty-layer reclamation.
pub trait ScopeLayer: Send + Sync + 'static {
    /// Returns whether every product-owned table in this aggregate is empty.
    fn is_empty(&self) -> bool;
}

struct LayerSlot<L> {
    layer: Arc<L>,
    active_mutations: AtomicUsize,
    version: AtomicU64,
}

impl<L> LayerSlot<L> {
    fn new(layer: L) -> Self {
        Self {
            layer: Arc::new(layer),
            active_mutations: AtomicUsize::new(0),
            version: AtomicU64::new(0),
        }
    }

    fn begin(self: &Arc<Self>) -> LayerMutation<L> {
        self.active_mutations.fetch_add(1, Ordering::AcqRel);
        self.advance_version();
        LayerMutation {
            slot: Arc::clone(self),
        }
    }

    fn advance_version(&self) {
        let _ = self
            .version
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |version| {
                version.checked_add(1)
            });
    }
}

struct LayerMutation<L> {
    slot: Arc<LayerSlot<L>>,
}

impl<L> Drop for LayerMutation<L> {
    fn drop(&mut self) {
        self.slot.advance_version();
        let previous = self.slot.active_mutations.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "scope layer mutation count underflow");
    }
}

enum LayerSelectionFailure {
    Capacity,
    FactoryPanic { payload_destruction_panicked: bool },
}

impl LayerSelectionFailure {
    fn message(&self) -> &'static str {
        match self {
            Self::Capacity => "scoped layer capacity exhausted",
            Self::FactoryPanic {
                payload_destruction_panicked: true,
            } => "scope layer factory panic payload destruction panicked",
            Self::FactoryPanic {
                payload_destruction_panicked: false,
            } => "scope layer factory panicked",
        }
    }
}

struct ScopedLayersInner<L: ScopeLayer> {
    root: ScopeRoot,
    global: Arc<LayerSlot<L>>,
    scoped: Mutex<HashMap<ScopeKey, Arc<ScopedCell<L>>>>,
    maximum_scoped_layers: usize,
    create_layer: Arc<LayerFactory<L>>,
    on_change: Arc<ChangeCallback>,
}

impl<L: ScopeLayer> ScopedLayersInner<L> {
    async fn scoped_mutation(
        self: &Arc<Self>,
        key: ScopeKey,
    ) -> Result<(Arc<ScopedCell<L>>, Arc<LayerSlot<L>>, LayerMutation<L>), LayerSelectionFailure>
    {
        loop {
            let cell = {
                let mut scoped = self.scoped.lock().expect("scoped layers poisoned");
                if let Some(cell) = scoped.get(&key) {
                    Arc::clone(cell)
                } else {
                    if scoped.len() >= self.maximum_scoped_layers {
                        return Err(LayerSelectionFailure::Capacity);
                    }
                    let cell = Arc::new(OnceCell::new());
                    scoped.insert(key.clone(), Arc::clone(&cell));
                    cell
                }
            };
            let create_layer = Arc::clone(&self.create_layer);
            let selected = key.clone();
            let initialized = AssertUnwindSafe(cell.get_or_init(|| async move {
                Arc::new(LayerSlot::new(create_layer(Some(selected))))
            }))
            .catch_unwind()
            .await;
            let slot = match initialized {
                Ok(slot) => Arc::clone(slot),
                Err(payload) => {
                    self.discard_uninitialized(&key, &cell);
                    return Err(LayerSelectionFailure::FactoryPanic {
                        payload_destruction_panicked: drop_caught_payload(payload),
                    });
                }
            };
            let mutation = {
                let scoped = self.scoped.lock().expect("scoped layers poisoned");
                let current = scoped.get(&key);
                if current.is_some_and(|current| Arc::ptr_eq(current, &cell)) {
                    Some(slot.begin())
                } else {
                    None
                }
            };
            if let Some(mutation) = mutation {
                return Ok((cell, slot, mutation));
            }
        }
    }

    fn discard_uninitialized(&self, key: &ScopeKey, cell: &Arc<ScopedCell<L>>) {
        let mut scoped = self.scoped.lock().expect("scoped layers poisoned");
        let exact_uninitialized = cell.get().is_none()
            && scoped
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, cell));
        if exact_uninitialized {
            scoped.remove(key);
        }
    }

    fn try_reclaim(&self, key: &ScopeKey, cell: &Arc<ScopedCell<L>>, slot: &Arc<LayerSlot<L>>) {
        if slot.active_mutations.load(Ordering::Acquire) != 0 {
            return;
        }
        let version = slot.version.load(Ordering::Acquire);
        if version == u64::MAX {
            return;
        }
        if !slot.layer.is_empty() {
            return;
        }
        let mut scoped = self.scoped.lock().expect("scoped layers poisoned");
        let unchanged = slot.active_mutations.load(Ordering::Acquire) == 0
            && slot.version.load(Ordering::Acquire) == version;
        let same = scoped.get(key).is_some_and(|current| {
            Arc::ptr_eq(current, cell)
                && current
                    .get()
                    .is_some_and(|current_slot| Arc::ptr_eq(current_slot, slot))
        });
        if unchanged && same {
            scoped.remove(key);
        }
    }

    async fn notify(&self, maximum: usize) -> Result<(), BoundedDiagnostic> {
        let future = match std::panic::catch_unwind(AssertUnwindSafe(|| (self.on_change)())) {
            Ok(future) => future,
            Err(payload) => {
                return Err(caught_panic(
                    payload,
                    "scope change callback panicked",
                    "scope change callback panic payload destruction panicked",
                    maximum,
                ));
            }
        };
        run_change_future(future, maximum).await
    }
}

/// Scope-aware aggregate layers with Context-derived effect ownership.
///
/// This is a product-composed storage primitive, not a Runtime registry. Its
/// global layer is eager; exact scoped layers are lazy and reclaimed only when
/// the aggregate reports every product table empty.
pub struct ScopedLayers<L: ScopeLayer> {
    inner: Arc<ScopedLayersInner<L>>,
}

impl<L: ScopeLayer> Clone for ScopedLayers<L> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<L: ScopeLayer> ScopedLayers<L> {
    /// Creates one product-owned layered store.
    ///
    /// The factory is called eagerly once for the global layer and lazily for
    /// scoped aggregates. `maximum_scoped_layers` bounds simultaneously
    /// retained exact-scope aggregates; zero creates a global-only store.
    /// Neither the factory nor the change callback runs under a scope-store
    /// lock.
    pub fn new<C, N, F>(
        root: ScopeRoot,
        maximum_scoped_layers: usize,
        create_layer: C,
        on_change: N,
    ) -> Self
    where
        C: Fn(Option<ScopeKey>) -> L + Send + Sync + 'static,
        N: Fn() -> F + Send + Sync + 'static,
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let create_layer: Arc<LayerFactory<L>> = Arc::new(create_layer);
        let global = Arc::new(LayerSlot::new(create_layer(None)));
        let on_change: Arc<ChangeCallback> = Arc::new(move || Box::pin(on_change()));
        Self {
            inner: Arc::new(ScopedLayersInner {
                root,
                global,
                scoped: Mutex::new(HashMap::new()),
                maximum_scoped_layers,
                create_layer,
                on_change,
            }),
        }
    }

    /// Returns the eagerly created global aggregate.
    pub fn global(&self) -> Arc<L> {
        Arc::clone(&self.inner.global.layer)
    }

    /// Returns an existing exact scoped aggregate without creating one.
    pub fn peek(&self, scope: &ScopeKey) -> Result<Option<Arc<L>>, ScopeError> {
        self.inner.root.ensure_local(scope)?;
        let cell = self
            .inner
            .scoped
            .lock()
            .expect("scoped layers poisoned")
            .get(scope)
            .cloned();
        Ok(cell
            .as_ref()
            .and_then(|cell| cell.get())
            .map(|slot| Arc::clone(&slot.layer)))
    }

    /// Returns existing overlays farthest ancestor first and exact scope last.
    pub fn chain_layers(&self, scope: &ScopeKey) -> Result<Vec<Arc<L>>, ScopeError> {
        let mut chain = self.inner.root.chain(scope)?;
        chain.reverse();
        let cells = {
            let scoped = self.inner.scoped.lock().expect("scoped layers poisoned");
            chain
                .into_iter()
                .filter_map(|key| scoped.get(&key).cloned())
                .collect::<Vec<_>>()
        };
        Ok(cells
            .into_iter()
            .filter_map(|cell| cell.get().map(|slot| Arc::clone(&slot.layer)))
            .collect())
    }

    /// Materializes an owned effective named view.
    ///
    /// Global values are applied first, followed by overlays from farthest to
    /// nearest. A nearer value replaces the same name in its original position.
    pub fn effective_named<V, P>(
        &self,
        scope: Option<&ScopeKey>,
        pick: P,
    ) -> Result<Vec<(String, V)>, ScopeError>
    where
        V: Clone + Send + Sync + 'static,
        P: for<'a> Fn(&'a L) -> &'a NamedEntries<V>,
    {
        let mut merged = pick(&self.inner.global.layer).shared_snapshot();
        let mut positions = merged
            .iter()
            .enumerate()
            .map(|(index, (name, _))| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        let overlays = match scope {
            Some(scope) => self.chain_layers(scope)?,
            None => Vec::new(),
        };
        for layer in overlays {
            for (name, value) in pick(&layer).shared_snapshot() {
                if let Some(index) = positions.get(&name).copied() {
                    merged[index].1 = value;
                } else {
                    positions.insert(name.clone(), merged.len());
                    merged.push((name, value));
                }
            }
        }
        Ok(NamedEntries::into_owned_snapshot(merged))
    }

    /// Makes one layer mutation visible and attaches its exact undo to `context`.
    ///
    /// Built-in entry mutations publish their undo to this action transaction
    /// before returning, so a later action error or panic still rolls them back.
    ///
    /// The Context selects both scope visibility and generation effect owner.
    /// Initial notification runs after visibility. Its failure joins exact undo
    /// and one compensating notification before returning [`MutationError`].
    pub async fn effect<C, A, E>(
        &self,
        context: &C,
        label: impl Into<String>,
        action: A,
    ) -> Result<EffectHandle, MutationError>
    where
        C: LayerContext + ?Sized,
        A: FnOnce(&L) -> Result<ScopeUndo, E>,
        E: fmt::Display,
    {
        let meta = context.meta_context();
        let maximum = meta.runtime().limits().payloads.maximum_diagnostic_bytes;
        let label = label.into();
        let mut transaction = meta
            .begin_effect(label.clone())
            .map_err(|error| MutationError::from_primary(error, maximum))?;
        let scope = context.selected_scope().cloned();
        if let Some(scope) = &scope {
            self.inner
                .root
                .ensure_local(scope)
                .map_err(|error| MutationError::from_primary(error, maximum))?;
        }

        let (cell, slot, mutation) = match self.begin_mutation(scope.as_ref()).await {
            Ok(selected) => selected,
            Err(failure) => {
                let cleanup = transaction.abort().await;
                return Err(MutationError::new(
                    BoundedDiagnostic::from_string(failure.message().to_owned(), maximum),
                    cleanup,
                    None,
                ));
            }
        };

        let plan = Arc::new(CleanupPlan::new());
        let cleanup = scoped_cleanup(
            Arc::clone(&self.inner),
            scope.clone(),
            cell.clone(),
            Arc::clone(&slot),
            Arc::clone(&plan),
            maximum,
        );
        if let Err(error) = transaction.defer(label, cleanup) {
            drop(mutation);
            self.reclaim_after_setup(scope.as_ref(), cell.as_ref(), &slot);
            let report = transaction.abort().await;
            return Err(MutationError::new(
                BoundedDiagnostic::from_display(error, maximum),
                report,
                None,
            ));
        }

        let undo_capture = ScopeUndoCapture::begin();
        let action = std::panic::catch_unwind(AssertUnwindSafe(|| action(&slot.layer)));
        let undo = match action {
            Ok(Ok(undo)) => {
                let mut undos = undo_capture.finish();
                if !undos.iter().any(|captured| captured.same_action(&undo)) {
                    undos.push(undo.clone());
                }
                plan.replace_undos(undos);
                undo
            }
            Ok(Err(error)) => {
                plan.replace_undos(undo_capture.finish());
                drop(mutation);
                self.reclaim_after_setup(scope.as_ref(), cell.as_ref(), &slot);
                let cleanup = transaction.abort().await;
                return Err(MutationError::new(
                    BoundedDiagnostic::from_display(error, maximum),
                    cleanup,
                    None,
                ));
            }
            Err(payload) => {
                let primary = caught_panic(
                    payload,
                    "scope layer action panicked",
                    "scope layer action panic payload destruction panicked",
                    maximum,
                );
                plan.replace_undos(undo_capture.finish());
                drop(mutation);
                self.reclaim_after_setup(scope.as_ref(), cell.as_ref(), &slot);
                let cleanup = transaction.abort().await;
                return Err(MutationError::new(primary, cleanup, None));
            }
        };
        drop(undo);
        drop(mutation);

        if let Err(primary) = self.inner.notify(maximum).await {
            plan.begin_compensation();
            let cleanup = transaction.abort().await;
            let compensation = plan.compensation();
            return Err(MutationError::new(primary, cleanup, compensation));
        }

        transaction
            .commit()
            .map_err(|error| MutationError::from_primary(error, maximum))
    }

    async fn begin_mutation(
        &self,
        scope: Option<&ScopeKey>,
    ) -> Result<SelectedMutation<L>, LayerSelectionFailure> {
        let Some(key) = scope else {
            let slot = Arc::clone(&self.inner.global);
            let mutation = slot.begin();
            return Ok((None, slot, mutation));
        };
        let inner = Arc::clone(&self.inner);
        let creation = AssertUnwindSafe(inner.scoped_mutation(key.clone()))
            .catch_unwind()
            .await;
        let creation = match creation {
            Ok(creation) => creation,
            Err(payload) => Err(LayerSelectionFailure::FactoryPanic {
                payload_destruction_panicked: drop_caught_payload(payload),
            }),
        };
        creation.map(|(cell, slot, mutation)| (Some(cell), slot, mutation))
    }

    fn reclaim_after_setup(
        &self,
        scope: Option<&ScopeKey>,
        cell: Option<&Arc<ScopedCell<L>>>,
        slot: &Arc<LayerSlot<L>>,
    ) {
        if let (Some(scope), Some(cell)) = (scope, cell)
            && let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(|| {
                self.inner.try_reclaim(scope, cell, slot);
            }))
        {
            let _payload_destruction_panicked = drop_caught_payload(payload);
        }
    }
}

impl<L: ScopeLayer> fmt::Debug for ScopedLayers<L> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scoped = self
            .inner
            .scoped
            .lock()
            .expect("scoped layers poisoned")
            .len();
        formatter
            .debug_struct("ScopedLayers")
            .field("scoped_slots", &scoped)
            .field("maximum_scoped_layers", &self.inner.maximum_scoped_layers)
            .finish_non_exhaustive()
    }
}
