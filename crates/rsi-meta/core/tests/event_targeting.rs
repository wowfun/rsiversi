use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, CleanupReport, ConfigValue, Context, ContextExtension, DeadlineLimits,
    DispatchMode, EventHandle, EventHandler, EventOptions, EventOutcome, EventTarget,
    ExecutionLimits, FactoryIdentity, InvocationContext, ListenerView, MetaError, PayloadLimits,
    PluginFactory, PreparedActivation, Result, Runtime, RuntimeLimits, TopologyLimits,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

mod support;

use support::FactorySpec;

struct ScopeName;

type SeenListener = (u64, u64, Option<String>);
type SeenListeners = Arc<Mutex<Vec<SeenListener>>>;

impl ContextExtension for ScopeName {
    type Value = String;
}

#[derive(Debug)]
struct CountingHandler(Arc<AtomicUsize>);

#[async_trait]
impl EventHandler for CountingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}

#[derive(Debug)]
struct ListenerFactory {
    spec: FactorySpec,
    event: &'static str,
    scope: Option<&'static str>,
    options: EventOptions,
    handler: Arc<dyn EventHandler>,
    handle: Option<Arc<Mutex<Option<EventHandle>>>>,
}

#[async_trait]
impl PluginFactory for ListenerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let mut context = plan.context().clone();
        if let Some(scope) = self.scope {
            context = context.with_extension::<ScopeName>(scope.to_owned())?;
        }
        let handle = context.on(self.event, Arc::clone(&self.handler), self.options)?;
        if let Some(capture) = &self.handle {
            *capture.lock().expect("event handle capture poisoned") = Some(handle);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ExtensionTarget {
    runtime: Runtime,
    expected: Option<&'static str>,
    seen: SeenListeners,
}

impl EventTarget for ExtensionTarget {
    fn select(&self, listener: &ListenerView) -> Result<bool> {
        // This would deadlock if target evaluation retained the Runtime state lock.
        let _snapshot = self.runtime.snapshot();
        let (fiber, generation) = listener.owner();
        let extension = listener.extension::<ScopeName>().as_deref().cloned();
        self.seen.lock().expect("target capture poisoned").push((
            fiber.0,
            generation.0,
            extension.clone(),
        ));
        Ok(extension.as_deref() == self.expected)
    }
}

async fn apply_target_listener(
    runtime: &Runtime,
    name: &'static str,
    scope: Option<&'static str>,
    options: EventOptions,
    count: Arc<AtomicUsize>,
) {
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin(name, "1")),
                event: "targeted",
                scope,
                options,
                handler: Arc::new(CountingHandler(count)),
                handle: None,
            }),
            Value::Null,
        )
        .await
        .unwrap();
}

