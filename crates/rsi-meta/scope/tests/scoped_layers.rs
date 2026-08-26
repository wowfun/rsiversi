use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FactoryIdentity, FiberHandle, MetaError, PluginFactory,
    PreparedActivation, Result, Runtime,
};
use rsi_meta_scope::{
    AnonymousEntries, MutationError, NamedEntries, ScopeKey, ScopeLayer, ScopeRoot, ScopedLayers,
};
use serde_json::Value;
use std::future::{Future, Ready};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use tokio::sync::{Barrier, oneshot};
use tokio_util::sync::CancellationToken;

const TEST_MAXIMUM_SCOPED_LAYERS: usize = 128;

fn new_scope_root() -> ScopeRoot {
    ScopeRoot::new(64).unwrap()
}

#[derive(Debug)]
struct TestLayer {
    named: NamedEntries<i32>,
    anonymous: AnonymousEntries<String>,
}

impl TestLayer {
    fn new() -> Self {
        Self {
            named: NamedEntries::new(),
            anonymous: AnonymousEntries::new(),
        }
    }
}

impl ScopeLayer for TestLayer {
    fn is_empty(&self) -> bool {
        self.named.is_empty() && self.anonymous.is_empty()
    }
}

#[derive(Debug)]
struct CloneProbe {
    marker: u8,
    clones: Arc<AtomicUsize>,
}

impl Clone for CloneProbe {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            marker: self.marker,
            clones: Arc::clone(&self.clones),
        }
    }
}

#[derive(Debug)]
struct CloneLayer {
    named: NamedEntries<CloneProbe>,
}

impl ScopeLayer for CloneLayer {
    fn is_empty(&self) -> bool {
        self.named.is_empty()
    }
}

#[derive(Debug)]
struct CaptureFactory {
    identity: FactoryIdentity,
    sender: Mutex<Option<oneshot::Sender<Context>>>,
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(plan.context().clone());
        }
        Ok(())
    }
}

async fn active_unscoped_context(runtime: &Runtime) -> (FiberHandle, Context) {
    let (sender, receiver) = oneshot::channel();
    let factory = Arc::new(CaptureFactory {
        identity: FactoryIdentity::builtin("scope-test.owner", "1"),
        sender: Mutex::new(Some(sender)),
    });
    let fiber = runtime.root().apply(factory, Value::Null).await.unwrap();
    fiber.wait_active(&CancellationToken::new()).await.unwrap();
    (fiber, receiver.await.unwrap())
}

#[tokio::test]
async fn global_is_eager_scope_layers_are_lazy_and_empty_aggregates_are_reclaimed() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let created = Arc::new(Mutex::new(Vec::<Option<ScopeKey>>::new()));
    let created_for_factory = Arc::clone(&created);
    let layers = ScopedLayers::new(
        scope_root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        move |scope| {
            created_for_factory.lock().unwrap().push(scope);
            TestLayer::new()
        },
        || async { Ok(()) },
    );
    assert_eq!(created.lock().unwrap().len(), 1);

    let scope = scope_root.create(&runtime.root()).await.unwrap();
    assert!(layers.peek(scope.key()).unwrap().is_none());
    assert_eq!(created.lock().unwrap().len(), 1);

    let named = layers
        .effect(scope.context(), "scope-test.named", |layer| {
            layer
                .named
                .insert("kept", 1)
                .map_err(|_| "duplicate named entry")
        })
        .await
        .unwrap();
    let anonymous = layers
        .effect(scope.context(), "scope-test.anonymous", |layer| {
            Ok::<_, &'static str>(layer.anonymous.append("kept".to_owned()))
        })
        .await
        .unwrap();
    assert_eq!(created.lock().unwrap().len(), 2);
    assert!(layers.peek(scope.key()).unwrap().is_some());

    assert!(named.dispose().await.is_clean());
    assert!(layers.peek(scope.key()).unwrap().is_some());
    assert!(anonymous.dispose().await.is_clean());
    assert!(layers.peek(scope.key()).unwrap().is_none());
    assert!(scope.dispose().await.is_clean());
}

