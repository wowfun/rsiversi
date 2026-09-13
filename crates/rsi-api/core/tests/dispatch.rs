use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_api::ApiRegistry;
use rsi_api_protocol::{
    ApiContext, ApiDispatch, ApiDispatchContract, ApiError, ApiHandler, ApiMessage, ApiOutput,
    ApiRegistrar, ApiRegistrarContract, ApiResponseCapacity, CallOrigin, DeviceId,
    MAXIMUM_API_BYTES, OperationClass, OperationEffect, OperationId, OperationSpec,
    RequestEncoding, Result, RetainedBytes,
};
use rsi_meta::Execution;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

fn registry() -> ApiRegistry {
    ApiRegistry::new(Execution::native(tokio::runtime::Handle::current()))
}

fn spec(name: &str, class: OperationClass, effect: OperationEffect) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("test", name, 1).unwrap(),
        class,
        effect,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128,
    }
}

#[derive(Debug)]
struct Gate {
    entered: Semaphore,
    release: Semaphore,
    completed: AtomicUsize,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            completed: AtomicUsize::new(0),
        })
    }
}
#[async_trait]
impl ApiHandler for Gate {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        self.completed.fetch_add(1, Ordering::SeqCst);
        let ApiResponseCapacity::Finite(reservation) = output else {
            panic!("finite operation")
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.encode(&true)?,
            binary: None,
        }))
    }
}

fn start(
    registry: &ApiRegistry,
    operation: &OperationSpec,
    origin: CallOrigin,
) -> futures_util::future::BoxFuture<'static, Result<ApiOutput>> {
    let invocation = registry.admit(&operation.id, origin).unwrap();
    let input = invocation.input_budget().copy(b"null").unwrap();
    invocation.invoke(input)
}