async fn dispatch_for_scope(
    runtime: &Runtime,
    expected: Option<&'static str>,
    seen: SeenListeners,
) -> rsi_meta::EventReceipt {
    runtime
        .root()
        .dispatch_targeted(
            "targeted",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(ExtensionTarget {
                runtime: runtime.clone(),
                expected,
                seen,
            }),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn target_owns_extension_matching_global_bypass_and_false_preserves_once() {
    let runtime = Runtime::default();
    let ordinary = Arc::new(AtomicUsize::new(0));
    let once = Arc::new(AtomicUsize::new(0));
    let missing = Arc::new(AtomicUsize::new(0));
    let global = Arc::new(AtomicUsize::new(0));
    for (name, scope, options, count) in [
        (
            "target-ordinary",
            Some("ordinary"),
            EventOptions::default(),
            Arc::clone(&ordinary),
        ),
        (
            "target-once",
            Some("once"),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
            Arc::clone(&once),
        ),
        (
            "target-missing",
            None,
            EventOptions::default(),
            Arc::clone(&missing),
        ),
        (
            "target-global",
            None,
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
            Arc::clone(&global),
        ),
    ] {
        apply_target_listener(&runtime, name, scope, options, count).await;
    }

    let first_seen = Arc::new(Mutex::new(Vec::new()));
    let first = dispatch_for_scope(&runtime, Some("ordinary"), Arc::clone(&first_seen)).await;
    assert_eq!(first.invoked, 2);
    assert_eq!(ordinary.load(Ordering::Acquire), 1);
    assert_eq!(once.load(Ordering::Acquire), 0);
    assert_eq!(global.load(Ordering::Acquire), 1);
    {
        let first_seen = first_seen.lock().expect("target capture poisoned");
        assert_eq!(first_seen.len(), 3, "global listeners bypass the target");
        assert!(
            first_seen
                .iter()
                .all(|(fiber, generation, _)| *fiber > 0 && *generation > 0)
        );
    }

    let second = dispatch_for_scope(&runtime, Some("once"), Arc::new(Mutex::new(Vec::new()))).await;
    assert_eq!(second.invoked, 2);
    assert_eq!(once.load(Ordering::Acquire), 1);
    assert_eq!(global.load(Ordering::Acquire), 2);

    let missing_receipt =
        dispatch_for_scope(&runtime, None, Arc::new(Mutex::new(Vec::new()))).await;
    assert_eq!(missing_receipt.invoked, 2);
    assert_eq!(missing.load(Ordering::Acquire), 1);
    assert_eq!(global.load(Ordering::Acquire), 3);

    let third = dispatch_for_scope(&runtime, Some("once"), Arc::new(Mutex::new(Vec::new()))).await;
    assert_eq!(third.invoked, 1);
    assert_eq!(once.load(Ordering::Acquire), 1);
    assert_eq!(global.load(Ordering::Acquire), 4);
}

#[derive(Debug)]
enum FailingTarget {
    Error(AtomicUsize),
    Panic(AtomicUsize),
    PanicPayloadDrop(AtomicUsize),
}

#[derive(Debug)]
struct PanickingPayloadDrop;

impl Drop for PanickingPayloadDrop {
    fn drop(&mut self) {
        panic!("selector panic payload destruction evidence");
    }
}

impl EventTarget for FailingTarget {
    fn select(&self, _: &ListenerView) -> Result<bool> {
        let selected = match self {
            Self::Error(count) | Self::Panic(count) | Self::PanicPayloadDrop(count) => {
                count.fetch_add(1, Ordering::AcqRel)
            }
        };
        if selected == 0 {
            return Ok(true);
        }
        match self {
            Self::Error(_) => Err(MetaError::Event("selector evidence ".repeat(128))),
            Self::Panic(_) => panic!("selector panic evidence"),
            Self::PanicPayloadDrop(_) => std::panic::panic_any(PanickingPayloadDrop),
        }
    }
}

#[tokio::test]
async fn selector_error_or_panic_is_bounded_and_starts_no_callbacks() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_bytes: 96,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let callbacks = Arc::new(AtomicUsize::new(0));
    for name in ["target-failure-first", "target-failure-second"] {
        runtime
            .root()
            .apply(
                Arc::new(ListenerFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin(name, "1")),
                    event: "target-failure",
                    scope: None,
                    options: EventOptions::default(),
                    handler: Arc::new(CountingHandler(Arc::clone(&callbacks))),
                    handle: None,
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }

    let error = runtime
        .root()
        .dispatch_targeted(
            "target-failure",
            DispatchMode::Parallel,
            Value::Null,
            Arc::new(FailingTarget::Error(AtomicUsize::new(0))),
        )
        .await
        .unwrap_err();
    let MetaError::Event(message) = error else {
        panic!("selector failures are normalized as event failures");
    };
    assert!(message.len() <= 96);
    assert_eq!(callbacks.load(Ordering::Acquire), 0);

    let error = runtime
        .root()
        .dispatch_targeted(
            "target-failure",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(FailingTarget::Panic(AtomicUsize::new(0))),
        )
        .await
        .unwrap_err();
    assert_eq!(error, MetaError::Event("event target panicked".to_owned()));
    assert_eq!(callbacks.load(Ordering::Acquire), 0);

    let error = runtime
        .root()
        .dispatch_targeted(
            "target-failure",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(FailingTarget::PanicPayloadDrop(AtomicUsize::new(0))),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        MetaError::Event("event target panic payload destruction panicked".to_owned())
    );
    assert_eq!(callbacks.load(Ordering::Acquire), 0);
}

#[derive(Debug)]
struct ReentrantTarget {
    context: Context,
    registered: AtomicUsize,
    handle: Arc<Mutex<Option<EventHandle>>>,
    callbacks: Arc<AtomicUsize>,
}

impl EventTarget for ReentrantTarget {
    fn select(&self, _: &ListenerView) -> Result<bool> {
        if self.registered.fetch_add(1, Ordering::AcqRel) == 0 {
            let handle = self.context.on(
                "target-reentry-added",
                Arc::new(CountingHandler(Arc::clone(&self.callbacks))),
                EventOptions::default(),
            )?;
            *self.handle.lock().expect("event handle capture poisoned") = Some(handle);
        }
        Ok(true)
    }
}

#[tokio::test]
async fn selector_can_reenter_listener_registration_outside_runtime_locks() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("target-reentry-owner", "1")),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let source = context
        .on(
            "target-reentry-source",
            Arc::new(CountingHandler(Arc::new(AtomicUsize::new(0)))),
            EventOptions::default(),
        )
        .unwrap();
    let added = Arc::new(Mutex::new(None));
    let callbacks = Arc::new(AtomicUsize::new(0));

    assert_eq!(
        context
            .dispatch_targeted(
                "target-reentry-source",
                DispatchMode::Emit,
                Value::Null,
                Arc::new(ReentrantTarget {
                    context: context.clone(),
                    registered: AtomicUsize::new(0),
                    handle: Arc::clone(&added),
                    callbacks: Arc::clone(&callbacks),
                }),
            )
            .await
            .unwrap()
            .invoked,
        1
    );
    assert_eq!(
        context
            .dispatch("target-reentry-added", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        1
    );
    assert_eq!(callbacks.load(Ordering::Acquire), 1);
    assert!(source.dispose().await.is_clean());
    let added = added
        .lock()
        .expect("event handle capture poisoned")
        .clone()
        .expect("selector registered one listener");
    assert!(added.dispose().await.is_clean());
}

#[derive(Debug)]
struct BlockingTarget {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    selected: bool,
}

impl EventTarget for BlockingTarget {
    fn select(&self, _: &ListenerView) -> Result<bool> {
        self.entered
            .send(())
            .expect("target test retains its entered receiver");
        self.release
            .lock()
            .expect("target release receiver poisoned")
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("target test releases deterministic selection barrier");
        Ok(self.selected)
    }
}

#[derive(Debug)]
struct DeadlineBlockingTarget {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    selections: Arc<AtomicUsize>,
}

impl EventTarget for DeadlineBlockingTarget {
    fn select(&self, _: &ListenerView) -> Result<bool> {
        if self.selections.fetch_add(1, Ordering::AcqRel) == 0 {
            self.entered
                .send(())
                .expect("target test retains its entered receiver");
            self.release
                .lock()
                .expect("target release receiver poisoned")
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("target test releases deterministic selection barrier");
        }
        Ok(true)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_target_selection_obeys_the_dispatch_deadline() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_event_dispatches: 1,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let callbacks = Arc::new(AtomicUsize::new(0));
    apply_target_listener(
        &runtime,
        "target-deadline",
        None,
        EventOptions::default(),
        Arc::clone(&callbacks),
    )
    .await;
    apply_target_listener(
        &runtime,
        "target-deadline-second",
        None,
        EventOptions::default(),
        Arc::clone(&callbacks),
    )
    .await;

    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let selections = Arc::new(AtomicUsize::new(0));
    let dispatch_context = runtime.root();
    let dispatch_selections = Arc::clone(&selections);
    let mut dispatch = tokio::spawn(async move {
        dispatch_context
            .dispatch_targeted(
                "targeted",
                DispatchMode::Emit,
                Value::Null,
                Arc::new(DeadlineBlockingTarget {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                    selections: dispatch_selections,
                }),
            )
            .await
    });
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("selector reaches its barrier");
    })
    .await
    .unwrap();

    let observed = tokio::time::timeout(std::time::Duration::from_millis(500), &mut dispatch).await;
    let completed_before_release = observed.is_ok();
    let retained_dispatches = runtime.resource_snapshot().event_dispatches.current;
    let capacity_error = runtime
        .root()
        .dispatch("targeted", DispatchMode::Emit, Value::Null)
        .await
        .unwrap_err();
    release_tx.send(()).unwrap();
    let outcome = match observed {
        Ok(outcome) => outcome,
        Err(_) => dispatch.await,
    };

    assert!(
        completed_before_release,
        "a blocked selector kept the dispatch alive past its deadline"
    );
    assert_eq!(
        outcome.unwrap().unwrap_err(),
        MetaError::Timeout("event dispatch")
    );
    assert_eq!(retained_dispatches, 1);
    assert_eq!(
        capacity_error,
        MetaError::CapacityExhausted {
            resource: "event dispatches"
        }
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while runtime.resource_snapshot().event_dispatches.current != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("selector return releases retained dispatch ownership");
    assert_eq!(selections.load(Ordering::Acquire), 1);
    assert_eq!(callbacks.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_listener_survives_concurrent_disposal_for_the_current_dispatch_only() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("target-concurrent-disposal", "1")),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let callbacks = Arc::new(AtomicUsize::new(0));
    let listener = context
        .on(
            "target-concurrent-disposal",
            Arc::new(CountingHandler(Arc::clone(&callbacks))),
            EventOptions::default(),
        )
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let dispatch_context = context.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_context
            .dispatch_targeted(
                "target-concurrent-disposal",
                DispatchMode::Emit,
                Value::Null,
                Arc::new(BlockingTarget {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                    selected: true,
                }),
            )
            .await
    });
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("selector reaches its barrier");
    })
    .await
    .unwrap();

    assert!(listener.dispose().await.is_clean());
    release_tx.send(()).unwrap();
    assert_eq!(dispatch.await.unwrap().unwrap().invoked, 1);
    assert_eq!(callbacks.load(Ordering::Acquire), 1);
    assert_eq!(
        context
            .dispatch(
                "target-concurrent-disposal",
                DispatchMode::Emit,
                Value::Null,
            )
            .await
            .unwrap()
            .invoked,
        0
    );
}

