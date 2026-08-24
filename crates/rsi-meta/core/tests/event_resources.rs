use async_trait::async_trait;
use rsi_meta::{
    Context, DeadlineLimits, DispatchMode, EventHandler, EventOptions, EventOutcome,
    ExecutionLimits, FactoryIdentity, FiberState, InvocationContext, MetaError, PayloadLimits,
    PluginDescriptor, PluginFactory, Result, Runtime, RuntimeLimits, TopologyLimits,
};
use serde_json::{Value, json};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, Semaphore, mpsc};

mod support;

use support::{ContextCaptureFactory, ListenerCaptureFactory};

#[derive(Debug)]
struct ListenerFactory {
    descriptor: PluginDescriptor,
    event: &'static str,
    handlers: Vec<Arc<dyn EventHandler>>,
    options: EventOptions,
}

#[async_trait]
impl PluginFactory for ListenerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        for handler in &self.handlers {
            context.on(self.event, Arc::clone(handler), self.options)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CapturingFactory {
    descriptor: PluginDescriptor,
    context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for CapturingFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(context);
        Ok(())
    }
}

#[derive(Debug)]
struct NoopHandler;

#[async_trait]
impl EventHandler for NoopHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct PanickingDropHandler;

impl Drop for PanickingDropHandler {
    fn drop(&mut self) {
        panic!("listener handler drop panic evidence");
    }
}

#[async_trait]
impl EventHandler for PanickingDropHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct SolePanickingDropHandlerFactory {
    descriptor: PluginDescriptor,
    handler: Mutex<Option<Arc<dyn EventHandler>>>,
}

#[async_trait]
impl PluginFactory for SolePanickingDropHandlerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let handler = self
            .handler
            .lock()
            .expect("handler holder poisoned")
            .take()
            .expect("the handler is registered once");
        context.on("drop-panic", handler, EventOptions::default())?;
        Ok(())
    }
}

#[tokio::test]
async fn panicking_listener_destructor_cannot_strand_persistent_disposal() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(SolePanickingDropHandlerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "listener-drop-panic",
                    "1",
                )),
                handler: Mutex::new(Some(Arc::new(PanickingDropHandler))),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(2), fiber.dispose())
        .await
        .expect("a listener destructor panic stranded persistent disposal");
    assert_eq!(report.total_failures(), 1);
    assert!(runtime.snapshot().terminal.is_some());
    assert_eq!(runtime.resource_snapshot().cleanup_runs.current, 0);
}

#[derive(Debug)]
struct BlockingHandler {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl EventHandler for BlockingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct ShortErrorHandler;

#[async_trait]
impl EventHandler for ShortErrorHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Err(MetaError::Service("x".to_owned()))
    }
}

#[derive(Debug)]
struct TerminalSpoofingHandler;

#[async_trait]
impl EventHandler for TerminalSpoofingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Err(MetaError::RuntimeTerminal(
            "spoofed event terminal".to_owned(),
        ))
    }
}

#[tokio::test]
async fn handler_errors_cannot_spoof_runtime_terminal_state() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "event-terminal-spoof",
                    "1",
                )),
                event: "event-terminal-spoof",
                handlers: vec![Arc::new(TerminalSpoofingHandler)],
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let error = runtime
        .root()
        .dispatch("event-terminal-spoof", DispatchMode::Emit, Value::Null)
        .await
        .unwrap_err();
    assert!(
        matches!(error, MetaError::Event(ref message) if message.contains("spoofed event terminal")),
        "handler error escaped the event boundary: {error:?}"
    );
    assert!(runtime.snapshot().terminal.is_none());
}