#[tokio::test]
async fn effective_named_snapshot_layers_global_then_farthest_to_nearest_without_moving_shadows() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let layers = ScopedLayers::new(
        scope_root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || async { Ok(()) },
    );
    let (owner, context) = active_unscoped_context(&runtime).await;
    let global_a = layers
        .effect(&context, "scope-test.global-a", |layer| {
            layer.named.insert("a", 1).map_err(|_| "duplicate")
        })
        .await
        .unwrap();
    let global_shared = layers
        .effect(&context, "scope-test.global-shared", |layer| {
            layer.named.insert("shared", 2).map_err(|_| "duplicate")
        })
        .await
        .unwrap();
    let parent = scope_root.create(&runtime.root()).await.unwrap();
    let (child, _binding) = scope_root
        .create_child(&runtime.root(), parent.key())
        .await
        .unwrap();
    let parent_shared = layers
        .effect(parent.context(), "scope-test.parent", |layer| {
            layer.named.insert("shared", 3).map_err(|_| "duplicate")
        })
        .await
        .unwrap();
    let child_tail = layers
        .effect(child.context(), "scope-test.child-tail", |layer| {
            layer.named.insert("tail", 4).map_err(|_| "duplicate")
        })
        .await
        .unwrap();
    let child_shared = layers
        .effect(child.context(), "scope-test.child-shared", |layer| {
            layer.named.insert("shared", 5).map_err(|_| "duplicate")
        })
        .await
        .unwrap();

    assert_eq!(
        layers
            .effective_named(Some(child.key()), |layer| &layer.named)
            .unwrap(),
        vec![
            ("a".to_owned(), 1),
            ("shared".to_owned(), 5),
            ("tail".to_owned(), 4),
        ]
    );

    for handle in [
        child_shared,
        child_tail,
        parent_shared,
        global_shared,
        global_a,
    ] {
        assert!(handle.dispose().await.is_clean());
    }
    assert!(child.dispose().await.is_clean());
    assert!(parent.dispose().await.is_clean());
    let _cleanup = owner.dispose().await;
}

#[tokio::test]
async fn effective_named_clones_only_the_final_visible_values() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let layers = ScopedLayers::new(
        scope_root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| CloneLayer {
            named: NamedEntries::new(),
        },
        || async { Ok(()) },
    );
    let clones = Arc::new(AtomicUsize::new(0));
    let (owner, context) = active_unscoped_context(&runtime).await;
    let global = layers
        .effect(&context, "scope-test.clone-global", |layer| {
            layer
                .named
                .insert(
                    "shared",
                    CloneProbe {
                        marker: 1,
                        clones: Arc::clone(&clones),
                    },
                )
                .map_err(|_| "duplicate")
        })
        .await
        .unwrap();
    let scope = scope_root.create(&runtime.root()).await.unwrap();
    let overlay = layers
        .effect(scope.context(), "scope-test.clone-overlay", |layer| {
            layer
                .named
                .insert(
                    "shared",
                    CloneProbe {
                        marker: 2,
                        clones: Arc::clone(&clones),
                    },
                )
                .map_err(|_| "duplicate")
        })
        .await
        .unwrap();

    let effective = layers
        .effective_named(Some(scope.key()), |layer| &layer.named)
        .unwrap();

    assert_eq!(effective.len(), 1);
    assert_eq!(effective[0].1.marker, 2);
    assert_eq!(
        clones.load(Ordering::Relaxed),
        effective.len(),
        "shadowed values were cloned before overlay resolution"
    );

    assert!(overlay.dispose().await.is_clean());
    assert!(global.dispose().await.is_clean());
    assert!(scope.dispose().await.is_clean());
    let _cleanup = owner.dispose().await;
}

#[tokio::test]
async fn add_is_visible_before_notify_and_first_failure_exactly_undoes_then_compensates() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let global = Arc::new(Mutex::new(None::<Arc<TestLayer>>));
    let calls = Arc::new(Mutex::new(0_usize));
    let observed_callback = Arc::clone(&observed);
    let global_callback = Arc::clone(&global);
    let calls_callback = Arc::clone(&calls);
    let layers = ScopedLayers::new(
        scope_root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let observed = Arc::clone(&observed_callback);
            let global = Arc::clone(&global_callback);
            let calls = Arc::clone(&calls_callback);
            async move {
                observed
                    .lock()
                    .unwrap()
                    .push(global.lock().unwrap().as_ref().unwrap().named.contains("x"));
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                if *calls == 1 {
                    Err("first notification failed".to_owned())
                } else {
                    Ok(())
                }
            }
        },
    );
    *global.lock().unwrap() = Some(layers.global());
    let (owner, context) = active_unscoped_context(&runtime).await;

    let error = layers
        .effect(&context, "scope-test.rollback", |layer| {
            layer.named.insert("x", 1).map_err(|_| "duplicate")
        })
        .await
        .unwrap_err();
    assert_eq!(error.primary(), "first notification failed");
    assert!(!error.primary_truncated());
    assert!(error.cleanup().is_clean());
    assert!(error.compensation().is_none());
    assert_eq!(*observed.lock().unwrap(), vec![true, false]);
    assert!(!layers.global().named.contains("x"));

    let _cleanup = owner.dispose().await;
}