#[tokio::test]
async fn local_operation_authority_precedes_admission_and_is_filtered_from_discovery() {
    let registry = Arc::new(registry());
    let gate = Gate::new();
    let mut private = spec(
        "operator",
        OperationClass::Control,
        OperationEffect::Mutation,
    );
    private.access = rsi_api_protocol::OperationAccess::Local;
    let registration = registry.register(private.clone(), gate.clone()).unwrap();
    let connection = rsi_api::ConnectionApi::register(
        registry.clone(),
        registry.as_ref(),
        rsi_api_protocol::EndpointId::from_bytes([2; 16]),
        rsi_api_protocol::HostEpoch::from_bytes([3; 16]),
    )
    .unwrap();
    let device = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: DeviceId::from_bytes([4; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    // Rejected calls consume neither work nor any of the sixteen local control slots.
    for _ in 0..32 {
        assert!(matches!(
            registry.admit(&private.id, device.clone()),
            Err(ApiError::Unauthorized)
        ));
    }
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(registry.admit(&private.id, CallOrigin::Local).unwrap());
    }
    assert!(matches!(
        registry.admit(&private.id, device.clone()),
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        registry.admit(&private.id, CallOrigin::Local),
        Err(ApiError::Capacity)
    ));
    drop(held);
    for (origin, expected) in [(CallOrigin::Local, true), (device, false)] {
        let operation = rsi_api_protocol::operations_operation();
        let call = registry.admit(&operation.id, origin).unwrap();
        let input = call.input_budget().copy(b"{}").unwrap();
        let ApiOutput::Reply(reply) = call.invoke(input).await.unwrap() else {
            panic!("catalog")
        };
        let catalog: rsi_api_protocol::OperationCatalog =
            serde_json::from_slice(reply.json.as_bytes()).unwrap();
        assert_eq!(catalog.operations().contains(&private), expected);
    }
    assert_eq!(gate.entered.available_permits(), 0);
    registration.close().await;
    connection.close().await;
    registry.close().await;
}

#[tokio::test]
async fn mutation_starts_before_waiter_poll_and_retirement_drains_it() {
    let registry = registry();
    let gate = Gate::new();
    let operation = spec("write", OperationClass::Data, OperationEffect::Mutation);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    drop(start(&registry, &operation, CallOrigin::Local));
    gate.entered.acquire().await.unwrap().forget();
    let mut closing = Box::pin(registration.close());
    assert!(closing.as_mut().now_or_never().is_none());
    assert!(matches!(
        registry.admit(&operation.id, CallOrigin::Local),
        Err(ApiError::ShuttingDown)
    ));
    assert!(registry.register(operation.clone(), gate.clone()).is_err());
    gate.release.add_permits(1);
    closing.await;
    assert_eq!(gate.completed.load(Ordering::SeqCst), 1);
    let replacement = registry.register(operation, gate).unwrap();
    replacement.close().await;
    registry.close().await;
}

#[tokio::test]
async fn delivery_admission_lasts_to_last_clone_without_delaying_registration_retirement() {
    let registry = registry();
    let gate = Gate::new();
    gate.release.add_permits(4);
    let operation = spec("delivery", OperationClass::Data, OperationEffect::Read);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: DeviceId::from_bytes([1; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let mut deliveries = Vec::new();
    for _ in 0..4 {
        let invocation = registry.admit(&operation.id, origin.clone()).unwrap();
        deliveries.push(invocation.retain_admission());
        let input = invocation.input_budget().copy(b"null").unwrap();
        drop(invocation.invoke(input).await.unwrap());
    }
    registration.close().await;
    let replacement = registry.register(operation.clone(), gate).unwrap();
    assert!(matches!(
        registry.admit(&operation.id, origin.clone()),
        Err(ApiError::Capacity)
    ));
    let last = deliveries.pop().unwrap();
    let clone = last.clone();
    drop(last);
    assert!(matches!(
        registry.admit(&operation.id, origin.clone()),
        Err(ApiError::Capacity)
    ));
    drop(clone);
    drop(registry.admit(&operation.id, origin).unwrap());
    drop(deliveries);
    replacement.close().await;
    registry.close().await;
}

#[tokio::test]
async fn dropping_read_waiter_releases_admission_without_running_its_write_tail() {
    let registry = registry();
    let gate = Gate::new();
    let operation = spec("read", OperationClass::Data, OperationEffect::Read);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let mut waiting = start(&registry, &operation, CallOrigin::Local);
    assert!(waiting.as_mut().now_or_never().is_none());
    gate.entered.acquire().await.unwrap().forget();
    drop(waiting);
    registration.close().await;
    assert_eq!(gate.completed.load(Ordering::SeqCst), 0);
    registry.close().await;
}

#[tokio::test]
async fn caller_cannot_override_lane_or_effect_and_devices_have_independent_quotas() {
    let registry = registry();
    let gate = Gate::new();
    for (class, global, device) in [
        (OperationClass::Control, 16, 4),
        (OperationClass::Data, 16, 4),
        (OperationClass::Subscription, 64, 16),
    ] {
        let operation = spec("quota", class, OperationEffect::Read);
        let registration = registry.register(operation.clone(), gate.clone()).unwrap();
        let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
            id: DeviceId::parse("a".repeat(32)).unwrap(),
            revoked: tokio_util::sync::CancellationToken::new(),
        });
        let mut admitted = Vec::new();
        for _ in 0..device {
            admitted.push(registry.admit(&operation.id, origin.clone()).unwrap());
        }
        assert!(matches!(
            registry.admit(&operation.id, origin),
            Err(ApiError::Capacity)
        ));
        let other = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
            id: DeviceId::parse("b".repeat(32)).unwrap(),
            revoked: tokio_util::sync::CancellationToken::new(),
        });
        admitted.push(registry.admit(&operation.id, other).unwrap());
        for _ in admitted.len()..global {
            admitted.push(registry.admit(&operation.id, CallOrigin::Local).unwrap());
        }
        assert!(matches!(
            registry.admit(&operation.id, CallOrigin::Local),
            Err(ApiError::Capacity)
        ));
        let independent = spec(
            "other-lane",
            if class == OperationClass::Control {
                OperationClass::Data
            } else {
                OperationClass::Control
            },
            OperationEffect::Read,
        );
        let other_registration = registry
            .register(independent.clone(), gate.clone())
            .unwrap();
        drop(registry.admit(&independent.id, CallOrigin::Local).unwrap());
        drop(admitted);
        other_registration.close().await;
        registration.close().await;
    }
    registry.close().await;
}

#[tokio::test]
async fn disconnected_mutations_retain_device_admission_until_actual_completion() {
    let registry = registry();
    let gate = Gate::new();
    let operation = spec("write", OperationClass::Data, OperationEffect::Mutation);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: DeviceId::parse("a".repeat(32)).unwrap(),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    for _ in 0..4 {
        drop(start(&registry, &operation, origin.clone()));
    }
    gate.entered.acquire_many(4).await.unwrap().forget();
    assert!(matches!(
        registry.admit(&operation.id, origin.clone()),
        Err(ApiError::Capacity)
    ));
    gate.release.add_permits(4);
    registration.close().await;
    assert_eq!(gate.completed.load(Ordering::SeqCst), 4);
    let replacement = registry.register(operation.clone(), gate).unwrap();
    for _ in 0..8 {
        drop(registry.admit(&operation.id, origin.clone()).unwrap());
    }
    replacement.close().await;
}