#[derive(Debug)]
struct SnapshotPanickingDropHandler;

impl Drop for SnapshotPanickingDropHandler {
    fn drop(&mut self) {
        panic!("dispatch snapshot listener destructor evidence");
    }
}

#[async_trait]
impl EventHandler for SnapshotPanickingDropHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_snapshot_contains_a_concurrently_disposed_listener_destructor_panic() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("target-rejected-drop-panic", "1")),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let listener = context
        .on(
            "target-rejected-drop-panic",
            Arc::new(SnapshotPanickingDropHandler),
            EventOptions::default(),
        )
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let dispatch_context = context.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_context
            .dispatch_targeted(
                "target-rejected-drop-panic",
                DispatchMode::Emit,
                Value::Null,
                Arc::new(BlockingTarget {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                    selected: false,
                }),
            )
            .await
    });
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("selector reaches its barrier");
    })
    .await
    .unwrap();

    assert!(listener.dispose().await.is_clean());
    release_tx.send(()).unwrap();
    assert_eq!(
        dispatch
            .await
            .expect("dispatch snapshot destructor panic escaped its owner")
            .unwrap()
            .invoked,
        0
    );
    assert!(runtime.snapshot().terminal.is_some());
    let report = owner.dispose().await;
    assert_eq!(report.total_failures(), 1);
    assert_eq!(report.failures()[0].label, "remove event listener");
    assert!(report.failures()[0].error.contains("destructor panicked"));
}