#[derive(Debug)]
struct PanickingPayloadDrop;

impl Drop for PanickingPayloadDrop {
    fn drop(&mut self) {
        std::panic::panic_any(Self);
    }
}

#[tokio::test]
async fn callback_panic_payload_destruction_is_contained_and_exactly_undone() {
    let runtime = Runtime::default();
    let layers = ScopedLayers::new(
        new_scope_root(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || -> Ready<std::result::Result<(), String>> {
            std::panic::panic_any(PanickingPayloadDrop)
        },
    );
    let (owner, context) = active_unscoped_context(&runtime).await;

    let outcome = AssertUnwindSafe(layers.effect(
        &context,
        "scope-test.callback-panic-payload",
        |layer| layer.named.insert("x", 1).map_err(|_| "duplicate"),
    ))
    .catch_unwind()
    .await;
    let error = outcome
        .expect("callback panic payload destruction must remain contained")
        .unwrap_err();
    assert_eq!(
        error.primary(),
        "scope change callback panic payload destruction panicked"
    );
    assert!(!layers.global().named.contains("x"));

    let _cleanup = owner.dispose().await;
}

struct ReadyChangeFuture;

impl Future for ReadyChangeFuture {
    type Output = std::result::Result<(), String>;

    fn poll(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Self::Output> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for ReadyChangeFuture {
    fn drop(&mut self) {
        panic!("scope change future destruction evidence");
    }
}

#[tokio::test]
async fn ready_change_future_destruction_is_contained_and_exactly_undone() {
    let runtime = Runtime::default();
    let layers = ScopedLayers::new(
        new_scope_root(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || ReadyChangeFuture,
    );
    let (owner, context) = active_unscoped_context(&runtime).await;

    let outcome = AssertUnwindSafe(layers.effect(
        &context,
        "scope-test.callback-future-drop",
        |layer| layer.named.insert("x", 1).map_err(|_| "duplicate"),
    ))
    .catch_unwind()
    .await;
    let error = outcome
        .expect("change future destruction must remain contained")
        .unwrap_err();
    assert_eq!(
        error.primary(),
        "scope change callback future destruction panicked"
    );
    assert!(!layers.global().named.contains("x"));

    let _cleanup = owner.dispose().await;
}

struct PollAndDropPanickingChangeFuture;

impl Future for PollAndDropPanickingChangeFuture {
    type Output = std::result::Result<(), String>;

    fn poll(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Self::Output> {
        panic!("scope change future poll evidence");
    }
}

impl Drop for PollAndDropPanickingChangeFuture {
    fn drop(&mut self) {
        panic!("scope change future destruction evidence");
    }
}

#[tokio::test]
async fn change_future_poll_and_destruction_panics_are_both_bounded() {
    let runtime = Runtime::default();
    let layers = ScopedLayers::new(
        new_scope_root(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || PollAndDropPanickingChangeFuture,
    );
    let (owner, context) = active_unscoped_context(&runtime).await;

    let error = layers
        .effect(&context, "scope-test.callback-poll-and-drop", |layer| {
            layer.named.insert("x", 1).map_err(|_| "duplicate")
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.primary(),
        "scope change callback panicked; scope change callback future destruction panicked"
    );
    assert!(!layers.global().named.contains("x"));

    let _cleanup = owner.dispose().await;
}

enum CancellationChangeFuture {
    Pending(Option<oneshot::Sender<()>>),
    Ready,
}

impl Future for CancellationChangeFuture {
    type Output = std::result::Result<(), String>;

    fn poll(mut self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Self::Output> {
        match &mut *self {
            Self::Pending(entered) => {
                if let Some(entered) = entered.take() {
                    let _ = entered.send(());
                }
                Poll::Pending
            }
            Self::Ready => Poll::Ready(Ok(())),
        }
    }
}

impl Drop for CancellationChangeFuture {
    fn drop(&mut self) {
        if matches!(self, Self::Pending(_)) {
            panic!("cancelled scope change future destruction evidence");
        }
    }
}

#[tokio::test]
async fn cancelling_pending_change_future_contains_drop_and_persistently_undoes() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let scope = root.create(&runtime.root()).await.unwrap();
    let (entered_sender, entered_receiver) = oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_sender)));
    let entered_for_callback = Arc::clone(&entered);
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifications_for_callback = Arc::clone(&notifications);
    let layers = ScopedLayers::new(
        root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            notifications_for_callback.fetch_add(1, Ordering::AcqRel);
            match entered_for_callback.lock().unwrap().take() {
                Some(entered) => CancellationChangeFuture::Pending(Some(entered)),
                None => CancellationChangeFuture::Ready,
            }
        },
    );
    let task_layers = layers.clone();
    let task_context = scope.context().clone();
    let task = tokio::spawn(async move {
        task_layers
            .effect(&task_context, "scope-test.cancel-change", |layer| {
                layer.named.insert("x", 1).map_err(|_| "duplicate")
            })
            .await
    });
    entered_receiver.await.unwrap();

    task.abort();
    let cancellation = task.await.unwrap_err();
    assert!(
        cancellation.is_cancelled(),
        "change future Drop escaped cancellation containment: {cancellation}"
    );
    assert!(scope.dispose().await.is_clean());
    assert_eq!(notifications.load(Ordering::Acquire), 2);
    assert!(layers.peek(scope.key()).unwrap().is_none());
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.effects.current, 0);
    assert_eq!(resources.effect_transactions.current, 0);
}

#[tokio::test]
async fn action_panic_payload_destruction_is_contained_and_reclaims_the_scope() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let scope = root.create(&runtime.root()).await.unwrap();
    let layers = ScopedLayers::new(
        root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || async { Ok(()) },
    );

    let outcome = AssertUnwindSafe(layers.effect(
        scope.context(),
        "scope-test.action-panic-payload",
        |_| -> std::result::Result<rsi_meta_scope::ScopeUndo, &'static str> {
            std::panic::panic_any(PanickingPayloadDrop)
        },
    ))
    .catch_unwind()
    .await;
    let error = outcome
        .expect("action panic payload destruction must remain contained")
        .unwrap_err();
    assert_eq!(
        error.primary(),
        "scope layer action panic payload destruction panicked"
    );
    assert!(layers.peek(scope.key()).unwrap().is_none());

    let _cleanup = scope.dispose().await;
}

