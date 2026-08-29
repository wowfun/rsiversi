use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Bail, BailEventHandler, Emit, EmitEventHandler, LocalEvent, LocalEventOptions,
    MAXIMUM_PARALLEL_EVENT_CALLBACKS, MetaError, Parallel, ParallelEventHandler, PluginFactory,
    PreparedActivation, Result, Runtime, RuntimeLimits, Serial, SerialEventHandler, TopologyLimits,
    Waterfall, WaterfallEventHandler,
};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[path = "support/resolver.rs"]
mod resolver;
use resolver::resolved;

struct Emitted;

impl LocalEvent for Emitted {
    const KEY: &'static str = "test.emitted";
    type Value = u32;
    type Error = Infallible;
    type Mode = Emit;
}

#[derive(Debug)]
struct RecordEmit(&'static str, Arc<Mutex<Vec<&'static str>>>);

impl EmitEventHandler<Emitted> for RecordEmit {
    fn handle(&self, _value: &u32) {
        self.1.lock().expect("emit log poisoned").push(self.0);
    }
}

struct ParallelWork;

impl LocalEvent for ParallelWork {
    const KEY: &'static str = "test.parallel";
    type Value = Arc<tokio::sync::Barrier>;
    type Error = &'static str;
    type Mode = Parallel;
}

#[derive(Debug)]
struct ParallelWorker(std::result::Result<(), &'static str>);

#[async_trait]
impl ParallelEventHandler<ParallelWork> for ParallelWorker {
    async fn handle(
        &self,
        barrier: Arc<tokio::sync::Barrier>,
    ) -> std::result::Result<(), &'static str> {
        barrier.wait().await;
        self.0
    }
}

#[derive(Debug)]
struct CountParallel(Arc<AtomicUsize>);

#[async_trait]
impl ParallelEventHandler<ParallelWork> for CountParallel {
    async fn handle(
        &self,
        _barrier: Arc<tokio::sync::Barrier>,
    ) -> std::result::Result<(), &'static str> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ParallelGate {
    started: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    released: AtomicBool,
    notify: Notify,
}

struct BoundedParallelWork;

impl LocalEvent for BoundedParallelWork {
    const KEY: &'static str = "test.parallel-bounded";
    type Value = Arc<ParallelGate>;
    type Error = Infallible;
    type Mode = Parallel;
}

#[derive(Debug)]
struct GatedParallelWorker;

#[async_trait]
impl ParallelEventHandler<BoundedParallelWork> for GatedParallelWorker {
    async fn handle(&self, gate: Arc<ParallelGate>) -> std::result::Result<(), Infallible> {
        gate.started.fetch_add(1, Ordering::AcqRel);
        let active = gate.active.fetch_add(1, Ordering::AcqRel) + 1;
        gate.peak.fetch_max(active, Ordering::AcqRel);
        while !gate.released.load(Ordering::Acquire) {
            let notified = gate.notify.notified();
            if gate.released.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        gate.active.fetch_sub(1, Ordering::AcqRel);
        Ok(())
    }
}

struct SerialQuery;

impl LocalEvent for SerialQuery {
    const KEY: &'static str = "test.serial";
    type Value = u32;
    type Error = &'static str;
    type Mode = Serial;
}

#[derive(Debug)]
struct SerialAnswer(Option<u32>, Arc<Mutex<Vec<u32>>>);

#[async_trait]
impl SerialEventHandler<SerialQuery> for SerialAnswer {
    async fn handle(&self, value: u32) -> std::result::Result<Option<u32>, &'static str> {
        self.1.lock().expect("serial log poisoned").push(value);
        Ok(self.0)
    }
}

struct BailQuery;

impl LocalEvent for BailQuery {
    const KEY: &'static str = "test.bail";
    type Value = u32;
    type Error = &'static str;
    type Mode = Bail;
}

#[derive(Debug)]
struct BailAnswer(Option<u32>, Arc<Mutex<Vec<u32>>>);

impl BailEventHandler<BailQuery> for BailAnswer {
    fn handle(&self, value: &u32) -> std::result::Result<Option<u32>, &'static str> {
        self.1.lock().expect("bail log poisoned").push(*value);
        Ok(self.0)
    }
}

struct WaterfallValue;

impl LocalEvent for WaterfallValue {
    const KEY: &'static str = "test.waterfall";
    type Value = u32;
    type Error = &'static str;
    type Mode = Waterfall;
}

#[derive(Debug)]
struct AddAround(u32);