#[derive(Debug)]
struct LoadingListenerFactory {
    spec: FactorySpec,
    registered: Arc<Notify>,
    release: Arc<Notify>,
    handle: Arc<Mutex<Option<EventHandle>>>,
    callbacks: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for LoadingListenerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let handle = plan.context().on(
            "loading-listener",
            Arc::new(CountingHandler(Arc::clone(&self.callbacks))),
            EventOptions::default(),
        )?;
        *self.handle.lock().expect("event handle capture poisoned") = Some(handle);
        self.registered.notify_one();
        self.release.notified().await;
        Err(MetaError::Activation("rollback evidence".to_owned()))
    }
}

#[tokio::test]
async fn loading_listener_is_immediate_and_rollback_disposes_the_exact_handle() {
    let runtime = Runtime::default();
    let registered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let handle = Arc::new(Mutex::new(None));
    let callbacks = Arc::new(AtomicUsize::new(0));
    let apply_runtime = runtime.clone();
    let factory = Arc::new(LoadingListenerFactory {
        spec: FactorySpec::new(FactoryIdentity::builtin("loading-listener", "1")),
        registered: Arc::clone(&registered),
        release: Arc::clone(&release),
        handle: Arc::clone(&handle),
        callbacks: Arc::clone(&callbacks),
    });
    let apply = tokio::spawn(async move { apply_runtime.root().apply(factory, Value::Null).await });
    registered.notified().await;

    assert_eq!(runtime.resource_snapshot().listeners.current, 1);
    assert_eq!(
        runtime
            .root()
            .dispatch("loading-listener", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        1
    );
    assert_eq!(callbacks.load(Ordering::Acquire), 1);
    let exact = handle
        .lock()
        .expect("event handle capture poisoned")
        .clone()
        .expect("Loading registration returns its handle");
    assert_eq!(exact.id(), exact.clone().id());

    release.notify_one();
    let fiber = apply
        .await
        .unwrap()
        .expect("admitted apply returns its Runtime-owned Fiber");
    assert!(matches!(
        fiber.wait_settled().await.state,
        rsi_meta::FiberState::Failed(_)
    ));
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
    assert_eq!(
        runtime
            .root()
            .dispatch("loading-listener", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
    assert!(exact.dispose().await.is_clean());
    assert!(exact.dispose().await.is_clean());
}

#[derive(Debug)]
struct ContextCaptureFactory {
    spec: FactorySpec,
    context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[tokio::test]
async fn once_churn_releases_listener_effect_and_transaction_capacity() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            maximum_effects_per_fiber: 1,
            maximum_effects: 1,
            maximum_effect_transactions_per_fiber: 1,
            maximum_effect_transactions: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("once-churn", "1")),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let baseline = runtime.resource_snapshot();

    for _ in 0..128 {
        let _handle = context
            .on(
                "once-churn",
                Arc::new(CountingHandler(Arc::new(AtomicUsize::new(0)))),
                EventOptions {
                    once: true,
                    ..EventOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            context
                .dispatch("once-churn", DispatchMode::Emit, Value::Null)
                .await
                .unwrap()
                .invoked,
            1
        );
        let current = runtime.resource_snapshot();
        assert_eq!(current.listeners.current, baseline.listeners.current);
        assert_eq!(current.effects.current, baseline.effects.current);
        assert_eq!(
            current.effect_transactions.current,
            baseline.effect_transactions.current
        );
    }
}

#[tokio::test]
async fn dynamic_listener_retirement_does_not_join_its_effect_before_lifo_cleanup() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin(
                    "dynamic-listener-retirement",
                    "1",
                )),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let handle = context
        .on(
            "dynamic-retirement",
            Arc::new(CountingHandler(Arc::new(AtomicUsize::new(0)))),
            EventOptions::default(),
        )
        .unwrap();
    let admitted = runtime.resource_snapshot();
    assert_eq!(admitted.listeners.current, 1);
    assert_eq!(admitted.effects.current, 1);
    assert_eq!(admitted.effect_transactions.current, 1);

    let report = tokio::time::timeout(std::time::Duration::from_secs(2), fiber.dispose())
        .await
        .expect("dynamic listener retirement must not deadlock");
    assert!(report.is_clean());
    let complete = runtime.resource_snapshot();
    assert_eq!(complete.listeners.current, 0);
    assert_eq!(complete.effects.current, 0);
    assert_eq!(complete.effect_transactions.current, 0);
    assert!(handle.dispose().await.is_clean());
}

#[derive(Debug)]
struct PanickingDropHandler;

#[async_trait]
impl EventHandler for PanickingDropHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}

