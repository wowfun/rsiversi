use super::*;
use rsi_meta::Runtime;

const TEST_MAXIMUM_SCOPED_LAYERS: usize = 4;

fn new_scope_root() -> ScopeRoot {
    ScopeRoot::new(64).unwrap()
}

#[derive(Debug)]
struct EmptyLayer;

impl ScopeLayer for EmptyLayer {
    fn is_empty(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct PanickingPayloadDrop;

impl Drop for PanickingPayloadDrop {
    fn drop(&mut self) {
        std::panic::panic_any(Self);
    }
}

#[tokio::test]
async fn peek_validates_locality_without_walking_ancestry() {
    const DEPTH: usize = 32;
    let runtime = Runtime::default();
    let root = ScopeRoot::new(DEPTH).unwrap();
    let layers = ScopedLayers::new(
        root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| EmptyLayer,
        || async { Ok(()) },
    );
    let mut scopes = Vec::with_capacity(DEPTH);
    let mut bindings = Vec::with_capacity(DEPTH - 1);
    scopes.push(root.create(&runtime.root()).await.unwrap());
    for _ in 1..DEPTH {
        let child = root.create(&runtime.root()).await.unwrap();
        bindings.push(
            root.bind_parent(child.key(), scopes.last().unwrap().key())
                .unwrap(),
        );
        scopes.push(child);
    }

    root.reset_topology_node_visits();
    assert!(layers.peek(scopes.last().unwrap().key()).unwrap().is_none());
    assert_eq!(
        root.topology_node_visits(),
        0,
        "exact-scope lookup needs only the root identity check"
    );

    drop(bindings);
    for scope in scopes.into_iter().rev() {
        assert!(scope.dispose().await.is_clean());
    }
}

#[tokio::test]
async fn failed_lazy_factory_releases_its_cell_and_scope_key() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&factory_calls);
    let layers = ScopedLayers::new(
        root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        move |_| {
            assert_eq!(
                calls.fetch_add(1, Ordering::AcqRel),
                0,
                "scoped layer factory evidence"
            );
            EmptyLayer
        },
        || async { Ok(()) },
    );
    let scope = root.create(&runtime.root()).await.unwrap();
    let node = Arc::downgrade(&scope.key().node);

    let error = layers
        .effect(
            scope.context(),
            "failed scoped layer factory",
            |_| -> Result<ScopeUndo, &'static str> {
                panic!("the action cannot run after factory failure");
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.primary(), "scope layer factory panicked");
    assert_eq!(
        layers
            .inner
            .scoped
            .lock()
            .expect("scoped layers poisoned")
            .len(),
        0,
        "a failed lazy materialization has no retained scope slot"
    );

    assert!(scope.dispose().await.is_clean());
    drop(scope);
    assert!(
        node.upgrade().is_none(),
        "a failed lazy materialization cannot retain its ScopeKey node"
    );
}

#[tokio::test]
async fn failed_lazy_factory_payload_destruction_is_contained_and_releases_its_cell() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&factory_calls);
    let layers = ScopedLayers::new(
        root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        move |_| match calls.fetch_add(1, Ordering::AcqRel) {
            0 => EmptyLayer,
            _ => std::panic::panic_any(PanickingPayloadDrop),
        },
        || async { Ok(()) },
    );
    let scope = root.create(&runtime.root()).await.unwrap();

    let outcome = AssertUnwindSafe(layers.effect(
        scope.context(),
        "failed scoped layer factory payload",
        |_| -> Result<ScopeUndo, &'static str> {
            panic!("the action cannot run after factory failure");
        },
    ))
    .catch_unwind()
    .await;
    let error = outcome
        .expect("factory panic payload destruction must remain contained")
        .unwrap_err();
    assert_eq!(
        error.primary(),
        "scope layer factory panic payload destruction panicked"
    );
    assert!(
        layers
            .inner
            .scoped
            .lock()
            .expect("scoped layers poisoned")
            .is_empty()
    );

    let _cleanup = scope.dispose().await;
}

#[tokio::test]
async fn exhausted_layer_version_never_wraps_and_permanently_fences_reclamation() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let layers = ScopedLayers::new(
        root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| EmptyLayer,
        || async { Ok(()) },
    );
    let scope = root.create(&runtime.root()).await.unwrap();
    let Ok((Some(cell), slot, mutation)) = layers.begin_mutation(Some(scope.key())).await else {
        panic!("exact scope mutation was not admitted");
    };
    slot.version.store(u64::MAX - 1, Ordering::Release);
    drop(mutation);
    assert_eq!(slot.version.load(Ordering::Acquire), u64::MAX);

    let Ok((Some(second_cell), second_slot, mutation)) =
        layers.begin_mutation(Some(scope.key())).await
    else {
        panic!("version exhaustion rejected an otherwise bounded mutation");
    };
    assert!(Arc::ptr_eq(&cell, &second_cell));
    assert!(Arc::ptr_eq(&slot, &second_slot));
    assert_eq!(
        slot.version.load(Ordering::Acquire),
        u64::MAX,
        "the reclamation ABA fence wrapped after exhaustion"
    );
    drop(mutation);
    assert_eq!(slot.active_mutations.load(Ordering::Acquire), 0);
    assert_eq!(slot.version.load(Ordering::Acquire), u64::MAX);

    layers.inner.try_reclaim(scope.key(), &cell, &slot);
    assert!(
        layers.peek(scope.key()).unwrap().is_some(),
        "an exhausted version must permanently fail closed"
    );
    let _cleanup = scope.dispose().await;
}