#[tokio::test]
async fn action_panic_after_insertion_runs_exact_undo_and_notifies_final_state() {
    let runtime = Runtime::default();
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed_notifications = Arc::clone(&notifications);
    let layers = ScopedLayers::new(
        new_scope_root(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let observed_notifications = Arc::clone(&observed_notifications);
            async move {
                observed_notifications.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
        },
    );
    let (owner, context) = active_unscoped_context(&runtime).await;

    let outcome = AssertUnwindSafe(layers.effect(
        &context,
        "scope-test.action-panic-after-insertion",
        |layer| -> std::result::Result<rsi_meta_scope::ScopeUndo, &'static str> {
            let _undo = layer.named.insert("stranded", 1).unwrap();
            panic!("action panic after insertion evidence")
        },
    ))
    .catch_unwind()
    .await;
    let error = outcome
        .expect("action panic must remain contained")
        .unwrap_err();

    assert_eq!(error.primary(), "scope layer action panicked");
    assert!(error.cleanup().is_clean());
    assert!(!layers.global().named.contains("stranded"));
    assert_eq!(notifications.load(Ordering::Acquire), 1);

    let _cleanup = owner.dispose().await;
}

#[tokio::test]
async fn successful_action_retains_every_captured_entry_undo() {
    let runtime = Runtime::default();
    let layers = ScopedLayers::new(
        new_scope_root(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || async { Ok(()) },
    );
    let (owner, context) = active_unscoped_context(&runtime).await;

    let effect = layers
        .effect(
            &context,
            "scope-test.successful-multiple-insertions",
            |layer| -> std::result::Result<rsi_meta_scope::ScopeUndo, &'static str> {
                let first = layer.named.insert("first", 1).map_err(|_| "duplicate")?;
                let second = layer.named.insert("second", 2).map_err(|_| "duplicate")?;
                drop(first);
                Ok(second)
            },
        )
        .await
        .unwrap();
    assert!(layers.global().named.contains("first"));
    assert!(layers.global().named.contains("second"));

    assert!(effect.dispose().await.is_clean());
    assert!(!layers.global().named.contains("first"));
    assert!(!layers.global().named.contains("second"));
    assert!(owner.dispose().await.is_clean());
}