#[tokio::test]
async fn mutation_response_reservation_precedes_work_and_last_clone_can_outlive_call_slot() {
    let registry = registry();
    let gate = Gate::new();
    let mut operation = spec(
        "large-read",
        OperationClass::Data,
        OperationEffect::Mutation,
    );
    operation.maximum_response_bytes = MAXIMUM_API_BYTES;
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let first = registry.admit(&operation.id, CallOrigin::Local).unwrap();
    assert!(matches!(
        registry.admit(&operation.id, CallOrigin::Local),
        Err(ApiError::Capacity)
    ));
    assert_eq!(
        gate.entered.available_permits(),
        0,
        "rejection must precede domain work"
    );
    let input = first.input_budget().copy(b"null").unwrap();
    gate.release.add_permits(1);
    let ApiOutput::Reply(reply) = first.invoke(input).await.unwrap() else {
        panic!("finite")
    };
    let clone = reply.json.clone().into_bytes();
    drop(reply);
    assert!(matches!(
        registry.admit(&operation.id, CallOrigin::Local),
        Err(ApiError::Capacity)
    ));
    drop(clone);
    drop(registry.admit(&operation.id, CallOrigin::Local).unwrap());
    registration.close().await;
}

#[tokio::test]
async fn retirement_cancels_a_read_even_when_its_waiter_is_retained_unpolled() {
    let registry = registry();
    let gate = Gate::new();
    let operation = spec("read", OperationClass::Data, OperationEffect::Read);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let waiter = start(&registry, &operation, CallOrigin::Local);
    gate.entered.acquire().await.unwrap().forget();
    registration.close().await;
    assert_eq!(waiter.await.unwrap_err(), ApiError::ShuttingDown);
    assert_eq!(gate.completed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn retired_body_reception_is_fenced_and_old_lease_cannot_remove_replacement() {
    let registry = registry();
    let gate = Gate::new();
    let operation = spec("write", OperationClass::Data, OperationEffect::Mutation);
    let old = registry.register(operation.clone(), gate.clone()).unwrap();
    let invocation = registry.admit(&operation.id, CallOrigin::Local).unwrap();
    let retiring = invocation.retiring();
    let input = invocation.input_budget().copy(b"null").unwrap();
    drop(old);
    assert!(retiring.is_cancelled());
    assert_eq!(
        invocation.invoke(input).await.unwrap_err(),
        ApiError::ShuttingDown
    );
    assert_eq!(gate.entered.available_permits(), 0);
    let replacement = registry.register(operation.clone(), gate.clone()).unwrap();
    assert_eq!(registry.operations(), vec![operation]);
    replacement.close().await;
    registry.close().await;
    assert!(matches!(
        registry.register(
            spec("new", OperationClass::Data, OperationEffect::Read),
            gate
        ),
        Err(ApiError::ShuttingDown)
    ));
}

#[derive(Debug)]
struct Panics;
#[async_trait]
impl ApiHandler for Panics {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        _: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        panic!("native fixture panic");
    }
}

#[tokio::test]
async fn native_panic_releases_mutation_owner_and_reports_an_unknown_outcome() {
    let registry = registry();
    let operation = spec("panic", OperationClass::Data, OperationEffect::Mutation);
    let registration = registry
        .register(operation.clone(), Arc::new(Panics))
        .unwrap();
    assert!(matches!(
        start(&registry, &operation, CallOrigin::Local).await,
        Err(ApiError::OutcomeUnknown)
    ));
    registration.close().await;
    registry.close().await;
}

#[tokio::test]
async fn ordinary_plugin_publishes_one_registry_and_withdrawal_fences_escaped_handles() {
    use rsi_meta::{ResolvedFactory, Runtime, RuntimeLimits, UpdateMode};
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let factory = ResolvedFactory::linked(
        "api",
        "test",
        UpdateMode::Replayable,
        Arc::new(rsi_api::ApiFactory),
    );
    let fiber = runtime
        .root()
        .apply(factory, serde_json::Value::Null)
        .await
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<ApiRegistrarContract>()
        .unwrap();
    let dispatch = runtime
        .root()
        .lookup_local::<ApiDispatchContract>()
        .unwrap();
    let operation = spec("plugin", OperationClass::Control, OperationEffect::Read);
    let registration = registrar.register(operation.clone(), Gate::new()).unwrap();
    assert_eq!(dispatch.operations(), vec![operation.clone()]);
    fiber.dispose().await;
    assert!(
        runtime
            .root()
            .lookup_local::<ApiDispatchContract>()
            .is_none()
    );
    assert!(matches!(
        dispatch.admit(&operation.id, CallOrigin::Local),
        Err(ApiError::ShuttingDown)
    ));
    registration.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct Source {
    produced: Arc<Semaphore>,
    dropped: Arc<AtomicUsize>,
    maximum_items: usize,
    panic_on_poll: bool,
}

struct SourceLifetime(Arc<AtomicUsize>);
impl Drop for SourceLifetime {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ApiHandler for Source {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let ApiResponseCapacity::Subscription { budget, maximum } = output else {
            panic!("subscription")
        };
        let source = (
            0,
            self.maximum_items,
            self.produced.clone(),
            SourceLifetime(self.dropped.clone()),
            budget,
            maximum,
            self.panic_on_poll,
        );
        Ok(ApiOutput::Stream(Box::pin(futures_util::stream::unfold(
            source,
            |(index, limit, produced, lifetime, budget, maximum, panic_on_poll)| async move {
                assert!(!panic_on_poll, "native stream fixture panic");
                if index == limit {
                    return None;
                }
                let reservation = budget.reserve(maximum).unwrap();
                let message = ApiMessage {
                    json: reservation.encode(&index).unwrap(),
                    binary: None,
                };
                produced.add_permits(1);
                Some((
                    Ok(message),
                    (
                        index + 1,
                        limit,
                        produced,
                        lifetime,
                        budget,
                        maximum,
                        panic_on_poll,
                    ),
                ))
            },
        ))))
    }
}

fn source(maximum_items: usize) -> Arc<Source> {
    Arc::new(Source {
        produced: Arc::new(Semaphore::new(0)),
        dropped: Arc::new(AtomicUsize::new(0)),
        maximum_items,
        panic_on_poll: false,
    })
}

#[tokio::test]
async fn stream_backpressure_precedes_materialization_and_retirement_releases_unpolled_stream() {
    use futures_util::StreamExt;
    let registry = registry();
    let source = source(usize::MAX);
    let operation = spec(
        "events",
        OperationClass::Subscription,
        OperationEffect::Read,
    );
    let registration = registry
        .register(operation.clone(), source.clone())
        .unwrap();
    let ApiOutput::Stream(mut stream) = start(&registry, &operation, CallOrigin::Local)
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    source.produced.acquire().await.unwrap().forget();
    tokio::task::yield_now().await;
    assert_eq!(
        source.produced.available_permits(),
        0,
        "slow consumer must block the next domain item"
    );
    registration.close().await;
    assert_eq!(
        source.dropped.load(Ordering::SeqCst),
        1,
        "retained unpolled receiver cannot pin domain work"
    );
    assert_eq!(
        stream.next().await.unwrap().unwrap_err(),
        ApiError::ShuttingDown
    );
    assert!(stream.next().await.is_none());
    // Keeping an ended stream must not retain the operation's admission owner.
    let next = registry.register(operation, source).unwrap();
    next.close().await;
    drop(stream);
}

#[tokio::test]
async fn normal_stream_end_preserves_buffered_items_and_panic_is_not_clean_eof() {
    use futures_util::StreamExt;
    let registry = registry();
    let operation = spec(
        "events",
        OperationClass::Subscription,
        OperationEffect::Read,
    );
    let source = source(2);
    let registration = registry
        .register(operation.clone(), source.clone())
        .unwrap();
    let ApiOutput::Stream(mut stream) = start(&registry, &operation, CallOrigin::Local)
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    for index in 0..2 {
        assert_eq!(
            stream.next().await.unwrap().unwrap().json.as_bytes(),
            index.to_string().as_bytes()
        );
    }
    assert!(stream.next().await.is_none());
    assert_eq!(source.dropped.load(Ordering::SeqCst), 1);
    registration.close().await;
    let panic_source = Arc::new(Source {
        panic_on_poll: true,
        ..Arc::try_unwrap(source).unwrap()
    });
    let registration = registry.register(operation.clone(), panic_source).unwrap();
    let ApiOutput::Stream(mut stream) = start(&registry, &operation, CallOrigin::Local)
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    assert!(matches!(
        stream.next().await,
        Some(Err(ApiError::Backend(_)))
    ));
    assert!(stream.next().await.is_none());
    registration.close().await;
}

#[tokio::test]
async fn registry_and_operation_validation_reject_before_accepting_excess_work() {
    let registry = registry();
    let gate = Gate::new();
    let mut invalid = spec(
        "invalid",
        OperationClass::Subscription,
        OperationEffect::Mutation,
    );
    assert!(registry.register(invalid.clone(), gate.clone()).is_err());
    invalid.class = OperationClass::Control;
    invalid.maximum_response_bytes = 128 * 1024 + 1;
    assert!(registry.register(invalid, gate.clone()).is_err());
    let mut registrations = Vec::new();
    for index in 0..2048 {
        registrations.push(
            registry
                .register(
                    spec(
                        &format!("op-{index}"),
                        OperationClass::Data,
                        OperationEffect::Read,
                    ),
                    gate.clone(),
                )
                .unwrap(),
        );
    }
    assert!(matches!(
        registry.register(
            spec("overflow", OperationClass::Data, OperationEffect::Read),
            gate.clone()
        ),
        Err(ApiError::Capacity)
    ));
    registrations.pop();
    let last = registry
        .register(
            spec("overflow", OperationClass::Data, OperationEffect::Read),
            gate,
        )
        .unwrap();
    drop(registrations);
    last.close().await;
    assert!(registry.operations().is_empty());
    registry.close().await;
}

#[derive(Debug)]
struct ControlBody;
#[async_trait]
impl ApiHandler for ControlBody {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let ApiResponseCapacity::Finite(reservation) = output else {
            panic!("finite")
        };
        // Simulates an already-existing domain payload; the response copy is admitted.
        let mut bytes = vec![b' '; 128 * 1024];
        bytes[0] = b'"';
        bytes[128 * 1024 - 1] = b'"';
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.copy(&bytes)?,
            binary: None,
        }))
    }
}

