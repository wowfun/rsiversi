use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, CallerEffect, ConfigValue, DeadlineLimits, DispatchMode, EventHandler,
    EventOptions, EventOutcome, FactoryIdentity, InvocationContext, MetaError, PayloadLimits,
    PluginFactory, PreparedActivation, Result, Runtime, RuntimeLimits,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

mod support;

use support::{ContextCaptureFactory, FactorySpec, ListenerCaptureFactory, wait_active};

#[derive(Debug)]
struct ValueIdentityHandler(Arc<Mutex<Vec<Arc<Value>>>>);

#[async_trait]
impl EventHandler for ValueIdentityHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0
            .lock()
            .expect("event value capture poisoned")
            .push(value);
        Ok(EventOutcome::Continue(Value::Null))
    }
}

#[tokio::test]
async fn parallel_listeners_share_one_immutable_input_value() {
    let runtime = Runtime::default();
    let values = Arc::new(Mutex::new(Vec::new()));
    for name in ["shared-value-first", "shared-value-second"] {
        runtime
            .root()
            .apply(
                Arc::new(EventFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin(name, "1")),
                    event: "shared-value",
                    handler: Arc::new(ValueIdentityHandler(Arc::clone(&values))),
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }
    runtime
        .root()
        .dispatch(
            "shared-value",
            DispatchMode::Parallel,
            Value::String("payload".repeat(1_024)),
        )
        .await
        .unwrap();

    let values = values.lock().expect("event value capture poisoned");
    assert_eq!(values.len(), 2);
    assert!(Arc::ptr_eq(&values[0], &values[1]));
}

#[derive(Debug)]
struct HangingHandler(Arc<Notify>);

#[async_trait]
impl EventHandler for HangingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[derive(Debug)]
struct PanickingHandlerFutureDropGuard;

impl Drop for PanickingHandlerFutureDropGuard {
    fn drop(&mut self) {
        panic!("event handler future drop panic evidence");
    }
}

#[derive(Debug)]
struct PanickingFutureDropHandler;

#[async_trait]
impl EventHandler for PanickingFutureDropHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        let _guard = PanickingHandlerFutureDropGuard;
        std::future::pending().await
    }
}

#[derive(Debug)]
struct RecursivelyPanickingEventPayload;

impl Drop for RecursivelyPanickingEventPayload {
    fn drop(&mut self) {
        std::panic::panic_any(Self);
    }
}

#[derive(Debug)]
struct PanicWhileConstructingEventFuture;

impl EventHandler for PanicWhileConstructingEventFuture {
    fn handle<'life0, 'async_trait>(
        &'life0 self,
        _: InvocationContext,
        _: Arc<Value>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventOutcome>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        std::panic::panic_any(RecursivelyPanickingEventPayload)
    }
}

struct DeferOnDropEventFuture {
    effect: Option<CallerEffect>,
    result: Arc<Mutex<Option<Result<()>>>>,
    cleanups: Arc<AtomicUsize>,
}

impl std::future::Future for DeferOnDropEventFuture {
    type Output = Result<EventOutcome>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::task::Poll::Ready(Ok(EventOutcome::Continue(Value::Null)))
    }
}

impl Drop for DeferOnDropEventFuture {
    fn drop(&mut self) {
        let cleanups = Arc::clone(&self.cleanups);
        let result = self
            .effect
            .take()
            .expect("event future retains the exact caller effect")
            .defer(
                "event future drop cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        cleanups.fetch_add(1, Ordering::AcqRel);
                        Ok(())
                    })
                }),
            );
        *self.result.lock().expect("event drop result poisoned") = Some(result);
    }
}

#[derive(Debug)]
struct DeferOnFutureDropHandler {
    result: Arc<Mutex<Option<Result<()>>>>,
    cleanups: Arc<AtomicUsize>,
}

impl EventHandler for DeferOnFutureDropHandler {
    fn handle<'life0, 'async_trait>(
        &'life0 self,
        invocation: InvocationContext,
        _: Arc<Value>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventOutcome>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(DeferOnDropEventFuture {
            effect: Some(
                invocation
                    .caller_effect()
                    .expect("owned event caller has callback effect")
                    .clone(),
            ),
            result: Arc::clone(&self.result),
            cleanups: Arc::clone(&self.cleanups),
        })
    }
}

struct ReadyThenPanickingEventFuture(Option<EventOutcome>);

impl std::future::Future for ReadyThenPanickingEventFuture {
    type Output = Result<EventOutcome>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::task::Poll::Ready(Ok(self.0.take().expect("event future is polled once")))
    }
}

impl Drop for ReadyThenPanickingEventFuture {
    fn drop(&mut self) {
        panic!("completed event future drop panic evidence");
    }
}

#[derive(Debug)]
struct DeepOutcomeThenPanickingFutureDropHandler;

impl EventHandler for DeepOutcomeThenPanickingFutureDropHandler {
    fn handle<'life0, 'async_trait>(
        &'life0 self,
        _: InvocationContext,
        _: Arc<Value>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EventOutcome>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(ReadyThenPanickingEventFuture(Some(EventOutcome::Continue(
            deeply_nested_event_value(),
        ))))
    }
}