#[derive(Debug)]
struct PanickingUndoValue;

impl Drop for PanickingUndoValue {
    fn drop(&mut self) {
        std::panic::panic_any(PanickingPayloadDrop);
    }
}

#[derive(Debug)]
struct PanickingUndoLayer {
    named: NamedEntries<PanickingUndoValue>,
}

#[derive(Debug)]
struct PanickingReclamationLayer {
    named: NamedEntries<i32>,
}

impl ScopeLayer for PanickingReclamationLayer {
    fn is_empty(&self) -> bool {
        panic!("scope layer reclamation evidence");
    }
}

#[tokio::test]
async fn reclamation_panics_consume_bounded_slots_and_new_keys_fail_closed() {
    const MAXIMUM_SCOPED_LAYERS: usize = 2;

    let runtime = Runtime::default();
    let root = new_scope_root();
    let layers = ScopedLayers::new(
        root.clone(),
        MAXIMUM_SCOPED_LAYERS,
        |_| PanickingReclamationLayer {
            named: NamedEntries::new(),
        },
        || async { Ok(()) },
    );
    let mut retained = Vec::new();
    for index in 0..MAXIMUM_SCOPED_LAYERS {
        let scope = root.create(&runtime.root()).await.unwrap();
        let handle = layers
            .effect(
                scope.context(),
                format!("scope-test.reclamation-{index}"),
                |layer| layer.named.insert("x", 1).map_err(|_| "duplicate"),
            )
            .await
            .unwrap();
        let report = handle.dispose().await;
        assert_eq!(report.total_failures(), 1);
        assert_eq!(
            report.failures()[0].error,
            "scope layer reclamation panicked"
        );
        retained.push(scope.key().clone());
        let _cleanup = scope.dispose().await;
    }
    assert!(
        retained
            .iter()
            .all(|key| layers.peek(key).unwrap().is_some())
    );

    let rejected = root.create(&runtime.root()).await.unwrap();
    let error = layers
        .effect(rejected.context(), "scope-test.capacity", |layer| {
            layer.named.insert("x", 1).map_err(|_| "duplicate")
        })
        .await
        .unwrap_err();
    assert_eq!(error.primary(), "scoped layer capacity exhausted");
    assert!(layers.peek(rejected.key()).unwrap().is_none());
    let _cleanup = rejected.dispose().await;
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
}

#[tokio::test]
async fn reclaimed_scoped_capacity_is_reusable_by_a_new_key() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let layers = ScopedLayers::new(root.clone(), 1, |_| TestLayer::new(), || async { Ok(()) });

    for name in ["first", "second"] {
        let scope = root.create(&runtime.root()).await.unwrap();
        let handle = layers
            .effect(scope.context(), format!("scope-test.{name}"), |layer| {
                layer.named.insert(name, 1).map_err(|_| "duplicate")
            })
            .await
            .unwrap();
        assert!(handle.dispose().await.is_clean());
        assert!(layers.peek(scope.key()).unwrap().is_none());
        assert!(scope.dispose().await.is_clean());
    }
}

impl ScopeLayer for PanickingUndoLayer {
    fn is_empty(&self) -> bool {
        self.named.is_empty()
    }
}