impl WaterfallEventHandler<WaterfallValue> for AddAround {
    fn handle(
        &self,
        value: u32,
        next: &mut dyn FnMut(u32) -> std::result::Result<u32, &'static str>,
    ) -> std::result::Result<u32, &'static str> {
        Ok(self.0 + next(value)?)
    }
}

#[derive(Debug)]
struct OwnerWithState(Arc<Mutex<Vec<&'static str>>>);

#[async_trait]
impl PluginFactory for OwnerWithState {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::with_state(
            desired.clone(),
            Arc::clone(&self.0),
            0,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> Result<()> {
        let emit_log = plan.take_state::<Arc<Mutex<Vec<&'static str>>>>()?;
        plan.context().on_emit::<Emitted, _>(
            Arc::new(RecordEmit("tail", Arc::clone(&emit_log))),
            LocalEventOptions::default(),
        )?;
        plan.context().on_emit::<Emitted, _>(
            Arc::new(RecordEmit("head", emit_log)),
            LocalEventOptions {
                prepend: true,
                once: true,
            },
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn marker_fixed_modes_preserve_order_bail_parallel_and_middleware_semantics() {
    let runtime = Runtime::default();
    let emit_log = Arc::new(Mutex::new(Vec::new()));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(OwnerWithState(Arc::clone(&emit_log)))),
            Value::Null,
        )
        .await
        .unwrap();
    runtime.root().dispatch_local::<Emitted>(7).unwrap();
    runtime.root().dispatch_local::<Emitted>(8).unwrap();
    assert_eq!(
        emit_log.lock().expect("emit log poisoned").as_slice(),
        &["head", "tail", "tail"]
    );

    let parallel_owner = runtime
        .root()
        .apply(crate::resolved(Arc::new(RegisterParallel)), Value::Null)
        .await
        .unwrap();
    let errors = runtime
        .root()
        .dispatch_local::<ParallelWork>(Arc::new(tokio::sync::Barrier::new(2)))
        .unwrap()
        .await
        .unwrap_err();
    assert_eq!(errors, vec!["first", "second"]);

    let serial_log = Arc::new(Mutex::new(Vec::new()));
    let serial_owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RegisterSerial(Arc::clone(&serial_log)))),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .root()
            .dispatch_local::<SerialQuery>(5)
            .unwrap()
            .await
            .unwrap(),
        Some(11)
    );
    assert_eq!(serial_log.lock().unwrap().as_slice(), &[5, 5]);

    let bail_log = Arc::new(Mutex::new(Vec::new()));
    let bail_owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RegisterBail(Arc::clone(&bail_log)))),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .root()
            .dispatch_local::<BailQuery>(6)
            .unwrap()
            .unwrap(),
        Some(12)
    );
    assert_eq!(bail_log.lock().unwrap().as_slice(), &[6, 6]);

    let waterfall_owner = runtime
        .root()
        .apply(crate::resolved(Arc::new(RegisterWaterfall)), Value::Null)
        .await
        .unwrap();
    assert_eq!(
        runtime
            .root()
            .dispatch_local::<WaterfallValue>(2)
            .unwrap()
            .unwrap(),
        5
    );

    for fiber in [
        waterfall_owner,
        bail_owner,
        serial_owner,
        parallel_owner,
        owner,
    ] {
        assert!(fiber.dispose().await.is_clean());
    }
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct RegisterParallel;