#[tokio::test]
async fn completed_control_responses_keep_the_two_mib_pool_without_using_data_capacity() {
    let registry = registry();
    let mut operation = spec("control", OperationClass::Control, OperationEffect::Read);
    operation.maximum_response_bytes = 128 * 1024;
    let registration = registry
        .register(operation.clone(), Arc::new(ControlBody))
        .unwrap();
    let mut retained = Vec::new();
    for _ in 0..16 {
        retained.push(
            start(&registry, &operation, CallOrigin::Local)
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        start(&registry, &operation, CallOrigin::Local).await,
        Err(ApiError::Capacity)
    ));
    let data = spec("data", OperationClass::Data, OperationEffect::Read);
    let data_registration = registry.register(data.clone(), Gate::new()).unwrap();
    drop(registry.admit(&data.id, CallOrigin::Local).unwrap());
    retained.pop();
    drop(registry.admit(&operation.id, CallOrigin::Local).unwrap());
    registration.close().await;
    data_registration.close().await;
    drop(retained);
    registry.close().await;
}

#[tokio::test]
async fn small_retained_read_and_mutation_do_not_exclude_maximum_sized_reads() {
    let registry = registry();
    let gate = Gate::new();
    gate.release.add_permits(3);
    let mut read = spec("large-read", OperationClass::Data, OperationEffect::Read);
    read.maximum_response_bytes = MAXIMUM_API_BYTES;
    let registered = registry.register(read.clone(), gate.clone()).unwrap();
    let mutation = spec(
        "small-mutation",
        OperationClass::Data,
        OperationEffect::Mutation,
    );
    let write_registration = registry.register(mutation.clone(), gate.clone()).unwrap();
    let pending_mutation = registry.admit(&mutation.id, CallOrigin::Local).unwrap();
    let ApiOutput::Reply(reply) = start(&registry, &read, CallOrigin::Local).await.unwrap() else {
        panic!("finite");
    };
    assert_eq!(reply.json.as_bytes(), b"true");
    let last_slice = reply.json.slice(1..).unwrap();
    drop(reply);
    let next = start(&registry, &read, CallOrigin::Local).await.unwrap();
    drop((next, last_slice, pending_mutation));
    registered.close().await;
    write_registration.close().await;
    registry.close().await;
}
