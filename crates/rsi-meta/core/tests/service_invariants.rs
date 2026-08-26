use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, CallId, Capability, ConfigValue, ContractVersion, DeadlineLimits, DispatchMode,
    EventHandler, EventOptions, EventOutcome, ExecutionLimits, FactoryIdentity, FiberState,
    InvocationContext, IsolationId, Message, MetaError, PayloadLimits, PluginFactory,
    PreparedActivation, ProviderChannel, Requirement, Result, Runtime, RuntimeLimits,
    ServiceEndpoint,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

mod support;

#[path = "support/foundation_service.rs"]
mod foundation_service;

use foundation_service::{ConsumerFactory, ProviderFactory};
use support::{EndpointFactory, wait_active};

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct CaptureFactory {
    identity: FactoryIdentity,
    requirement: Requirement,
    slot: Arc<Mutex<Option<Capability>>>,
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring(self.requirement.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.slot.lock().expect("capture poisoned") = Some(
            plan.inject(&self.requirement.key)
                .expect("prepared requirement must be injected")
                .clone(),
        );
        Ok(())
    }
}

#[derive(Debug)]
struct CancellationProbe {
    entered: Arc<Notify>,
    cancelled: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for CancellationProbe {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        channel: ProviderChannel<'_>,
    ) -> Result<()> {
        self.entered.notify_one();
        let cancellation = channel.cancellation();
        cancellation.cancelled().await;
        self.cancelled.notify_one();
        Ok(())
    }
}

async fn captured_service(endpoint: Arc<dyn ServiceEndpoint>) -> (Runtime, Capability) {
    captured_service_with_runtime(Runtime::default(), endpoint).await
}

async fn captured_service_with_runtime(
    runtime: Runtime,
    endpoint: Arc<dyn ServiceEndpoint>,
) -> (Runtime, Capability) {
    let provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("provider", "1"),
                "echo",
                "test.echo",
                V1,
                endpoint,
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(provider.snapshot().state, FiberState::Active));
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("consumer", "1"),
                requirement: Requirement::new("echo", "test.echo", V1),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    let service = slot.lock().expect("capture poisoned").clone().unwrap();
    (runtime, service)
}

#[tokio::test]
async fn dropping_a_call_cancels_its_provider_invocation() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let (_runtime, service) = captured_service(Arc::new(CancellationProbe {
        entered: Arc::clone(&entered),
        cancelled: Arc::clone(&cancelled),
    }))
    .await;
    let call = service.open().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("provider invocation did not start");
    drop(call);
    tokio::time::timeout(std::time::Duration::from_secs(2), cancelled.notified())
        .await
        .expect("provider did not observe cancellation");
}

#[tokio::test]
async fn runtime_bounds_concurrent_service_calls_before_provider_invocation() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_service_calls: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_runtime, service) = captured_service_with_runtime(
        runtime,
        Arc::new(CancellationProbe {
            entered: Arc::clone(&entered),
            cancelled: Arc::clone(&cancelled),
        }),
    )
    .await;
    let first = service.open().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("first provider invocation did not start");
    let second = service.open();
    let rejected = matches!(
        second,
        Err(MetaError::CapacityExhausted {
            resource: "service calls"
        })
    );
    drop(second);
    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(2), cancelled.notified())
        .await
        .expect("first provider invocation did not observe cancellation");
    assert!(rejected, "a second live service call bypassed admission");
    tokio::task::yield_now().await;
    drop(service.open().expect("completed calls release admission"));
}

#[derive(Debug)]
struct SerializedProbe {
    active: AtomicUsize,
    maximum: AtomicUsize,
    calls: AtomicUsize,
    first_entered: Notify,
    second_entered: Notify,
    release_first: Notify,
}

#[async_trait]
impl ServiceEndpoint for SerializedProbe {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum.fetch_max(active, Ordering::AcqRel);
        let call = self.calls.fetch_add(1, Ordering::AcqRel);
        if call == 0 {
            self.first_entered.notify_one();
            self.release_first.notified().await;
        } else {
            self.second_entered.notify_one();
        }
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
        Ok(())
    }
}

