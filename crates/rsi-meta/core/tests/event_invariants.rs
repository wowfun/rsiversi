use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    Context, DeadlineLimits, DispatchMode, EventHandler, EventOptions, EventOutcome,
    FactoryIdentity, InvocationContext, MetaError, PayloadLimits, PluginDescriptor, PluginFactory,
    Result, Runtime, RuntimeLimits,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

mod support;

use support::{ListenerCaptureFactory, wait_active};

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
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(name, "1")),
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
struct HangingFactory(PluginDescriptor, Arc<Notify>);

#[async_trait]
impl PluginFactory for HangingFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.on(
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
    descriptor: PluginDescriptor,
    completed: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for OnceEventFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.on(
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("parallel-once", "1")),
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
    descriptor: PluginDescriptor,
    event: &'static str,
    handler: Arc<dyn EventHandler>,
}

#[async_trait]
impl PluginFactory for EventFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.on(
            self.event,
            Arc::clone(&self.handler),
            EventOptions::default(),
        )?;
        Ok(())
    }
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
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(name, "1")),
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
    descriptor: PluginDescriptor,
    delay: std::time::Duration,
}

#[async_trait]
impl PluginFactory for OnceDelayedFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.on(
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("once-timeout", "1")),
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
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(name, "1")),
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "timeout-listener",
                    "1",
                )),
                context: Arc::new(Mutex::new(None)),
                listener: Arc::new(Mutex::new(None)),
                remove_while_staged: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(HangingFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("hanging", "1")),
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "panicking-future-drop",
                    "1",
                )),
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
            maximum_frame_bytes: 8,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(EventFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "oversized-event-outcome",
                    "1",
                )),
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
            maximum_frame_bytes: 8,
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