#[tokio::test]
async fn undo_panic_payload_destruction_does_not_skip_reclamation_or_notification() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let scope = root.create(&runtime.root()).await.unwrap();
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifications_for_callback = Arc::clone(&notifications);
    let layers = ScopedLayers::new(
        root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| PanickingUndoLayer {
            named: NamedEntries::new(),
        },
        move || {
            notifications_for_callback.fetch_add(1, Ordering::AcqRel);
            async { Ok(()) }
        },
    );
    let handle = layers
        .effect(scope.context(), "scope-test.undo-panic-payload", |layer| {
            layer
                .named
                .insert("x", PanickingUndoValue)
                .map_err(|_| "duplicate")
        })
        .await
        .unwrap();

    let outcome = AssertUnwindSafe(handle.dispose()).catch_unwind().await;
    let report = outcome.expect("undo panic payload destruction must remain contained");
    assert_eq!(report.total_failures(), 1);
    assert_eq!(
        report.failures()[0].error,
        "scope entry undo panic payload destruction panicked"
    );
    assert_eq!(notifications.load(Ordering::Acquire), 2);
    assert!(layers.peek(scope.key()).unwrap().is_none());

    let _cleanup = scope.dispose().await;
}

#[tokio::test]
async fn compensation_and_removal_failures_are_bounded_evidence_without_resurrection() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let calls = Arc::new(Mutex::new(0_usize));
    let calls_callback = Arc::clone(&calls);
    let layers = ScopedLayers::new(
        scope_root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let calls = Arc::clone(&calls_callback);
            async move {
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                match *calls {
                    1 => Err("primary".repeat(20_000)),
                    2 => Err("compensation".repeat(20_000)),
                    _ => Ok(()),
                }
            }
        },
    );
    let (owner, context) = active_unscoped_context(&runtime).await;
    let error: MutationError = layers
        .effect(&context, "scope-test.both-fail", |layer| {
            layer.named.insert("failed", 1).map_err(|_| "duplicate")
        })
        .await
        .unwrap_err();
    let maximum = runtime.limits().payloads.maximum_diagnostic_bytes;
    assert!(error.primary().len() <= maximum);
    assert!(error.primary_truncated());
    assert!(error.compensation().unwrap().len() <= maximum);
    assert!(error.compensation_truncated());
    assert!(!error.cleanup().is_clean());
    assert!(!layers.global().named.contains("failed"));

    let removal_calls = Arc::new(Mutex::new(0_usize));
    let removal_callback = Arc::clone(&removal_calls);
    let removal_layers = ScopedLayers::new(
        new_scope_root(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let calls = Arc::clone(&removal_callback);
            async move {
                let mut calls = calls.lock().unwrap();
                *calls += 1;
                if *calls == 2 {
                    Err("removal notification failed".to_owned())
                } else {
                    Ok(())
                }
            }
        },
    );
    let handle = removal_layers
        .effect(&context, "scope-test.remove", |layer| {
            layer.named.insert("removed", 1).map_err(|_| "duplicate")
        })
        .await
        .unwrap();
    let report = handle.dispose().await;
    assert!(!report.is_clean());
    assert!(!removal_layers.global().named.contains("removed"));

    let _cleanup = owner.dispose().await;
}

#[tokio::test]
async fn action_and_notification_can_reenter_reads_without_a_scope_store_lock() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let scope = scope_root.create(&runtime.root()).await.unwrap();
    let holder = Arc::new(Mutex::new(None::<ScopedLayers<TestLayer>>));
    let holder_callback = Arc::clone(&holder);
    let callback_views = Arc::new(Mutex::new(Vec::new()));
    let callback_views_observed = Arc::clone(&callback_views);
    let key = scope.key().clone();
    let callback_key = key.clone();
    let layers = ScopedLayers::new(
        scope_root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let holder = Arc::clone(&holder_callback);
            let observed = Arc::clone(&callback_views_observed);
            let key = callback_key.clone();
            async move {
                let layers = holder.lock().unwrap().as_ref().unwrap().clone();
                observed
                    .lock()
                    .unwrap()
                    .push(layers.peek(&key).unwrap().is_some());
                Ok(())
            }
        },
    );
    *holder.lock().unwrap() = Some(layers.clone());
    let action_layers = layers.clone();
    let action_key = key.clone();

    let handle = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        layers.effect(scope.context(), "scope-test.reentrant", move |layer| {
            assert!(action_layers.peek(&action_key).unwrap().is_some());
            layer.named.insert("reentrant", 1).map_err(|_| "duplicate")
        }),
    )
    .await
    .expect("scope callback reentry deadlocked")
    .unwrap();
    assert!(handle.dispose().await.is_clean());
    assert_eq!(*callback_views.lock().unwrap(), vec![true, false]);
    assert!(scope.dispose().await.is_clean());
}