#[tokio::test]
async fn event_error_suffix_budget_is_safe_for_every_small_diagnostic_limit() {
    const OMITTED_SUFFIX: &str = "1 event errors omitted";
    for maximum_diagnostic_bytes in 1..=OMITTED_SUFFIX.len() + 2 {
        let runtime = Runtime::new(RuntimeLimits {
            payloads: PayloadLimits {
                maximum_diagnostic_entries: 1,
                maximum_diagnostic_bytes,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        })
        .unwrap();
        runtime
            .root()
            .apply(
                Arc::new(ListenerFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        format!("small-diagnostic-{maximum_diagnostic_bytes}"),
                        "1",
                    )),
                    event: "small-diagnostic",
                    handlers: vec![Arc::new(ShortErrorHandler), Arc::new(ShortErrorHandler)],
                    options: EventOptions::default(),
                }),
                Value::Null,
            )
            .await
            .unwrap();

        let error = runtime
            .root()
            .dispatch("small-diagnostic", DispatchMode::Parallel, Value::Null)
            .await
            .unwrap_err();
        let MetaError::Event(message) = error else {
            panic!("parallel errors changed category: {error:?}");
        };
        assert!(message.len() <= maximum_diagnostic_bytes);
        assert!(!message.starts_with("; "));
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
        assert!(runtime.shutdown().await.is_complete());
    }
}

#[tokio::test]
async fn dispatch_admission_is_fail_fast_and_reusable() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_event_dispatches: 1,
            maximum_concurrent_event_callbacks: 1,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_secs(1),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "dispatch-admission",
                    "1",
                )),
                event: "dispatch-admission",
                handlers: vec![Arc::new(BlockingHandler {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                })],
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let first_context = runtime.root();
    let first = tokio::spawn(async move {
        first_context
            .dispatch("dispatch-admission", DispatchMode::Emit, Value::Null)
            .await
    });
    entered.notified().await;
    let admitted = runtime.resource_snapshot();
    assert_eq!(admitted.listeners.current, 1);
    assert_eq!(admitted.event_dispatches.current, 1);
    assert_eq!(admitted.event_callbacks.current, 1);

    assert_eq!(
        runtime
            .root()
            .dispatch("dispatch-admission", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "event dispatches",
        }
    );
    assert_eq!(runtime.resource_snapshot().event_dispatches.rejected, 1);

    release.notify_one();
    assert_eq!(first.await.unwrap().unwrap().invoked, 1);
    let released = runtime.resource_snapshot();
    assert_eq!(released.event_dispatches.current, 0);
    assert_eq!(released.event_callbacks.current, 0);
    release.notify_one();
    assert_eq!(
        runtime
            .root()
            .dispatch("dispatch-admission", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        1
    );
}

#[tokio::test]
async fn shutdown_cancels_live_dispatch_and_completes_with_stable_zero_usage() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "shutdown-dispatch",
                    "1",
                )),
                event: "shutdown-dispatch",
                handlers: vec![Arc::new(BlockingHandler {
                    entered: Arc::clone(&entered),
                    release,
                })],
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let dispatch = tokio::spawn({
        let context = runtime.root();
        async move {
            context
                .dispatch("shutdown-dispatch", DispatchMode::Emit, Value::Null)
                .await
        }
    });
    entered.notified().await;
    assert_eq!(runtime.resource_snapshot().event_dispatches.current, 1);
    assert_eq!(runtime.resource_snapshot().event_callbacks.current, 1);

    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(
        dispatch.await.unwrap().unwrap_err(),
        MetaError::RuntimeShuttingDown
    );
    let complete = runtime.resource_snapshot();
    assert_eq!(complete.event_dispatches.current, 0);
    assert_eq!(complete.event_callbacks.current, 0);
    assert_eq!(complete.fibers.current, 0);
    assert_eq!(complete.listeners.current, 0);
    let revision = runtime.snapshot().revision;

    assert_eq!(
        runtime
            .root()
            .dispatch("shutdown-dispatch", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::RuntimeShuttingDown
    );
    tokio::task::yield_now().await;
    assert_eq!(runtime.resource_snapshot(), complete);
    assert_eq!(runtime.snapshot().revision, revision);
}

#[derive(Debug)]
struct PeakHandler {
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
}

struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl EventHandler for PeakHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::AcqRel);
        let _guard = ActiveGuard(Arc::clone(&self.active));
        self.entered.send(()).expect("peak observer remains alive");
        self.release
            .acquire()
            .await
            .expect("release semaphore remains open")
            .forget();
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[tokio::test]
async fn callback_admission_is_global_across_parallel_dispatches() {
    const LISTENERS: usize = 4;
    const DISPATCHES: usize = 3;
    const CALLBACK_LIMIT: usize = 2;

    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_event_dispatches: DISPATCHES,
            maximum_concurrent_event_callbacks: CALLBACK_LIMIT,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_secs(2),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let handlers = (0..LISTENERS)
        .map(|_| {
            Arc::new(PeakHandler {
                active: Arc::clone(&active),
                peak: Arc::clone(&peak),
                entered: entered_tx.clone(),
                release: Arc::clone(&release),
            }) as Arc<dyn EventHandler>
        })
        .collect();
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "callback-admission",
                    "1",
                )),
                event: "callback-admission",
                handlers,
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let dispatches = (0..DISPATCHES)
        .map(|_| {
            let context = runtime.root();
            tokio::spawn(async move {
                context
                    .dispatch("callback-admission", DispatchMode::Parallel, Value::Null)
                    .await
            })
        })
        .collect::<Vec<_>>();
    for _ in 0..CALLBACK_LIMIT {
        tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx.recv())
            .await
            .expect("configured callbacks enter")
            .expect("entry channel remains open");
    }
    tokio::task::yield_now().await;
    assert_eq!(active.load(Ordering::Acquire), CALLBACK_LIMIT);
    assert_eq!(peak.load(Ordering::Acquire), CALLBACK_LIMIT);

    release.add_permits(LISTENERS * DISPATCHES);
    for dispatch in dispatches {
        assert_eq!(dispatch.await.unwrap().unwrap().invoked, LISTENERS);
    }
    assert_eq!(active.load(Ordering::Acquire), 0);
    assert_eq!(peak.load(Ordering::Acquire), CALLBACK_LIMIT);
}

#[derive(Debug)]
struct NotifyingHandler(Arc<Notify>);

#[async_trait]
impl EventHandler for NotifyingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0.notify_one();
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[tokio::test]
async fn parallel_dispatch_refills_a_callback_slot_before_a_slow_sibling_finishes() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_event_callbacks: 2,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            event_dispatch: std::time::Duration::from_secs(1),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let fast_completed = Arc::new(Notify::new());
    let slow_entered = Arc::new(Notify::new());
    let slow_release = Arc::new(Notify::new());
    let refill_entered = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "parallel-lazy-refill",
                    "1",
                )),
                event: "parallel-lazy-refill",
                handlers: vec![
                    Arc::new(NotifyingHandler(Arc::clone(&fast_completed))),
                    Arc::new(BlockingHandler {
                        entered: Arc::clone(&slow_entered),
                        release: Arc::clone(&slow_release),
                    }),
                    Arc::new(NotifyingHandler(Arc::clone(&refill_entered))),
                ],
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let context = runtime.root();
    let dispatch = tokio::spawn(async move {
        context
            .dispatch("parallel-lazy-refill", DispatchMode::Parallel, Value::Null)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), fast_completed.notified())
        .await
        .expect("the fast callback completes");
    tokio::time::timeout(std::time::Duration::from_secs(1), slow_entered.notified())
        .await
        .expect("the slow callback occupies one slot");
    tokio::time::timeout(std::time::Duration::from_secs(1), refill_entered.notified())
        .await
        .expect("the released callback slot is refilled without waiting for the slow sibling");
    assert!(!dispatch.is_finished());

    slow_release.notify_one();
    assert_eq!(dispatch.await.unwrap().unwrap().invoked, 3);
}

#[derive(Debug)]
struct FailingHandler(usize);

#[async_trait]
impl EventHandler for FailingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Err(MetaError::Event(format!(
            "listener-{}-{}",
            self.0,
            "错误".repeat(64)
        )))
    }
}

#[tokio::test]
async fn parallel_error_aggregation_obeys_entry_and_utf8_byte_limits() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_entries: 2,
            maximum_diagnostic_bytes: 80,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "bounded-event-errors",
                    "1",
                )),
                event: "bounded-event-errors",
                handlers: (0..8)
                    .map(|index| Arc::new(FailingHandler(index)) as Arc<dyn EventHandler>)
                    .collect(),
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let MetaError::Event(message) = runtime
        .root()
        .dispatch("bounded-event-errors", DispatchMode::Parallel, Value::Null)
        .await
        .unwrap_err()
    else {
        panic!("parallel listener failures must remain an event error");
    };
    assert!(message.len() <= 80, "{message:?}");
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    assert!(message.contains("truncated") || message.contains("omitted"));
}