fn deeply_nested_event_value() -> Value {
    let mut value = Value::Null;
    for _ in 0..100_000 {
        value = Value::Array(vec![value]);
    }
    value
}

fn run_isolated_event_case(test: &str, child_variable: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .env(child_variable, "run")
        .args(["--exact", test, "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated event case crashed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn completed_future_drop_panic_is_bounded_after_deep_outcome_adoption() {
    const CHILD: &str = "RSI_META_DEEP_EVENT_FUTURE_DROP_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        executor.block_on(async {
            let runtime = Runtime::default();
            runtime
                .root()
                .apply(
                    Arc::new(EventFactory {
                        spec: FactorySpec::new(FactoryIdentity::builtin(
                            "deep-event-future-drop",
                            "1",
                        )),
                        event: "deep-event-future-drop",
                        handler: Arc::new(DeepOutcomeThenPanickingFutureDropHandler),
                    }),
                    Value::Null,
                )
                .await
                .unwrap();
            let result = std::panic::AssertUnwindSafe(runtime.root().dispatch(
                "deep-event-future-drop",
                DispatchMode::Emit,
                Value::Null,
            ))
            .catch_unwind()
            .await
            .expect("completed event future destruction escaped its callback boundary");
            assert_eq!(
                result.unwrap_err(),
                MetaError::Event("event handler panicked".to_owned()),
            );
            assert_eq!(runtime.resource_snapshot().event_callbacks.current, 0);
            assert!(runtime.shutdown().await.is_complete());
        });
        return;
    }

    run_isolated_event_case(
        "completed_future_drop_panic_is_bounded_after_deep_outcome_adoption",
        CHILD,
    );
}

#[derive(Debug)]
struct HangingFactory(FactorySpec, Arc<Notify>);

#[async_trait]
impl PluginFactory for HangingFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on(
            "hang",
            Arc::new(HangingHandler(Arc::clone(&self.1))),
            EventOptions::default(),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct PanickingHandler;

#[async_trait]
impl EventHandler for PanickingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        panic!("event panic evidence");
    }
}

#[derive(Debug)]
struct CountingHandler(Arc<AtomicUsize>);