impl Drop for PanickingDropHandler {
    fn drop(&mut self) {
        panic!("listener destructor evidence");
    }
}

#[tokio::test]
async fn explicit_dispose_completes_with_a_report_when_listener_destruction_panics() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin(
                    "panicking-listener-destruction",
                    "1",
                )),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let handle = context
        .on(
            "panicking-listener-destruction",
            Arc::new(PanickingDropHandler),
            EventOptions::default(),
        )
        .unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(2), handle.dispose())
        .await
        .expect("a destructor panic must not strand exact removal completion");
    assert_eq!(report.total_failures(), 1);
    assert_eq!(report.failures()[0].label, "remove event listener");
    assert!(
        report.failures()[0]
            .error
            .contains("listener removal panicked")
    );
    assert!(matches!(
        context
            .dispatch(
                "panicking-listener-destruction",
                DispatchMode::Emit,
                Value::Null,
            )
            .await,
        Err(MetaError::RuntimeTerminal(_))
    ));
}

#[derive(Debug)]
struct LoadingPanickingDisposeFactory {
    spec: FactorySpec,
    report: Arc<Mutex<Option<CleanupReport>>>,
    complete: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for LoadingPanickingDisposeFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let handle = plan.context().on(
            "loading-panicking-listener-destruction",
            Arc::new(PanickingDropHandler),
            EventOptions::default(),
        )?;
        let report = handle.dispose().await;
        *self.report.lock().expect("cleanup report capture poisoned") = Some(report);
        self.complete.notify_one();
        Ok(())
    }
}