#[tokio::test]
async fn safe_rust_provider_callbacks_can_run_concurrently() {
    let probe = Arc::new(SerializedProbe {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        first_entered: Notify::new(),
        second_entered: Notify::new(),
        release_first: Notify::new(),
    });
    let (_runtime, service) = captured_service(probe.clone()).await;
    let first = service.clone();
    let second = service;
    let first_task =
        tokio::spawn(async move { first.invoke(Message::new(b"first".to_vec())).await.unwrap() });
    probe.first_entered.notified().await;
    let second_task = tokio::spawn(async move {
        second
            .invoke(Message::new(b"second".to_vec()))
            .await
            .unwrap()
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        probe.second_entered.notified(),
    )
    .await
    .expect("the second callback was serialized behind the first");
    assert_eq!(probe.maximum.load(Ordering::Acquire), 2);
    probe.release_first.notify_one();
    let (first, second) = tokio::join!(first_task, second_task);
    assert_eq!(first.unwrap().as_bytes(), b"first");
    assert_eq!(second.unwrap().as_bytes(), b"second");
    assert_eq!(probe.maximum.load(Ordering::Acquire), 2);
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
struct CountingEndpoint(Arc<AtomicUsize>);

#[async_trait]
impl ServiceEndpoint for CountingEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        self.0.fetch_add(1, Ordering::AcqRel);
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SharedCallbackFactory {
    identity: FactoryIdentity,
    handler: Arc<dyn EventHandler>,
    endpoint: Arc<dyn ServiceEndpoint>,
}

#[async_trait]
impl PluginFactory for SharedCallbackFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .on("block", Arc::clone(&self.handler), EventOptions::default())?;
        plan.context()
            .provide("echo", "test.echo", V1, Arc::clone(&self.endpoint))?;
        Ok(())
    }
}

