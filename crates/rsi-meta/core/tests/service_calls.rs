use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    Context, ContractVersion, DeadlineLimits, FactoryIdentity, FiberState, InvocationContext,
    MetaError, PayloadLimits, PluginDescriptor, PluginFactory, ProviderChannel, Provision,
    Requirement, Result, Runtime, RuntimeLimits, ServiceEndpoint, ServiceFrame, ServiceHandle,
    ServiceKey, ShutdownOutcome,
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

mod support;

#[path = "support/foundation_service.rs"]
mod foundation_service;

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct CompletionGuard(Arc<Notify>);

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[derive(Debug)]
struct CompletingEndpoint {
    completed: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for CompletingEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let _completion = CompletionGuard(Arc::clone(&self.completed));
        let request = channel.recv().await.expect("test request");
        channel.send(request).await
    }
}

#[derive(Debug)]
struct YieldAfterResponseEndpoint;

#[async_trait]
impl ServiceEndpoint for YieldAfterResponseEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let request = channel.recv().await.expect("test request");
        channel.send(request).await?;
        // Let the caller consume the frame and begin its terminal receive
        // before CallDriver publishes Terminal and wakes shared cancellation.
        tokio::task::yield_now().await;
        Ok(())
    }
}

#[derive(Debug)]
struct SaturatedResponseEndpoint {
    entered: Arc<AtomicUsize>,
    wakeup: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for SaturatedResponseEndpoint {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.wakeup.notify_waiters();
        channel.send(ServiceFrame::new([1_u8])).await
    }
}

#[derive(Debug)]
struct ProviderFactory {
    descriptor: PluginDescriptor,
    endpoint: Arc<dyn ServiceEndpoint>,
}

#[async_trait]
impl PluginFactory for ProviderFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.provide("echo", "test.echo", V1, Arc::clone(&self.endpoint))
    }
}

#[derive(Debug)]
struct ConsumerFactory {
    descriptor: PluginDescriptor,
    captured: Arc<Mutex<Option<ServiceHandle>>>,
}

#[async_trait]
impl PluginFactory for ConsumerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        *self.captured.lock().expect("capture poisoned") = Some(context.service("echo")?);
        Ok(())
    }
}

async fn captured_service(runtime: &Runtime, endpoint: Arc<dyn ServiceEndpoint>) -> ServiceHandle {
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("provider", "1"))
                    .providing(Provision::new("echo", "test.echo", V1)),
                endpoint,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    provider
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(provider.snapshot().state, FiberState::Active));

    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("consumer", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                captured: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));

    captured
        .lock()
        .expect("capture poisoned")
        .clone()
        .expect("service captured")
}

#[tokio::test]
async fn oversized_frame_remains_the_authoritative_error_after_finish() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_frame_bytes: 4,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(PendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    call.finish();

    assert_eq!(
        call.send(ServiceFrame::new([0_u8; 5])).await.unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 4 }
    );
}

#[test]
fn opening_a_service_outside_tokio_uses_the_caller_fiber_executor_without_panicking() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let runtime = Runtime::default();
    let service = executor.block_on(captured_service(
        &runtime,
        Arc::new(CompletingEndpoint {
            completed: Arc::new(Notify::new()),
        }),
    ));
    let (call, panic_hooks) = count_current_thread_panic_hooks(|| service.open());
    assert_eq!(panic_hooks, 0);
    let call = call.expect("call opening uses the caller Fiber executor");

    executor.block_on(async {
        assert_eq!(
            call.unary(ServiceFrame::new(b"outside".to_vec()))
                .await
                .unwrap()
                .as_bytes(),
            b"outside"
        );
        assert!(runtime.shutdown().await.is_complete());
    });
}