#[tokio::test]
async fn parent_rebind_never_implicitly_notifies_a_product_registry() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let first = scope_root.create(&runtime.root()).await.unwrap();
    let second = scope_root.create(&runtime.root()).await.unwrap();
    let (child, binding) = scope_root
        .create_child(&runtime.root(), first.key())
        .await
        .unwrap();
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifications_callback = Arc::clone(&notifications);
    let _layers = ScopedLayers::new(
        scope_root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let notifications = Arc::clone(&notifications_callback);
            async move {
                notifications.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        },
    );

    binding.rebind(second.key()).unwrap();
    assert_eq!(notifications.load(Ordering::Relaxed), 0);

    assert!(child.dispose().await.is_clean());
    assert!(second.dispose().await.is_clean());
    assert!(first.dispose().await.is_clean());
}

#[derive(Debug)]
struct RollbackFactory {
    identity: FactoryIdentity,
    root: ScopeRoot,
    layers: ScopedLayers<TestLayer>,
    key: Arc<Mutex<Option<ScopeKey>>>,
}

#[async_trait]
impl PluginFactory for RollbackFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let scope = self
            .root
            .create(plan.context())
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        *self.key.lock().unwrap() = Some(scope.key().clone());
        self.layers
            .effect(scope.context(), "scope-test.rollback-owned", |layer| {
                layer.named.insert("owned", 1).map_err(|_| "duplicate")
            })
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        Err(MetaError::Activation("force parent rollback".to_owned()))
    }
}

#[tokio::test]
async fn scope_effects_follow_the_same_context_through_parent_activation_rollback() {
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let layers = ScopedLayers::new(
        scope_root.clone(),
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        || async { Ok(()) },
    );
    let key = Arc::new(Mutex::new(None));
    let factory = Arc::new(RollbackFactory {
        identity: FactoryIdentity::builtin("scope-test.rollback", "1"),
        root: scope_root,
        layers: layers.clone(),
        key: Arc::clone(&key),
    });
    let fiber = runtime.root().apply(factory, Value::Null).await.unwrap();
    assert!(fiber.wait_active(&CancellationToken::new()).await.is_err());
    let scope_key = key.lock().unwrap().clone().unwrap();
    assert!(layers.peek(&scope_key).unwrap().is_none());
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
    let _cleanup = fiber.dispose().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_add_read_and_remove_reclaims_one_shared_scope_layer() {
    const TASKS: usize = 64;
    let runtime = Runtime::default();
    let scope_root = new_scope_root();
    let scope = scope_root.create(&runtime.root()).await.unwrap();
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifications_callback = Arc::clone(&notifications);
    let layers = ScopedLayers::new(
        scope_root,
        TEST_MAXIMUM_SCOPED_LAYERS,
        |_| TestLayer::new(),
        move || {
            let notifications = Arc::clone(&notifications_callback);
            async move {
                notifications.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        },
    );
    let inserted = Arc::new(Barrier::new(TASKS + 1));
    let remove = Arc::new(Barrier::new(TASKS + 1));
    let mut tasks = Vec::new();
    for index in 0..TASKS {
        let context = scope.context().clone();
        let key = scope.key().clone();
        let layers = layers.clone();
        let inserted = Arc::clone(&inserted);
        let remove = Arc::clone(&remove);
        tasks.push(tokio::spawn(async move {
            let name = format!("entry-{index}");
            let handle = layers
                .effect(
                    &context,
                    format!("scope-test.concurrent-{index}"),
                    |layer| {
                        layer
                            .named
                            .insert(name, i32::try_from(index).unwrap())
                            .map_err(|_| "duplicate")
                    },
                )
                .await
                .unwrap();
            assert!(
                !layers
                    .effective_named(Some(&key), |layer| &layer.named)
                    .unwrap()
                    .is_empty()
            );
            inserted.wait().await;
            remove.wait().await;
            assert!(handle.dispose().await.is_clean());
        }));
    }

    inserted.wait().await;
    assert_eq!(
        layers
            .effective_named(Some(scope.key()), |layer| &layer.named)
            .unwrap()
            .len(),
        TASKS
    );
    remove.wait().await;
    for task in tasks {
        task.await.unwrap();
    }
    assert!(layers.peek(scope.key()).unwrap().is_none());
    assert_eq!(notifications.load(Ordering::Relaxed), TASKS * 2);
    assert!(scope.dispose().await.is_clean());
}