#[tokio::test]
async fn service_callbacks_do_not_wait_for_same_generation_event_handlers() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            service_call: std::time::Duration::from_secs(1),
            event_dispatch: std::time::Duration::from_secs(1),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            Arc::new(SharedCallbackFactory {
                identity: FactoryIdentity::builtin("shared-lock", "1"),
                handler: Arc::new(BlockingHandler {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }),
                endpoint: Arc::new(CountingEndpoint(Arc::clone(&calls))),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("queue-client", "1"),
                requirement: Requirement::new("echo", "test.echo", V1),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let dispatch_root = runtime.root();
    let dispatch = tokio::spawn(async move {
        dispatch_root
            .dispatch("block", DispatchMode::Emit, Value::Null)
            .await
    });
    entered.notified().await;
    let service = slot.lock().unwrap().clone().unwrap();
    let response = service
        .invoke(Message::new(b"queued".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"queued");
    assert_eq!(calls.load(Ordering::Acquire), 1);
    release.notify_one();
    dispatch.await.unwrap().unwrap();
}

#[derive(Debug)]
struct TraceLeaf(Arc<Mutex<Option<CallId>>>);

#[async_trait]
impl ServiceEndpoint for TraceLeaf {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        *self.0.lock().expect("trace capture poisoned") = invocation.parent_call_id();
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct TraceBridge {
    enclosing: Arc<Mutex<Option<CallId>>>,
}

#[async_trait]
impl ServiceEndpoint for TraceBridge {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        *self.enclosing.lock().expect("trace capture poisoned") = Some(invocation.call_id());
        while let Some(frame) = channel.recv().await {
            let response = invocation
                .provider_context()
                .service("leaf")?
                .invoke(frame)
                .await?;
            channel.send(response).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn nested_service_trace_names_the_immediate_enclosing_call() {
    let runtime = Runtime::default();
    let leaf_parent = Arc::new(Mutex::new(None));
    let leaf = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("trace-leaf", "1"),
                "leaf",
                "test.leaf",
                V1,
                Arc::new(TraceLeaf(Arc::clone(&leaf_parent))),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(leaf.snapshot().state, FiberState::Active));
    let enclosing = Arc::new(Mutex::new(None));
    let bridge = runtime
        .root()
        .apply(
            Arc::new(
                EndpointFactory::new(
                    FactoryIdentity::builtin("trace-bridge", "1"),
                    "bridge",
                    "test.bridge",
                    V1,
                    Arc::new(TraceBridge {
                        enclosing: Arc::clone(&enclosing),
                    }),
                )
                .requiring(Requirement::new("leaf", "test.leaf", V1)),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(bridge.snapshot().state, FiberState::Active));
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("trace-client", "1"),
                requirement: Requirement::new("bridge", "test.bridge", V1),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    let service = slot.lock().unwrap().clone().unwrap();
    service
        .invoke(Message::new(b"trace".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        *leaf_parent.lock().expect("trace capture poisoned"),
        *enclosing.lock().expect("trace capture poisoned")
    );
}

#[derive(Debug)]
struct DrainEndpoint {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for DrainEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[derive(Debug)]
struct BlockingEndpointDrop {
    entered: Arc<Notify>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Drop for BlockingEndpointDrop {
    fn drop(&mut self) {
        self.entered.notify_one();
        self.release
            .recv()
            .expect("endpoint future drop release remains owned by the test");
    }
}

#[derive(Debug)]
struct DropBlockingEndpoint {
    entered: Arc<Notify>,
    drop_entered: Arc<Notify>,
    drop_release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

#[async_trait]
impl ServiceEndpoint for DropBlockingEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        let drop_release = self
            .drop_release
            .lock()
            .expect("endpoint drop release poisoned")
            .take()
            .expect("endpoint is invoked once");
        let _drop_guard = BlockingEndpointDrop {
            entered: Arc::clone(&self.drop_entered),
            release: drop_release,
        };
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[derive(Debug)]
struct DrainFactory {
    identity: FactoryIdentity,
    endpoint: Arc<dyn ServiceEndpoint>,
    cleaned: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for DrainFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .provide("echo", "test.echo", V1, Arc::clone(&self.endpoint))?;
        let cleaned = Arc::clone(&self.cleaned);
        plan.context().defer(
            "drain cleanup",
            Box::new(move || {
                async move {
                    cleaned.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn retirement_waits_for_callback_exit_and_caller_terminal_observation() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(5),
            service_call: std::time::Duration::from_secs(1),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cleaned = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(DrainFactory {
                identity: FactoryIdentity::builtin("drain", "1"),
                endpoint: Arc::new(DrainEndpoint {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }),
                cleaned: Arc::clone(&cleaned),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("drain-client", "1"),
                requirement: Requirement::new("echo", "test.echo", V1),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let mut call = slot.lock().unwrap().clone().unwrap().open().unwrap();
    entered.notified().await;
    let mut disposal = tokio::spawn(async move { provider.dispose().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut disposal)
            .await
            .is_err()
    );
    assert_eq!(cleaned.load(Ordering::Acquire), 0);
    release.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut disposal)
            .await
            .is_err(),
        "dependent retirement crossed an unread caller terminal",
    );
    assert_eq!(cleaned.load(Ordering::Acquire), 0);
    assert_eq!(runtime.resource_snapshot().service_calls.current, 1);
    assert!(call.recv().await.unwrap().is_none());
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut disposal)
            .await
            .expect("terminal observation did not release dependent retirement")
            .unwrap()
            .is_clean()
    );
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
    drop(call);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_waits_for_endpoint_future_destruction() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let drop_entered = Arc::new(Notify::new());
    let (drop_release, drop_wait) = std::sync::mpsc::sync_channel(1);
    let cleaned = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(DrainFactory {
                identity: FactoryIdentity::builtin("drop-blocking-provider", "1"),
                endpoint: Arc::new(DropBlockingEndpoint {
                    entered: Arc::clone(&entered),
                    drop_entered: Arc::clone(&drop_entered),
                    drop_release: Mutex::new(Some(drop_wait)),
                }),
                cleaned: Arc::clone(&cleaned),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("drop-blocking-client", "1"),
                requirement: Requirement::new("echo", "test.echo", V1),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let call = slot.lock().unwrap().take().unwrap().open().unwrap();
    entered.notified().await;
    let mut disposal = tokio::spawn(async move { provider.dispose().await });

    drop(call);
    drop_entered.notified().await;
    let early = tokio::time::timeout(std::time::Duration::from_millis(50), &mut disposal).await;
    let completed_before_endpoint_drop = early.is_ok();
    drop_release
        .send(())
        .expect("endpoint future remains blocked until released");
    let report = match early {
        Ok(report) => report.unwrap(),
        Err(_) => disposal.await.unwrap(),
    };

    assert!(report.is_clean());
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
    assert!(
        !completed_before_endpoint_drop,
        "provider cleanup completed before endpoint future destruction"
    );
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct BufferedThenHangingEndpoint;

#[async_trait]
impl ServiceEndpoint for BufferedThenHangingEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let request = channel.recv().await.expect("test request");
        channel.send(request).await?;
        std::future::pending().await
    }
}

#[derive(Debug)]
struct RespondThenFailEndpoint;

#[async_trait]
impl ServiceEndpoint for RespondThenFailEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let request = channel.recv().await.expect("test request");
        channel.send(request).await?;
        Err(MetaError::Service("provider terminal facts".to_owned()))
    }
}

#[derive(Debug)]
struct OversizedResponseEndpoint;

#[async_trait]
impl ServiceEndpoint for OversizedResponseEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        channel.recv().await.expect("test request");
        channel.send(Message::new(vec![0; 5])).await
    }
}

#[tokio::test]
async fn provider_response_frames_obey_the_same_bound_as_caller_requests() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_message_bytes: 4,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_runtime, service) =
        captured_service_with_runtime(runtime, Arc::new(OversizedResponseEndpoint)).await;

    let error = service.invoke(Message::new(Vec::new())).await.unwrap_err();
    assert!(
        matches!(error, MetaError::Service(ref message) if message.contains("4-byte")),
        "provider channel error escaped the service boundary: {error:?}"
    );
}

#[tokio::test]
async fn unary_preserves_a_provider_error_after_the_response() {
    let (_runtime, service) = captured_service(Arc::new(RespondThenFailEndpoint)).await;
    assert_eq!(
        service
            .invoke(Message::new(b"response".to_vec()))
            .await
            .unwrap_err(),
        MetaError::Service("provider terminal facts".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_service_error_survives_a_full_response_channel() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            channel_capacity: 1,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            service_call: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_runtime, service) =
        captured_service_with_runtime(runtime, Arc::new(BufferedThenHangingEndpoint)).await;
    let mut call = service.open().unwrap();
    call.send(Message::new(b"buffered".to_vec())).await.unwrap();
    call.finish();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    assert_eq!(call.recv().await.unwrap().unwrap().as_bytes(), b"buffered");
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::Timeout("service call")
    );
}

#[derive(Debug)]
struct InvocationDebugEndpoint;

#[async_trait]
impl ServiceEndpoint for InvocationDebugEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        while channel.recv().await.is_some() {
            channel
                .send(Message::new(format!("{invocation:?}").into_bytes()))
                .await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn invocation_debug_output_does_not_disclose_edge_overlays() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("debug-provider", "1"),
                "echo",
                "test.echo",
                V1,
                Arc::new(InvocationDebugEndpoint),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .intercept("echo", Value::String("top-secret-overlay".to_owned()))
        .unwrap()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("debug-client", "1"),
                requirement: Requirement::new("echo", "test.echo", V1),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = slot.lock().unwrap().clone().unwrap();
    let response = service.invoke(Message::new(Vec::new())).await.unwrap();
    let debug = std::str::from_utf8(response.as_bytes()).unwrap();
    assert!(!debug.contains("top-secret-overlay"), "{debug}");
}

#[path = "service_invariants/foundation.rs"]
mod foundation;