#[test]
fn opening_a_service_does_not_probe_an_unrelated_runtime_without_time() {
    let setup_executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let runtime = Runtime::default();
    let service = setup_executor.block_on(captured_service(
        &runtime,
        Arc::new(CancellationAwarePendingEndpoint {
            entered: Arc::new(Notify::new()),
        }),
    ));
    let executor_without_time = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let call = executor_without_time
        .block_on(async { service.open() })
        .expect("call opening uses the caller Fiber executor");
    drop(call);
    setup_executor.block_on(async {
        tokio::time::timeout(Duration::from_secs(1), async {
            while runtime.resource_snapshot().service_calls.current != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the caller Fiber executor did not retire the cancelled call");
        assert!(runtime.shutdown().await.is_complete());
    });
}

fn count_current_thread_panic_hooks<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    static HOOK_LOCK: Mutex<()> = Mutex::new(());

    let _hook_guard = HOOK_LOCK.lock().expect("panic-hook test lock poisoned");
    let thread = std::thread::current().id();
    let invocations = Arc::new(AtomicUsize::new(0));
    let previous = Arc::new(Mutex::new(Some(std::panic::take_hook())));
    let hook_invocations = Arc::clone(&invocations);
    let hook_previous = Arc::clone(&previous);
    std::panic::set_hook(Box::new(move |information| {
        if std::thread::current().id() == thread {
            hook_invocations.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(previous) = hook_previous
            .lock()
            .expect("previous panic hook poisoned")
            .as_ref()
        {
            previous(information);
        }
    }));

    let output = operation();
    drop(std::panic::take_hook());
    std::panic::set_hook(
        previous
            .lock()
            .expect("previous panic hook poisoned")
            .take()
            .expect("previous panic hook remains available"),
    );
    (output, invocations.load(Ordering::Acquire))
}

#[tokio::test]
async fn completed_call_retains_admission_until_the_caller_consumes_its_terminal() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: rsi_meta::ExecutionLimits {
            maximum_concurrent_service_calls: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let completed = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(CompletingEndpoint {
            completed: Arc::clone(&completed),
        }),
    )
    .await;

    let mut first = service.open().unwrap();
    let admitted = runtime.resource_snapshot();
    assert_eq!(admitted.service_calls.current, 1);
    assert_eq!(admitted.service_calls.high_watermark, 1);
    first
        .send(ServiceFrame::new(b"first".to_vec()))
        .await
        .unwrap();
    first.finish();
    completed.notified().await;

    assert!(matches!(
        service.open(),
        Err(MetaError::CapacityExhausted {
            resource: "service calls"
        })
    ));
    assert_eq!(runtime.resource_snapshot().service_calls.rejected, 1);
    assert_eq!(first.recv().await.unwrap().unwrap().as_bytes(), b"first");
    assert!(matches!(
        service.open(),
        Err(MetaError::CapacityExhausted {
            resource: "service calls"
        })
    ));
    assert_eq!(runtime.resource_snapshot().service_calls.rejected, 2);

    assert!(first.recv().await.unwrap().is_none());
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
    drop(
        service
            .open()
            .expect("observing the terminal releases admission"),
    );
    drop(first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn driver_cancellation_never_overtakes_the_terminal_after_a_frame() {
    let runtime = Runtime::default();
    let service = captured_service(&runtime, Arc::new(YieldAfterResponseEndpoint)).await;
    let calls = (0..256_u16)
        .map(|value| {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .open()
                    .unwrap()
                    .unary(ServiceFrame::new(value.to_le_bytes().to_vec()))
                    .await
            })
        })
        .collect::<Vec<_>>();

    for (value, call) in (0..256_u16).zip(calls) {
        assert_eq!(call.await.unwrap().unwrap().as_bytes(), value.to_le_bytes());
    }
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
}

#[derive(Debug)]
struct IndependentCleanupFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for IndependentCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        context.defer(
            "service-independent cleanup",
            Box::new(move || {
                let entered = Arc::clone(&entered);
                async move {
                    entered.notify_one();
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test(start_paused = true)]
async fn unread_terminal_delays_shutdown_without_blocking_independent_cleanup() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            shutdown_wait: Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let completed = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(CompletingEndpoint {
            completed: Arc::clone(&completed),
        }),
    )
    .await;
    let cleanup_entered = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(IndependentCleanupFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "service-independent-root",
                    "1",
                )),
                entered: Arc::clone(&cleanup_entered),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let mut call = service.open().unwrap();
    call.send(ServiceFrame::new(b"shutdown".to_vec()))
        .await
        .unwrap();
    call.finish();
    completed.notified().await;
    assert_eq!(runtime.resource_snapshot().service_calls.current, 1);

    let first = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    tokio::time::timeout(Duration::from_secs(1), cleanup_entered.notified())
        .await
        .expect("unread terminal head-of-line blocked an independent root cleanup");
    assert!(!first.is_finished());

    tokio::time::advance(Duration::from_millis(21)).await;
    assert!(matches!(
        first.await.unwrap(),
        ShutdownOutcome::TimedOut { .. }
    ));
    assert_eq!(runtime.resource_snapshot().service_calls.current, 1);

    drop(call);
    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
    assert!(runtime.snapshot().fibers.is_empty());
}

#[derive(Debug)]
struct PanicBeforeResponse;

#[derive(Debug)]
struct StructuredLongEndpointError;

#[derive(Debug)]
struct TerminalSpoofingEndpoint;

#[async_trait]
impl ServiceEndpoint for TerminalSpoofingEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Err(MetaError::RuntimeTerminal(
            "spoofed service terminal".to_owned(),
        ))
    }
}

