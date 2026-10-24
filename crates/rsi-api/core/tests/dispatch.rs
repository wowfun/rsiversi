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

#[tokio::test]
async fn single_key_discovery_remains_available_when_data_is_full() {
    let registry = Arc::new(registry());
    let data = spec(
        "saturated-data",
        OperationClass::Data,
        OperationEffect::Read,
    );
    let registration = registry.register(data.clone(), Gate::new()).unwrap();
    let connection = rsi_api::ConnectionApi::register(
        registry.clone(),
        registry.as_ref(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        rsi_api_protocol::HostEpoch::from_bytes([2; 16]),
    )
    .unwrap();
    let held: Vec<_> = (0..16)
        .map(|_| registry.admit(&data.call(), CallOrigin::Local).unwrap())
        .collect();
    assert!(matches!(
        registry.admit(
            &rsi_api_protocol::operations_operation().call(),
            CallOrigin::Local
        ),
        Err(ApiError::Capacity)
    ));
    for key in [Some(data.key()), None] {
        let call = registry
            .admit(
                &rsi_api_protocol::operation_operation().call(),
                CallOrigin::Local,
            )
            .unwrap();
        let input = call
            .input_budget()
            .encode(
                &rsi_api_protocol::OperationsQuery { key: key.clone() },
                1024,
            )
            .unwrap();
        let result = call.invoke(input).await;
        if key.is_none() {
            assert!(matches!(result, Err(ApiError::Invalid(_))));
            continue;
        }
        let ApiOutput::Reply(reply) = result.unwrap() else {
            panic!("single descriptor")
        };
        let catalog: rsi_api_protocol::OperationCatalog =
            serde_json::from_slice(reply.json.as_bytes()).unwrap();
        assert_eq!(catalog.operations(), &[data.clone()]);
    }
    drop(held);
    registration.close().await;
    connection.close().await;
    registry.close().await;
}

#[tokio::test]
async fn filtered_discovery_does_not_reveal_denied_protocol_or_retirement() {
    let registry = Arc::new(registry());
    let mut private = spec("private-query", OperationClass::Data, OperationEffect::Read);
    private.access = rsi_api_protocol::OperationAccess::Local;
    let registration = registry.register(private.clone(), Gate::new()).unwrap();
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
    let held = registry.admit(&private.call(), CallOrigin::Local).unwrap();
    let mut closing = Box::pin(registration.close());
    for retired in [false, true] {
        if retired {
            assert!(closing.as_mut().now_or_never().is_none());
        }
        for version in [private.protocol.version, private.protocol.version + 1] {
            let mut key = private.key();
            key.protocol.version = version;
            let call = registry
                .admit(
                    &rsi_api_protocol::operation_operation().call(),
                    device.clone(),
                )
                .unwrap();
            let input = call
                .input_budget()
                .encode(&serde_json::json!({"key": key}), 4096)
                .unwrap();
            assert!(matches!(
                call.invoke(input).await,
                Err(ApiError::UnknownOperation)
            ));
        }
    }
    drop(held);
    closing.await;
    connection.close().await;
}

#[tokio::test]
async fn denied_callers_cannot_probe_registered_protocol_or_shape() {
    let registry = registry();
    let mut operation = spec("local", OperationClass::Data, OperationEffect::Read);
    operation.access = rsi_api_protocol::OperationAccess::Local;
    let registration = registry.register(operation.clone(), Gate::new()).unwrap();
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: DeviceId::parse("a".repeat(32)).unwrap(),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    for shape in [
        rsi_api_protocol::CallShape::Read,
        rsi_api_protocol::CallShape::Mutation,
    ] {
        for version in [operation.protocol.version, operation.protocol.version + 1] {
            let mut call = operation.call();
            call.shape = shape;
            call.key.protocol.version = version;
            assert!(matches!(
                registry.admit(&call, origin.clone()),
                Err(ApiError::Unauthorized)
            ));
        }
    }
    registration.close().await;
}

fn spec(name: &str, class: OperationClass, effect: OperationEffect) -> OperationSpec {
    OperationSpec {
        protocol: rsi_api_protocol::connection_protocol(),
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
    let invocation = registry.admit(&operation.call(), origin).unwrap();
    let input = invocation.input_budget().copy(b"null").unwrap();
    invocation.invoke(input)
}

#[tokio::test]
async fn call_shape_and_protocol_are_checked_before_admission() {
    use rsi_api_protocol::CallShape;
    for (class, effect) in [
        (OperationClass::Data, OperationEffect::Read),
        (OperationClass::Control, OperationEffect::Mutation),
        (OperationClass::Subscription, OperationEffect::Read),
    ] {
        let registry = registry();
        let operation = spec("shape", class, effect);
        let gate = Gate::new();
        let registration = registry.register(operation.clone(), gate.clone()).unwrap();
        let mut call = operation.call();
        for shape in [CallShape::Read, CallShape::Mutation, CallShape::Stream] {
            if shape == operation.call().shape {
                continue;
            }
            call.shape = shape;
            assert!(matches!(
                registry.admit(&call, CallOrigin::Local),
                Err(ApiError::CallShapeMismatch)
            ));
        }
        call = operation.call();
        call.key.protocol.version += 1;
        assert!(matches!(
            registry.admit(&call, CallOrigin::Local),
            Err(ApiError::ProtocolMismatch)
        ));
        assert_eq!(gate.entered.available_permits(), 0);
        assert_eq!(gate.completed.load(Ordering::SeqCst), 0);
        drop(
            registry
                .admit(&operation.call(), CallOrigin::Local)
                .unwrap(),
        );
        registration.close().await;
    }
}

#[tokio::test]
async fn policy_changes_do_not_change_call_identity_or_require_dummy_admission() {
    let registry = registry();
    let client = spec("policy", OperationClass::Control, OperationEffect::Mutation);
    let mut host = client.clone();
    host.class = OperationClass::Data;
    host.maximum_request_bytes = 256;
    host.maximum_response_bytes = 512;
    host.access = rsi_api_protocol::OperationAccess::Local;
    let registration = registry.register(host.clone(), Gate::new()).unwrap();
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(registry.admit(&client.call(), CallOrigin::Local).unwrap());
    }
    assert!(matches!(
        registry.admit(&client.call(), CallOrigin::Local),
        Err(ApiError::Capacity)
    ));
    assert_eq!(
        registry
            .operation(&client.key(), &CallOrigin::Local)
            .unwrap(),
        host
    );
    assert!(
        registry
            .input_budget(OperationClass::Data)
            .copy(b"null")
            .is_ok()
    );
    drop(held);
    registration.close().await;
    assert!(matches!(
        registry.operation(&client.key(), &CallOrigin::Local),
        Err(ApiError::UnknownOperation)
    ));
    assert!(
        registry
            .input_budget(OperationClass::Data)
            .copy(b"null")
            .is_ok()
    );
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
            registry.admit(&private.call(), device.clone()),
            Err(ApiError::Unauthorized)
        ));
    }
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(registry.admit(&private.call(), CallOrigin::Local).unwrap());
    }
    assert!(matches!(
        registry.admit(&private.call(), device.clone()),
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        registry.admit(&private.call(), CallOrigin::Local),
        Err(ApiError::Capacity)
    ));
    drop(held);
    for (origin, expected) in [(CallOrigin::Local, true), (device, false)] {
        let operation = rsi_api_protocol::operations_operation();
        let call = registry.admit(&operation.call(), origin).unwrap();
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
    assert!(!registry.contains_operation(&operation.id));
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    assert!(registry.contains_operation(&operation.id));
    drop(start(&registry, &operation, CallOrigin::Local));
    gate.entered.acquire().await.unwrap().forget();
    let mut closing = Box::pin(registration.close());
    assert!(closing.as_mut().now_or_never().is_none());
    assert!(!registry.contains_operation(&operation.id));
    assert!(matches!(
        registry.admit(&operation.call(), CallOrigin::Local),
        Err(ApiError::OperationRetiring)
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
        let invocation = registry.admit(&operation.call(), origin.clone()).unwrap();
        deliveries.push(invocation.retain_admission());
        let input = invocation.input_budget().copy(b"null").unwrap();
        drop(invocation.invoke(input).await.unwrap());
    }
    registration.close().await;
    let replacement = registry.register(operation.clone(), gate).unwrap();
    assert!(matches!(
        registry.admit(&operation.call(), origin.clone()),
        Err(ApiError::Capacity)
    ));
    let last = deliveries.pop().unwrap();
    let clone = last.clone();
    drop(last);
    assert!(matches!(
        registry.admit(&operation.call(), origin.clone()),
        Err(ApiError::Capacity)
    ));
    drop(clone);
    drop(registry.admit(&operation.call(), origin).unwrap());
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
            admitted.push(registry.admit(&operation.call(), origin.clone()).unwrap());
        }
        assert!(matches!(
            registry.admit(&operation.call(), origin),
            Err(ApiError::Capacity)
        ));
        let other = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
            id: DeviceId::parse("b".repeat(32)).unwrap(),
            revoked: tokio_util::sync::CancellationToken::new(),
        });
        admitted.push(registry.admit(&operation.call(), other).unwrap());
        for _ in admitted.len()..global {
            admitted.push(
                registry
                    .admit(&operation.call(), CallOrigin::Local)
                    .unwrap(),
            );
        }
        assert!(matches!(
            registry.admit(&operation.call(), CallOrigin::Local),
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
        drop(
            registry
                .admit(&independent.call(), CallOrigin::Local)
                .unwrap(),
        );
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
        registry.admit(&operation.call(), origin.clone()),
        Err(ApiError::Capacity)
    ));
    gate.release.add_permits(4);
    registration.close().await;
    assert_eq!(gate.completed.load(Ordering::SeqCst), 4);
    let replacement = registry.register(operation.clone(), gate).unwrap();
    for _ in 0..8 {
        drop(registry.admit(&operation.call(), origin.clone()).unwrap());
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
    let first = registry
        .admit(&operation.call(), CallOrigin::Local)
        .unwrap();
    assert!(matches!(
        registry.admit(&operation.call(), CallOrigin::Local),
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
        registry.admit(&operation.call(), CallOrigin::Local),
        Err(ApiError::Capacity)
    ));
    drop(clone);
    drop(
        registry
            .admit(&operation.call(), CallOrigin::Local)
            .unwrap(),
    );
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
    let invocation = registry
        .admit(&operation.call(), CallOrigin::Local)
        .unwrap();
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
    assert_eq!(registry.operations().unwrap(), vec![operation]);
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
    assert_eq!(dispatch.operations().unwrap(), vec![operation.clone()]);
    fiber.dispose().await;
    assert!(
        runtime
            .root()
            .lookup_local::<ApiDispatchContract>()
            .is_none()
    );
    assert!(matches!(
        dispatch.admit(&operation.call(), CallOrigin::Local),
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
    assert!(registry.operations().unwrap().is_empty());
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
    drop(registry.admit(&data.call(), CallOrigin::Local).unwrap());
    retained.pop();
    drop(
        registry
            .admit(&operation.call(), CallOrigin::Local)
            .unwrap(),
    );
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
    let pending_mutation = registry.admit(&mutation.call(), CallOrigin::Local).unwrap();
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