#[tokio::test]
async fn sequential_modes_bound_handler_errors_at_the_same_public_seam() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_entries: 2,
            maximum_diagnostic_bytes: 80,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "bounded-sequential-event-errors",
                    "1",
                )),
                event: "bounded-sequential-event-errors",
                handlers: vec![Arc::new(FailingHandler(0))],
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    for mode in [
        DispatchMode::Emit,
        DispatchMode::Serial,
        DispatchMode::Waterfall,
    ] {
        let MetaError::Event(message) = runtime
            .root()
            .dispatch("bounded-sequential-event-errors", mode, Value::Null)
            .await
            .unwrap_err()
        else {
            panic!("handler failures must remain an event error");
        };
        assert!(message.len() <= 80, "{mode:?}: {message:?}");
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    }
}

#[derive(Debug)]
struct NestedOutcomeHandler;

#[async_trait]
impl EventHandler for NestedOutcomeHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue(json!([[[[null]]]])))
    }
}

fn deeply_nested_event_value() -> Value {
    let mut value = Value::Null;
    for _ in 0..100_000 {
        value = Value::Array(vec![value]);
    }
    value
}

fn run_isolated_event_case(test: &str, child_variable: &str, case: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .env(child_variable, case)
        .args(["--exact", test, "--nocapture"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "deep event value case {case:?} crashed the process:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn never_polled_dispatch_futures_destroy_deep_owned_values_without_recursing() {
    const CHILD: &str = "RSI_META_DEEP_UNPOLLED_EVENT_CHILD";
    if let Some(case) = std::env::var_os(CHILD) {
        let runtime = Runtime::default();
        let context = runtime.root();
        match case.to_str().unwrap() {
            "dispatch" => drop(context.dispatch(
                "deep-unpolled",
                DispatchMode::Emit,
                deeply_nested_event_value(),
            )),
            "dispatch-scoped" => drop(context.dispatch_scoped(
                "service",
                "deep-unpolled",
                DispatchMode::Emit,
                deeply_nested_event_value(),
            )),
            other => panic!("unknown deep unpolled event case {other:?}"),
        }
        return;
    }

    let test = "never_polled_dispatch_futures_destroy_deep_owned_values_without_recursing";
    for case in ["dispatch", "dispatch-scoped"] {
        run_isolated_event_case(test, CHILD, case);
    }
}

#[test]
fn polled_dispatch_rejects_a_deep_owned_value_without_recursing() {
    const CHILD: &str = "RSI_META_DEEP_POLLED_EVENT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let runtime = Runtime::default();
        let error = executor
            .block_on(runtime.root().dispatch(
                "deep-polled",
                DispatchMode::Emit,
                deeply_nested_event_value(),
            ))
            .unwrap_err();
        assert!(
            matches!(error, MetaError::InvalidInput(ref message) if message.contains("nesting")),
            "deep event input changed rejection: {error:?}"
        );
        return;
    }

    run_isolated_event_case(
        "polled_dispatch_rejects_a_deep_owned_value_without_recursing",
        CHILD,
        "dispatch",
    );
}

#[derive(Debug)]
struct DeepOutcomeHandler;

#[async_trait]
impl EventHandler for DeepOutcomeHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue(deeply_nested_event_value()))
    }
}

#[test]
fn deep_handler_outcome_is_rejected_without_recursing() {
    const CHILD: &str = "RSI_META_DEEP_EVENT_OUTCOME_CHILD";
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
                    Arc::new(ListenerFactory {
                        descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                            "deep-event-outcome",
                            "1",
                        )),
                        event: "deep-event-outcome",
                        handlers: vec![Arc::new(DeepOutcomeHandler)],
                        options: EventOptions::default(),
                    }),
                    Value::Null,
                )
                .await
                .unwrap();

            let error = runtime
                .root()
                .dispatch("deep-event-outcome", DispatchMode::Emit, Value::Null)
                .await
                .unwrap_err();
            assert!(
                matches!(error, MetaError::InvalidInput(ref message) if message.contains("nesting")),
                "deep event outcome changed rejection: {error:?}"
            );
            assert!(runtime.shutdown().await.is_complete());
        });
        return;
    }

    run_isolated_event_case(
        "deep_handler_outcome_is_rejected_without_recursing",
        CHILD,
        "outcome",
    );
}