#[tokio::test]
async fn endpoint_errors_cannot_spoof_runtime_terminal_state() {
    let runtime = Runtime::default();
    let service = captured_service(&runtime, Arc::new(TerminalSpoofingEndpoint)).await;
    let mut call = service.open().unwrap();
    call.finish();

    let error = call.recv().await.unwrap_err();
    assert!(
        matches!(error, MetaError::Service(ref message) if message.contains("spoofed service terminal")),
        "endpoint error escaped the service boundary: {error:?}"
    );
    assert!(runtime.snapshot().terminal.is_none());
}

#[async_trait]
impl ServiceEndpoint for StructuredLongEndpointError {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Err(MetaError::DuplicateProvider {
            service: ServiceKey::from("界".repeat(4_096)),
        })
    }
}

#[tokio::test]
async fn structured_endpoint_errors_are_bounded_before_terminal_retention() {
    const MAXIMUM_DIAGNOSTIC_BYTES: usize = 16;
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_bytes: MAXIMUM_DIAGNOSTIC_BYTES,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let service = captured_service(&runtime, Arc::new(StructuredLongEndpointError)).await;
    let mut call = service.open().unwrap();
    call.finish();

    let error = call.recv().await.unwrap_err();
    let MetaError::Service(message) = error else {
        panic!("structured endpoint error was not normalized at the service boundary: {error:?}");
    };
    assert!(message.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
}

#[async_trait]
impl ServiceEndpoint for PanicBeforeResponse {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        panic!("provider panic before response")
    }
}

#[derive(Debug)]
struct PanicAfterResponse;

#[async_trait]
impl ServiceEndpoint for PanicAfterResponse {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let request = channel.recv().await.expect("test request");
        channel.send(request).await?;
        panic!("provider panic after response")
    }
}

#[tokio::test]
async fn endpoint_panic_is_an_authoritative_terminal_before_or_after_a_response() {
    let runtime = Runtime::default();
    let service = captured_service(&runtime, Arc::new(PanicBeforeResponse)).await;
    let mut call = service.open().unwrap();
    call.finish();
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::ServiceEndpointPanicked
    );

    let runtime = Runtime::default();
    let service = captured_service(&runtime, Arc::new(PanicAfterResponse)).await;
    assert_eq!(
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"response".to_vec()))
            .await
            .unwrap_err(),
        MetaError::ServiceEndpointPanicked
    );
}

#[derive(Debug)]
struct PanicOnceThenEcho(AtomicUsize);

#[async_trait]
impl ServiceEndpoint for PanicOnceThenEcho {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        assert_ne!(
            self.0.fetch_add(1, Ordering::AcqRel),
            0,
            "first call panics"
        );
        let request = channel.recv().await.expect("test request");
        channel.send(request).await
    }
}