#[async_trait]
impl PluginFactory for RegisterParallel {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on_parallel::<ParallelWork, _>(
            Arc::new(ParallelWorker(Err("first"))),
            LocalEventOptions::default(),
        )?;
        plan.context().on_parallel::<ParallelWork, _>(
            Arc::new(ParallelWorker(Err("second"))),
            LocalEventOptions::default(),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct RegisterBoundedParallel;

#[async_trait]
impl PluginFactory for RegisterBoundedParallel {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        for _ in 0..=MAXIMUM_PARALLEL_EVENT_CALLBACKS {
            plan.context().on_parallel::<BoundedParallelWork, _>(
                Arc::new(GatedParallelWorker),
                LocalEventOptions::default(),
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RegisterBoundedOnceParallel;

#[async_trait]
impl PluginFactory for RegisterBoundedOnceParallel {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        for _ in 0..=MAXIMUM_PARALLEL_EVENT_CALLBACKS {
            plan.context().on_parallel::<BoundedParallelWork, _>(
                Arc::new(GatedParallelWorker),
                LocalEventOptions {
                    prepend: false,
                    once: true,
                },
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RegisterSerial(Arc<Mutex<Vec<u32>>>);

#[async_trait]
impl PluginFactory for RegisterSerial {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        for answer in [None, Some(11), Some(99)] {
            plan.context().on_serial::<SerialQuery, _>(
                Arc::new(SerialAnswer(answer, Arc::clone(&self.0))),
                LocalEventOptions::default(),
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RegisterBail(Arc<Mutex<Vec<u32>>>);

#[async_trait]
impl PluginFactory for RegisterBail {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        for answer in [None, Some(12), Some(99)] {
            plan.context().on_bail::<BailQuery, _>(
                Arc::new(BailAnswer(answer, Arc::clone(&self.0))),
                LocalEventOptions::default(),
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RegisterWaterfall;

#[async_trait]
impl PluginFactory for RegisterWaterfall {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on_waterfall::<WaterfallValue, _>(
            Arc::new(AddAround(1)),
            LocalEventOptions::default(),
        )?;
        plan.context().on_waterfall::<WaterfallValue, _>(
            Arc::new(AddAround(2)),
            LocalEventOptions::default(),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct CaptureContext(Arc<Mutex<Option<rsi_meta::Context>>>);

#[async_trait]
impl PluginFactory for CaptureContext {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.0.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[derive(Debug)]
struct CountEmit(Arc<AtomicUsize>);

impl EmitEventHandler<Emitted> for CountEmit {
    fn handle(&self, _value: &u32) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct PausedEmit {
    entered: Arc<Notify>,
    released: Arc<(Mutex<bool>, Condvar)>,
}

impl EmitEventHandler<Emitted> for PausedEmit {
    fn handle(&self, _value: &u32) {
        self.entered.notify_one();
        let (released, changed) = &*self.released;
        let mut is_released = released.lock().unwrap();
        while !*is_released {
            is_released = changed.wait(is_released).unwrap();
        }
    }
}

#[tokio::test]
async fn typed_listener_disposal_and_isolation_are_exact() {
    let runtime = Runtime::default();
    let public_capture = Arc::new(Mutex::new(None));
    let public_owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&public_capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let public_context = public_capture.lock().unwrap().clone().unwrap();
    let public_count = Arc::new(AtomicUsize::new(0));
    let public_listener = public_context
        .on_emit::<Emitted, _>(
            Arc::new(CountEmit(Arc::clone(&public_count))),
            LocalEventOptions::default(),
        )
        .unwrap();

    let (private_root, isolation) = runtime.root().isolate_event_fresh::<Emitted>().unwrap();
    assert_ne!(isolation, rsi_meta::LocalIsolationId(0));
    let private_capture = Arc::new(Mutex::new(None));
    let private_owner = private_root
        .clone()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&private_capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let private_context = private_capture.lock().unwrap().clone().unwrap();
    let private_count = Arc::new(AtomicUsize::new(0));
    let private_listener = private_context
        .on_emit::<Emitted, _>(
            Arc::new(CountEmit(Arc::clone(&private_count))),
            LocalEventOptions::default(),
        )
        .unwrap();

    runtime.root().dispatch_local::<Emitted>(1).unwrap();
    assert_eq!(public_count.load(Ordering::Acquire), 1);
    assert_eq!(private_count.load(Ordering::Acquire), 0);
    private_root.dispatch_local::<Emitted>(2).unwrap();
    assert_eq!(public_count.load(Ordering::Acquire), 1);
    assert_eq!(private_count.load(Ordering::Acquire), 1);

    assert!(private_listener.dispose().await.is_clean());
    private_root.dispatch_local::<Emitted>(3).unwrap();
    assert_eq!(private_count.load(Ordering::Acquire), 1);
    assert!(public_listener.dispose().await.is_clean());
    runtime.root().dispatch_local::<Emitted>(4).unwrap();
    assert_eq!(public_count.load(Ordering::Acquire), 1);

    assert!(private_owner.dispose().await.is_clean());
    assert!(public_owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn listener_capacity_is_fail_closed_observable_and_reusable() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let first = context
        .on_emit::<Emitted, _>(
            Arc::new(CountEmit(Arc::clone(&count))),
            LocalEventOptions::default(),
        )
        .unwrap();

    assert_eq!(
        context
            .on_emit::<Emitted, _>(
                Arc::new(CountEmit(Arc::clone(&count))),
                LocalEventOptions::default(),
            )
            .unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "event listeners",
        }
    );
    let saturated = runtime.resource_snapshot().listeners;
    assert_eq!(saturated.current, 1);
    assert_eq!(saturated.high_watermark, 1);
    assert_eq!(saturated.rejected, 1);

    assert!(first.dispose().await.is_clean());
    let replacement = context
        .on_emit::<Emitted, _>(Arc::new(CountEmit(count)), LocalEventOptions::default())
        .unwrap();
    assert!(replacement.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn waterfall_listener_depth_is_bounded_per_exact_slot() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 4,
            maximum_waterfall_listeners_per_slot: 2,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let first = context
        .on_waterfall::<WaterfallValue, _>(Arc::new(AddAround(1)), LocalEventOptions::default())
        .unwrap();
    let second = context
        .on_waterfall::<WaterfallValue, _>(Arc::new(AddAround(2)), LocalEventOptions::default())
        .unwrap();

    assert_eq!(
        context
            .on_waterfall::<WaterfallValue, _>(
                Arc::new(AddAround(3)),
                LocalEventOptions::default(),
            )
            .unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "Waterfall listeners in one event slot",
        }
    );
    assert_eq!(runtime.resource_snapshot().listeners.current, 2);

    assert!(first.dispose().await.is_clean());
    let replacement = context
        .on_waterfall::<WaterfallValue, _>(Arc::new(AddAround(4)), LocalEventOptions::default())
        .unwrap();
    assert!(replacement.dispose().await.is_clean());
    assert!(second.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn once_listener_is_claimed_exactly_once_across_concurrent_dispatches() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let listener = context
        .on_emit::<Emitted, _>(
            Arc::new(CountEmit(Arc::clone(&count))),
            LocalEventOptions {
                prepend: false,
                once: true,
            },
        )
        .unwrap();

    let mut dispatches = Vec::new();
    for value in 0..64 {
        let root = runtime.root();
        dispatches.push(tokio::spawn(async move {
            root.dispatch_local::<Emitted>(value).unwrap();
        }));
    }
    for dispatch in dispatches {
        dispatch.await.unwrap();
    }
    assert_eq!(count.load(Ordering::Acquire), 1);
    assert!(listener.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn unpolled_parallel_dispatch_does_not_consume_a_once_listener() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let listener = context
        .on_parallel::<ParallelWork, _>(
            Arc::new(CountParallel(Arc::clone(&count))),
            LocalEventOptions {
                prepend: false,
                once: true,
            },
        )
        .unwrap();

    drop(
        runtime
            .root()
            .dispatch_local::<ParallelWork>(Arc::new(tokio::sync::Barrier::new(1)))
            .unwrap(),
    );
    runtime
        .root()
        .dispatch_local::<ParallelWork>(Arc::new(tokio::sync::Barrier::new(1)))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::Acquire), 1);

    assert!(listener.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn once_claim_and_owner_retirement_complete_without_retaining_each_other() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let entered = Arc::new(Notify::new());
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let listener = context
        .on_emit::<Emitted, _>(
            Arc::new(PausedEmit {
                entered: Arc::clone(&entered),
                released: Arc::clone(&released),
            }),
            LocalEventOptions {
                prepend: false,
                once: true,
            },
        )
        .unwrap();
    let dispatch = tokio::task::spawn_blocking({
        let root = runtime.root();
        move || root.dispatch_local::<Emitted>(1)
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    let retirement = tokio::spawn(async move { owner.dispose().await });
    tokio::task::yield_now().await;
    {
        let (is_released, changed) = &*released;
        *is_released.lock().unwrap() = true;
        changed.notify_all();
    }
    assert!(dispatch.await.unwrap().is_ok());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), retirement)
            .await
            .expect("once claim and owner retirement retained each other")
            .unwrap()
            .is_clean()
    );
    assert!(listener.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_snapshot_keeps_ordinary_membership_after_concurrent_removal() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let entered = Arc::new(Notify::new());
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let first = context
        .on_emit::<Emitted, _>(
            Arc::new(PausedEmit {
                entered: Arc::clone(&entered),
                released: Arc::clone(&released),
            }),
            LocalEventOptions::default(),
        )
        .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let second = context
        .on_emit::<Emitted, _>(
            Arc::new(CountEmit(Arc::clone(&count))),
            LocalEventOptions::default(),
        )
        .unwrap();
    let dispatch = tokio::task::spawn_blocking({
        let root = runtime.root();
        move || root.dispatch_local::<Emitted>(1)
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();

    assert!(second.dispose().await.is_clean());
    {
        let (is_released, changed) = &*released;
        *is_released.lock().unwrap() = true;
        changed.notify_all();
    }
    dispatch.await.unwrap().unwrap();
    assert_eq!(count.load(Ordering::Acquire), 1);
    runtime.root().dispatch_local::<Emitted>(2).unwrap();
    assert_eq!(count.load(Ordering::Acquire), 1);

    assert!(first.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct PanickingEmit;

impl EmitEventHandler<Emitted> for PanickingEmit {
    fn handle(&self, _value: &u32) {
        panic!("listener callback panic evidence");
    }
}

#[derive(Debug)]
struct PanickingParallel;

#[async_trait]
impl ParallelEventHandler<ParallelWork> for PanickingParallel {
    async fn handle(
        &self,
        _value: Arc<tokio::sync::Barrier>,
    ) -> std::result::Result<(), &'static str> {
        panic!("asynchronous listener callback panic evidence");
    }
}

#[tokio::test]
async fn synchronous_listener_panic_propagates_to_the_dispatching_plugin() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let listener = context
        .on_emit::<Emitted, _>(Arc::new(PanickingEmit), LocalEventOptions::default())
        .unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.root().dispatch_local::<Emitted>(1).unwrap();
    }));
    assert!(result.is_err());
    assert!(runtime.snapshot().terminal.is_none());

    assert!(listener.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn asynchronous_listener_panic_propagates_to_the_dispatching_task() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let listener = context
        .on_parallel::<ParallelWork, _>(Arc::new(PanickingParallel), LocalEventOptions::default())
        .unwrap();
    let dispatch = tokio::spawn(
        runtime
            .root()
            .dispatch_local::<ParallelWork>(Arc::new(tokio::sync::Barrier::new(1)))
            .unwrap(),
    );

    assert!(dispatch.await.unwrap_err().is_panic());
    assert!(runtime.snapshot().terminal.is_none());

    assert!(listener.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct PanickingDrop;

impl EmitEventHandler<Emitted> for PanickingDrop {
    fn handle(&self, _value: &u32) {}
}

impl Drop for PanickingDrop {
    fn drop(&mut self) {
        panic!("listener destructor panic evidence");
    }
}

#[tokio::test]
async fn listener_destructor_panic_releases_membership_reports_and_terminalizes() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureContext(Arc::clone(&capture)))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let listener = context
        .on_emit::<Emitted, _>(Arc::new(PanickingDrop), LocalEventOptions::default())
        .unwrap();

    let report = listener.dispose().await;
    assert!(!report.is_clean());
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
    assert!(runtime.snapshot().terminal.is_some());
    let retirement = owner.dispose().await;
    assert!(!retirement.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn parallel_dispatch_bounds_simultaneously_polled_callbacks() {
    let runtime = Runtime::default();
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RegisterBoundedParallel)),
            Value::Null,
        )
        .await
        .unwrap();
    let gate = Arc::new(ParallelGate::default());
    let dispatch = tokio::spawn(
        runtime
            .root()
            .dispatch_local::<BoundedParallelWork>(Arc::clone(&gate))
            .unwrap(),
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while gate.started.load(Ordering::Acquire) < MAXIMUM_PARALLEL_EVENT_CALLBACKS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        gate.started.load(Ordering::Acquire),
        MAXIMUM_PARALLEL_EVENT_CALLBACKS
    );
    assert_eq!(
        gate.peak.load(Ordering::Acquire),
        MAXIMUM_PARALLEL_EVENT_CALLBACKS
    );

    gate.released.store(true, Ordering::Release);
    gate.notify.notify_waiters();
    dispatch.await.unwrap().unwrap();
    assert_eq!(
        gate.started.load(Ordering::Acquire),
        MAXIMUM_PARALLEL_EVENT_CALLBACKS + 1
    );
    assert_eq!(
        gate.peak.load(Ordering::Acquire),
        MAXIMUM_PARALLEL_EVENT_CALLBACKS
    );

    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn cancelled_parallel_dispatch_claims_only_callbacks_admitted_for_polling() {
    let runtime = Runtime::default();
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RegisterBoundedOnceParallel)),
            Value::Null,
        )
        .await
        .unwrap();
    let gate = Arc::new(ParallelGate::default());
    let dispatch = tokio::spawn(
        runtime
            .root()
            .dispatch_local::<BoundedParallelWork>(Arc::clone(&gate))
            .unwrap(),
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while gate.started.load(Ordering::Acquire) < MAXIMUM_PARALLEL_EVENT_CALLBACKS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    dispatch.abort();
    assert!(dispatch.await.unwrap_err().is_cancelled());

    gate.released.store(true, Ordering::Release);
    gate.notify.notify_waiters();
    runtime
        .root()
        .dispatch_local::<BoundedParallelWork>(Arc::clone(&gate))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        gate.started.load(Ordering::Acquire),
        MAXIMUM_PARALLEL_EVENT_CALLBACKS + 1
    );

    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