#[tokio::test]
async fn event_inputs_and_handler_outcomes_obey_json_shape_limits() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_json_depth: 4,
            maximum_json_nodes: 16,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "event-json-shape",
                    "1",
                )),
                event: "event-json-shape",
                handlers: vec![Arc::new(NestedOutcomeHandler)],
                options: EventOptions::default(),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    assert!(matches!(
        runtime
            .root()
            .dispatch("no-listeners", DispatchMode::Emit, json!([[[[null]]]]))
            .await,
        Err(MetaError::InvalidInput(message)) if message.contains("depth")
    ));
    assert!(matches!(
        runtime
            .root()
            .dispatch("event-json-shape", DispatchMode::Emit, Value::Null)
            .await,
        Err(MetaError::InvalidInput(message)) if message.contains("depth")
    ));
}

#[tokio::test]
async fn once_and_off_churn_reuses_listener_capacity_without_phantom_invocations() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CapturingFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("listener-churn", "1")),
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

    for _ in 0..128 {
        let listener = context
            .on("off-churn", Arc::new(NoopHandler), EventOptions::default())
            .unwrap();
        assert!(context.off(listener));
        assert_eq!(
            context
                .dispatch("off-churn", DispatchMode::Emit, Value::Null)
                .await
                .unwrap()
                .invoked,
            0
        );

        context
            .on(
                "once-churn",
                Arc::new(NoopHandler),
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
        assert_eq!(
            context
                .dispatch("once-churn", DispatchMode::Emit, Value::Null)
                .await
                .unwrap()
                .invoked,
            0
        );
    }
    let listeners = runtime.resource_snapshot().listeners;
    assert_eq!(listeners.current, 0);
    assert_eq!(listeners.high_watermark, 1);
}

#[tokio::test]
async fn once_removal_releases_capacity_while_its_callback_snapshot_is_still_alive() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CapturingFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "listener-snapshot-capacity",
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
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    context
        .on(
            "once-snapshot",
            Arc::new(BlockingHandler {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )
        .unwrap();

    let dispatch_context = context.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_context
            .dispatch("once-snapshot", DispatchMode::Emit, Value::Null)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .expect("once callback enters after its registry claim");
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);

    let replacement = context
        .on(
            "once-snapshot",
            Arc::new(NoopHandler),
            EventOptions::default(),
        )
        .expect("registry capacity is reusable before the old callback returns");
    assert_eq!(runtime.resource_snapshot().listeners.current, 1);

    release.notify_one();
    assert_eq!(dispatch.await.unwrap().unwrap().invoked, 1);
    assert!(context.off(replacement));
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
}

#[tokio::test]
async fn each_exact_listener_removal_advances_the_registry_revision_once() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CapturingFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "listener-removal-revision",
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
    let baseline = runtime.snapshot().revision;

    let explicit = context
        .on("revision", Arc::new(NoopHandler), EventOptions::default())
        .unwrap();
    assert_eq!(runtime.snapshot().revision, baseline + 1);
    assert!(context.off(explicit));
    assert_eq!(runtime.snapshot().revision, baseline + 2);
    assert!(!context.off(explicit));
    assert_eq!(runtime.snapshot().revision, baseline + 2);

    context
        .on(
            "revision",
            Arc::new(NoopHandler),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )
        .unwrap();
    assert_eq!(runtime.snapshot().revision, baseline + 3);
    assert_eq!(
        context
            .dispatch("revision", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        1
    );
    assert_eq!(runtime.snapshot().revision, baseline + 4);
    assert_eq!(
        context
            .dispatch("revision", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
    assert_eq!(runtime.snapshot().revision, baseline + 4);
}

#[path = "event_resources/contract_invariants.rs"]
mod contract_invariants;