#[tokio::test]
async fn endpoint_panic_does_not_retire_the_provider_generation() {
    let runtime = Runtime::default();
    let service =
        captured_service(&runtime, Arc::new(PanicOnceThenEcho(AtomicUsize::new(0)))).await;

    assert_eq!(
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"first".to_vec()))
            .await
            .unwrap_err(),
        MetaError::ServiceEndpointPanicked
    );
    assert_eq!(
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"second".to_vec()))
            .await
            .unwrap()
            .as_bytes(),
        b"second"
    );
}

#[derive(Debug)]
struct PendingEndpoint {
    entered: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for PendingEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[derive(Debug)]
struct PanickingFutureDropGuard;

impl Drop for PanickingFutureDropGuard {
    fn drop(&mut self) {
        panic!("service future drop panic evidence");
    }
}

#[derive(Debug)]
struct ReadyThenPanickingDropFuture;

impl std::future::Future for ReadyThenPanickingDropFuture {
    type Output = Result<()>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl Drop for ReadyThenPanickingDropFuture {
    fn drop(&mut self) {
        panic!("completed service future drop panic evidence");
    }
}

#[derive(Debug)]
struct ReadyThenPanickingDropEndpoint;

impl ServiceEndpoint for ReadyThenPanickingDropEndpoint {
    fn serve<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _: InvocationContext,
        _: ProviderChannel<'life1>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(ReadyThenPanickingDropFuture)
    }
}

#[derive(Debug)]
struct PanickingFutureDropEndpoint {
    entered: Arc<Notify>,
    cancellation: Arc<Mutex<Option<CancellationToken>>>,
}

#[async_trait]
impl ServiceEndpoint for PanickingFutureDropEndpoint {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        let _guard = PanickingFutureDropGuard;
        *self.cancellation.lock().unwrap() = Some(channel.cancellation());
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn endpoint_future_drop_panic_is_an_authoritative_terminal() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let cancellation = Arc::new(Mutex::new(None));
    let service = captured_service(
        &runtime,
        Arc::new(PanickingFutureDropEndpoint {
            entered: Arc::clone(&entered),
            cancellation,
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;

    call.cancel();
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::ServiceEndpointPanicked
    );
}

#[tokio::test]
async fn completed_endpoint_future_drop_panic_remains_an_authoritative_terminal() {
    let runtime = Runtime::default();
    let service = captured_service(&runtime, Arc::new(ReadyThenPanickingDropEndpoint)).await;

    let mut call = service.open().unwrap();
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::ServiceEndpointPanicked
    );
}

#[tokio::test]
async fn runtime_terminal_remains_authoritative_when_endpoint_future_drop_panics() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let cancellation = Arc::new(Mutex::new(None));
    let service = captured_service(
        &runtime,
        Arc::new(PanickingFutureDropEndpoint {
            entered: Arc::clone(&entered),
            cancellation: Arc::clone(&cancellation),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;
    let driver_cancellation = cancellation
        .lock()
        .unwrap()
        .clone()
        .expect("endpoint captured call cancellation");

    runtime.mark_terminal("terminal evidence");
    driver_cancellation.cancelled().await;
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::RuntimeTerminal("terminal evidence".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn call_deadline_remains_authoritative_when_endpoint_future_drop_panics() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: rsi_meta::DeadlineLimits {
            service_call: Duration::from_millis(20),
            ..rsi_meta::DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let cancellation = Arc::new(Mutex::new(None));
    let service = captured_service(
        &runtime,
        Arc::new(PanickingFutureDropEndpoint {
            entered: Arc::clone(&entered),
            cancellation: Arc::clone(&cancellation),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;
    let driver_cancellation = cancellation
        .lock()
        .unwrap()
        .clone()
        .expect("endpoint captured call cancellation");

    driver_cancellation.cancelled().await;
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::Timeout("service call")
    );
}

#[derive(Debug)]
struct CancellationAwarePendingEndpoint {
    entered: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for CancellationAwarePendingEndpoint {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        self.entered.notify_one();
        channel.cancellation().cancelled().await;
        Ok(())
    }
}

#[tokio::test]
async fn runtime_terminal_is_not_obscured_by_internal_call_cancellation() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(PendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;

    runtime.mark_terminal("terminal evidence");
    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::RuntimeTerminal("terminal evidence".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn call_deadline_is_not_obscured_by_internal_call_cancellation() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: rsi_meta::DeadlineLimits {
            service_call: Duration::from_millis(20),
            ..rsi_meta::DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(PendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;

    assert_eq!(
        call.recv().await.unwrap_err(),
        MetaError::Timeout("service call")
    );
}

#[tokio::test]
async fn cancelling_a_call_wakes_a_sender_blocked_on_channel_capacity() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(CancellationAwarePendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;
    call.send(ServiceFrame::new(vec![1])).await.unwrap();
    {
        let blocked = call.send(ServiceFrame::new(vec![2]));
        tokio::pin!(blocked);
        tokio::select! {
            biased;
            result = &mut blocked => panic!("the full request channel accepted a second frame: {result:?}"),
            () = tokio::task::yield_now() => {}
        }

        call.cancel();
        assert_eq!(blocked.await.unwrap_err(), MetaError::Cancelled);
    }
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
}

#[tokio::test]
async fn cancelling_byte_pressure_records_one_rejection_and_releases_the_reservation() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_frame_bytes: 1,
            maximum_buffered_service_bytes: 1,
            ..rsi_meta::PayloadLimits::default()
        },
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 2,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(CancellationAwarePendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;
    call.send(ServiceFrame::new(vec![1])).await.unwrap();
    assert_eq!(
        runtime.resource_snapshot().buffered_service_bytes.current,
        1
    );

    {
        let blocked = call.send(ServiceFrame::new(vec![2]));
        tokio::pin!(blocked);
        tokio::select! {
            biased;
            result = &mut blocked => panic!("the full byte budget accepted another frame: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        call.cancel();
        assert_eq!(blocked.await.unwrap_err(), MetaError::Cancelled);
    }
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.resource_snapshot().buffered_service_bytes.current != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled request queue retained its byte reservation");
    let released = runtime.resource_snapshot().buffered_service_bytes;
    assert_eq!(released.current, 0);
    assert_eq!(released.high_watermark, 1);
    assert_eq!(released.rejected, 1);
}

#[tokio::test]
async fn cancellation_without_byte_pressure_does_not_record_a_rejection() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_frame_bytes: 1,
            maximum_buffered_service_bytes: 1,
            ..rsi_meta::PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(CancellationAwarePendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;

    call.cancel();
    assert_eq!(
        call.send(ServiceFrame::new(vec![1])).await.unwrap_err(),
        MetaError::Cancelled
    );
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
    let bytes = runtime.resource_snapshot().buffered_service_bytes;
    assert_eq!(bytes.current, 0);
    assert_eq!(bytes.high_watermark, 0);
    assert_eq!(bytes.rejected, 0);
}

#[derive(Debug)]
struct ConcurrentBufferedResponses {
    next_call: AtomicUsize,
    first_sent: Arc<Notify>,
    second_entered: Arc<Notify>,
    second_sent: Arc<Notify>,
    second_completed: Arc<AtomicBool>,
}

#[async_trait]
impl ServiceEndpoint for ConcurrentBufferedResponses {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        let call = self.next_call.fetch_add(1, Ordering::AcqRel);
        if call == 0 {
            channel.send(ServiceFrame::new(vec![1; 4])).await?;
            self.first_sent.notify_one();
        } else {
            self.second_entered.notify_one();
            channel.send(ServiceFrame::new(vec![2; 4])).await?;
            self.second_completed.store(true, Ordering::Release);
            self.second_sent.notify_one();
        }
        Ok(())
    }
}

#[tokio::test]
async fn queued_frames_share_one_runtime_wide_weighted_byte_budget() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_frame_bytes: 4,
            maximum_buffered_service_bytes: 4,
            ..rsi_meta::PayloadLimits::default()
        },
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 2,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let first_sent = Arc::new(Notify::new());
    let second_entered = Arc::new(Notify::new());
    let second_sent = Arc::new(Notify::new());
    let second_completed = Arc::new(AtomicBool::new(false));
    let service = captured_service(
        &runtime,
        Arc::new(ConcurrentBufferedResponses {
            next_call: AtomicUsize::new(0),
            first_sent: Arc::clone(&first_sent),
            second_entered: Arc::clone(&second_entered),
            second_sent: Arc::clone(&second_sent),
            second_completed: Arc::clone(&second_completed),
        }),
    )
    .await;
    let mut first = service.open().unwrap();
    first.finish();
    first_sent.notified().await;
    let first_buffered = runtime.resource_snapshot();
    assert_eq!(first_buffered.buffered_service_bytes.current, 4);
    assert_eq!(first_buffered.buffered_service_bytes.high_watermark, 4);
    let mut second = service.open().unwrap();
    second.finish();
    second_entered.notified().await;
    assert!(
        !second_completed.load(Ordering::Acquire),
        "a second call bypassed the Runtime-wide byte budget"
    );

    assert_eq!(first.recv().await.unwrap().unwrap().as_bytes(), &[1; 4]);
    tokio::time::timeout(Duration::from_secs(2), second_sent.notified())
        .await
        .expect("receiving the first frame did not release its byte permit");
    assert_eq!(second.recv().await.unwrap().unwrap().as_bytes(), &[2; 4]);
    assert_eq!(
        runtime.resource_snapshot().buffered_service_bytes.current,
        0
    );
    assert!(first.recv().await.unwrap().is_none());
    assert!(second.recv().await.unwrap().is_none());
    assert_eq!(runtime.resource_snapshot().service_calls.current, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_byte_handoffs_release_the_ledger_before_waking_senders() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_frame_bytes: 1,
            maximum_buffered_service_bytes: 1,
            ..rsi_meta::PayloadLimits::default()
        },
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(AtomicUsize::new(0));
    let wakeup = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(SaturatedResponseEndpoint {
            entered: Arc::clone(&entered),
            wakeup: Arc::clone(&wakeup),
        }),
    )
    .await;

    for pair in 0..128 {
        let mut first = service.open().unwrap();
        first.finish();
        let first_count = pair * 2 + 1;
        while entered.load(Ordering::Acquire) < first_count {
            wakeup.notified().await;
        }
        while runtime.resource_snapshot().buffered_service_bytes.current == 0 {
            tokio::task::yield_now().await;
        }

        let mut second = service.open().unwrap();
        second.finish();
        let second_count = first_count + 1;
        while entered.load(Ordering::Acquire) < second_count {
            wakeup.notified().await;
        }
        tokio::task::yield_now().await;

        assert_eq!(first.recv().await.unwrap().unwrap().as_bytes(), &[1]);
        assert!(first.recv().await.unwrap().is_none());
        assert_eq!(second.recv().await.unwrap().unwrap().as_bytes(), &[1]);
        assert!(second.recv().await.unwrap().is_none());
    }

    let resources = runtime.resource_snapshot();
    assert_eq!(resources.buffered_service_bytes.current, 0);
    assert_eq!(resources.buffered_service_bytes.high_watermark, 1);
    assert_eq!(resources.service_calls.current, 0);
}

#[tokio::test]
async fn caller_cancellation_wakes_recv_and_the_call_driver_promptly() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: rsi_meta::DeadlineLimits {
            service_call: Duration::from_hours(1),
            ..rsi_meta::DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(PendingEndpoint {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;

    call.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), call.recv())
            .await
            .expect("caller cancellation did not wake ServiceCall::recv")
            .unwrap_err(),
        MetaError::Cancelled,
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.resource_snapshot().service_calls.current != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("caller cancellation did not finish the Runtime-owned CallDriver");
}

#[path = "service_calls/byte_admission.rs"]
mod byte_admission;
#[path = "service_calls/foundation.rs"]
mod foundation;