#[async_trait]
impl EventHandler for CountingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct OnceEventFactory {
    spec: FactorySpec,
    completed: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for OnceEventFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on(
            "parallel-once",
            Arc::new(CountingHandler(Arc::clone(&self.completed))),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn concurrent_parallel_dispatches_claim_a_once_listener_exactly_once() {
    let runtime = Runtime::default();
    let completed = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            Arc::new(OnceEventFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("parallel-once", "1")),
                completed: Arc::clone(&completed),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let first = runtime.root();
    let second = runtime.root();
    let (first, second) = tokio::join!(
        first.dispatch("parallel-once", DispatchMode::Parallel, Value::Null),
        second.dispatch("parallel-once", DispatchMode::Parallel, Value::Null),
    );

    assert_eq!(first.unwrap().invoked + second.unwrap().invoked, 1);
    assert_eq!(completed.load(Ordering::Acquire), 1);
}

#[derive(Debug)]
struct EventFactory {
    spec: FactorySpec,
    event: &'static str,
    handler: Arc<dyn EventHandler>,
}

#[async_trait]
impl PluginFactory for EventFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on(
            self.event,
            Arc::clone(&self.handler),
            EventOptions::default(),
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn event_future_construction_and_recursive_payload_panics_are_contained() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(EventFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("event-constructor-panic", "1")),
                event: "event-constructor-panic",
                handler: Arc::new(PanicWhileConstructingEventFuture),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let dispatch = std::panic::AssertUnwindSafe(runtime.root().dispatch(
        "event-constructor-panic",
        DispatchMode::Emit,
        Value::Null,
    ))
    .catch_unwind()
    .await;
    let result = match dispatch {
        Ok(result) => result,
        Err(payload) => {
            // The RED implementation returns the hostile payload to this
            // public seam. Do not recursively destroy it in the test process.
            std::mem::forget(payload);
            panic!("event future construction panic escaped its callback boundary");
        }
    };
    assert_eq!(
        result.unwrap_err(),
        MetaError::Event("event handler panicked".to_owned()),
    );
    assert_eq!(runtime.resource_snapshot().event_callbacks.current, 0);
}

#[tokio::test]
async fn completed_event_future_drop_retains_exact_caller_effect_authority() {
    let runtime = Runtime::default();
    let result = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let listener = runtime
        .root()
        .apply(
            Arc::new(EventFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("event-future-drop-listener", "1")),
                event: "event-future-drop-caller-effect",
                handler: Arc::new(DeferOnFutureDropHandler {
                    result: Arc::clone(&result),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&listener).await;
    let context = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("event-future-drop-caller", "1")),
                context: Arc::clone(&context),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&caller).await;
    let context = context
        .lock()
        .expect("event caller context capture poisoned")
        .clone()
        .expect("caller activation captures its Context");

    assert_eq!(
        context
            .dispatch(
                "event-future-drop-caller-effect",
                DispatchMode::Emit,
                Value::Null,
            )
            .await
            .unwrap()
            .invoked,
        1,
    );
    assert_eq!(
        result
            .lock()
            .expect("event drop result poisoned")
            .take()
            .expect("completed event future Drop attempted registration"),
        Ok(()),
    );
    assert_eq!(cleanups.load(Ordering::Acquire), 0);
    assert!(caller.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(listener.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn panicking_event_handlers_become_errors_without_cancelling_parallel_siblings() {
    let runtime = Runtime::default();
    let completed = Arc::new(AtomicUsize::new(0));
    for (name, handler) in [
        (
            "panicking-event",
            Arc::new(PanickingHandler) as Arc<dyn EventHandler>,
        ),
        (
            "parallel-sibling",
            Arc::new(CountingHandler(Arc::clone(&completed))) as Arc<dyn EventHandler>,
        ),
    ] {
        runtime
            .root()
            .apply(
                Arc::new(EventFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin(name, "1")),
                    event: "panic",
                    handler,
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }

    let error = runtime
        .root()
        .dispatch("panic", DispatchMode::Parallel, Value::Null)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("panicked"), "{error}");
    assert_eq!(completed.load(Ordering::Acquire), 1);
    assert!(runtime.snapshot().terminal.is_none());
}

#[derive(Debug)]
struct DelayedHandler(std::time::Duration);

#[async_trait]
impl EventHandler for DelayedHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        tokio::time::sleep(self.0).await;
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct OnceDelayedFactory {
    spec: FactorySpec,
    delay: std::time::Duration,
}

#[async_trait]
impl PluginFactory for OnceDelayedFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on(
            "once-timeout",
            Arc::new(DelayedHandler(self.delay)),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )?;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_once_listener_is_still_consumed_by_its_single_attempt() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(OnceDelayedFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("once-timeout", "1")),
                delay: std::time::Duration::from_millis(30),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    assert_eq!(
        runtime
            .root()
            .dispatch("once-timeout", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::Timeout("event dispatch")
    );
    assert_eq!(
        runtime
            .root()
            .dispatch("once-timeout", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn one_event_deadline_bounds_the_complete_serial_dispatch() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    for name in ["first-delayed-event", "second-delayed-event"] {
        runtime
            .root()
            .apply(
                Arc::new(EventFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin(name, "1")),
                    event: "deadline",
                    handler: Arc::new(DelayedHandler(std::time::Duration::from_millis(15))),
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }

    let error = runtime
        .root()
        .dispatch("deadline", DispatchMode::Emit, Value::Null)
        .await
        .unwrap_err();
    assert_eq!(error, MetaError::Timeout("event dispatch"));
}

#[tokio::test]
async fn event_callback_deadline_bounds_the_handler() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("timeout-listener", "1")),
                context: Arc::new(Mutex::new(None)),
                listener: Arc::new(Mutex::new(None)),
                dispose_during_activation: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(HangingFactory(
                FactorySpec::new(FactoryIdentity::builtin("hanging", "1")),
                Arc::clone(&entered),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let result = runtime
        .root()
        .dispatch("hang", DispatchMode::Emit, Value::Null)
        .await;
    assert_eq!(result.unwrap_err(), MetaError::Timeout("event dispatch"));
}

#[tokio::test]
async fn event_handler_future_drop_panic_is_a_bounded_event_error() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(EventFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("panicking-future-drop", "1")),
                event: "panicking-future-drop",
                handler: Arc::new(PanickingFutureDropHandler),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let result = std::panic::AssertUnwindSafe(runtime.root().dispatch(
        "panicking-future-drop",
        DispatchMode::Emit,
        Value::Null,
    ))
    .catch_unwind()
    .await
    .expect("handler future destruction escaped the event boundary");
    assert_eq!(
        result.unwrap_err(),
        MetaError::Event("event handler panicked".to_owned())
    );
}

#[derive(Debug)]
struct OversizedOutcomeHandler;

#[async_trait]
impl EventHandler for OversizedOutcomeHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue(Value::String(
            "0123456789".to_owned(),
        )))
    }
}

#[tokio::test]
async fn handler_produced_event_values_obey_the_frame_bound() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_message_bytes: 8,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(EventFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("oversized-event-outcome", "1")),
                event: "oversized-outcome",
                handler: Arc::new(OversizedOutcomeHandler),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    assert_eq!(
        runtime
            .root()
            .dispatch("oversized-outcome", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 8 }
    );
}

#[path = "event_invariants/foundation.rs"]
mod foundation;

#[tokio::test]
async fn dispatch_rejects_an_oversized_input_before_listener_lookup() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_message_bytes: 8,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    assert_eq!(
        runtime
            .root()
            .dispatch(
                "no-listeners",
                DispatchMode::Emit,
                Value::String("0123456789".to_owned()),
            )
            .await
            .unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 8 }
    );
}