#[tokio::test]
async fn loading_dispose_retains_a_detached_listener_destruction_failure() {
    let runtime = Runtime::default();
    let report = Arc::new(Mutex::new(None));
    let complete = Arc::new(Notify::new());
    let fiber = runtime
        .root()
        .apply(
            Arc::new(LoadingPanickingDisposeFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin(
                    "loading-panicking-listener-destruction",
                    "1",
                )),
                report: Arc::clone(&report),
                complete: Arc::clone(&complete),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), complete.notified())
        .await
        .expect("Loading exact disposal must publish its cleanup result");
    let report = report
        .lock()
        .expect("cleanup report capture poisoned")
        .clone()
        .expect("Loading exact disposal captures a cleanup report");
    assert_eq!(report.total_failures(), 1);
    assert_eq!(report.failures()[0].label, "remove event listener");
    assert!(
        report.failures()[0]
            .error
            .contains("listener removal panicked")
    );
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), fiber.wait_settled())
        .await
        .expect("terminalized Loading ownership still rolls back");
    let rsi_meta::FiberState::Failed(error) = settled.state else {
        panic!("terminalized Loading ownership must fail activation");
    };
    assert!(error.contains("listener removal panicked"));
}

#[tokio::test]
async fn failed_publication_churn_releases_its_wrapper_without_a_ghost_listener() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            maximum_effects_per_fiber: 2,
            maximum_effects: 2,
            maximum_effect_transactions_per_fiber: 2,
            maximum_effect_transactions: 2,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin(
                    "failed-listener-publication",
                    "1",
                )),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let survivor = context
        .on(
            "publication-survivor",
            Arc::new(CountingHandler(Arc::new(AtomicUsize::new(0)))),
            EventOptions::default(),
        )
        .unwrap();
    let baseline = runtime.resource_snapshot();

    for _ in 0..64 {
        assert!(matches!(
            context.on(
                "publication-failure",
                Arc::new(CountingHandler(Arc::new(AtomicUsize::new(0)))),
                EventOptions::default(),
            ),
            Err(MetaError::CapacityExhausted {
                resource: "event listeners"
            })
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let usage = runtime.resource_snapshot();
                if usage.effects.current == baseline.effects.current
                    && usage.effect_transactions.current == baseline.effect_transactions.current
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed publication wrapper must roll back");
    }
    assert_eq!(
        context
            .dispatch("publication-failure", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
    assert!(survivor.dispose().await.is_clean());
    let complete = runtime.resource_snapshot();
    assert_eq!(complete.listeners.current, 0);
    assert_eq!(complete.effects.current, 0);
    assert_eq!(complete.effect_transactions.current, 0);
}
