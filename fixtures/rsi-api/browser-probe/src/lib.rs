#![deny(unsafe_code)]

mod connection;

use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use rsi_api::ApiRegistry;
use rsi_api_protocol::*;
use rsi_meta::{Execution, ResolvedFactory, Runtime, RuntimeLimits, UpdateMode};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;
use wasm_bindgen::prelude::*;

#[derive(Debug)]
struct Gate {
    entered: Semaphore,
    release: Semaphore,
    completed: AtomicUsize,
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
            panic!("finite")
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.encode(&true)?,
            binary: None,
        }))
    }
}
fn gate() -> Arc<Gate> {
    Arc::new(Gate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        completed: AtomicUsize::new(0),
    })
}
fn spec(effect: OperationEffect) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("worker", "probe", 1).unwrap(),
        class: OperationClass::Data,
        effect,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128,
    }
}

fn bytes_probe() {
    let budget = ByteBudget::new(128).unwrap();
    let value: serde_json::Value = serde_json::from_str(
        r#"{"large":18446744073709551615,"exact":123456789012345678901234567890}"#,
    )
    .unwrap();
    let encoded = budget.encode(&value, 128).unwrap();
    assert_eq!(encoded.as_bytes(), value.to_string().as_bytes());
    let charged = budget.used();
    let slice = encoded.slice(0..1).unwrap().into_bytes();
    drop(encoded);
    assert_eq!(budget.used(), charged);
    assert!(budget.reserve(128).is_err());
    drop(slice);
    assert_eq!(budget.used(), 0);
}

fn decoder_probe() {
    use rsi_api_client::{FiniteDecoder, FiniteEncoding, SseDecoder, SseEvent};
    let budget = ByteBudget::new(256).unwrap();
    let exact = b"18446744073709551615";
    let mut decoder =
        FiniteDecoder::new(FiniteEncoding::Binary, budget.reserve(256).unwrap(), None).unwrap();
    let mut wire = (exact.len() as u64).to_be_bytes().to_vec();
    wire.extend_from_slice(&2u64.to_be_bytes());
    wire.extend_from_slice(exact);
    wire.extend_from_slice(&[128, 255]);
    for byte in &wire {
        decoder.push(&[*byte]).unwrap();
    }
    let reply = decoder.finish().unwrap();
    assert_eq!(reply.json.as_bytes(), exact);
    assert_eq!(reply.binary.as_ref().unwrap().as_bytes(), [128, 255]);
    drop(reply);
    assert_eq!(budget.used(), 0);
    let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 128).unwrap();
    let mut items = 0;
    let mut ended = false;
    for byte in b"event: item\ndata: 18446744073709551615\n\nevent: end\ndata: {}\n\n" {
        match decoder.push(&[*byte]).unwrap().1 {
            Some(SseEvent::Item(message)) => {
                assert_eq!(message.json.as_bytes(), exact);
                items += 1;
            }
            Some(SseEvent::End(None)) => ended = true,
            Some(SseEvent::End(Some(_))) => panic!("unexpected stream error"),
            None => {}
        }
    }
    assert_eq!(items, 1);
    assert!(ended);
    decoder.finish().unwrap();
    assert!(
        SseDecoder::new(ByteBudget::default(), budget.clone(), 128)
            .unwrap()
            .finish()
            .is_err()
    );
    assert_eq!(budget.used(), 0);
}

fn compact_sse_probe() {
    use rsi_api_client::{SseDecoder, SseEvent};
    let budget = ByteBudget::default();
    let mut decoder = SseDecoder::new(ByteBudget::default(), budget.clone(), 36 * 1024 * 1024 + 64 * 1024).unwrap();
    let mut held = Vec::new();
    for frame in [b"event: item\ndata: 1\n\n", b"event: item\ndata: 2\n\n"] {
        let (_, Some(SseEvent::Item(item))) = decoder.push(frame).unwrap() else {
            panic!("expected item")
        };
        held.push(item);
    }
    assert_eq!(budget.used(), 2);
    drop(held);
    assert_eq!(budget.used(), 0);
    let receiving = ByteBudget::default();
    let mut facts = SseDecoder::new(receiving.clone(), budget.clone(), 36 * 1024 * 1024 + 64 * 1024).unwrap();
    let mut interactions = SseDecoder::new(receiving.clone(), budget.clone(), 32 * 1024 * 1024 + 64 * 1024).unwrap();
    assert!(facts.push(b"event: item\ndata: tr").unwrap().1.is_none());
    let (_, Some(SseEvent::Item(interaction))) = interactions.push(b"event: item\ndata: true\n\n").unwrap() else {
        panic!("interleaved interaction")
    };
    let (_, Some(SseEvent::Item(fact))) = facts.push(b"ue\n\n").unwrap() else {
        panic!("interleaved fact")
    };
    assert_eq!(receiving.used(), 0);
    assert_eq!(budget.used(), 8);
    drop((fact, interaction));
    assert_eq!(budget.used(), 0);
}

async fn response_driver_probe(execution: Execution) {
    use rsi_api_client::{decode_event_stream, decode_response};
    let budget = ByteBudget::default();
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("content-length", "20".parse().unwrap());
    let source = Box::pin(futures_util::stream::iter([Ok(bytes::Bytes::from_static(
        b"18446744073709551615",
    ))]));
    let result = decode_response(
        200,
        &headers,
        source,
        Some(budget.reserve(128).unwrap().into()),
        false,
        &ByteBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.json.as_bytes(), b"18446744073709551615");
    drop(result);
    assert_eq!(budget.used(), 0);
    let source = Box::pin(futures_util::stream::iter([Ok(bytes::Bytes::from_static(
        b"1",
    ))]));
    assert_eq!(
        decode_response(
            200,
            &headers,
            source,
            Some(budget.reserve(128).unwrap().into()),
            true,
            &ByteBudget::default(),
        )
        .await
        .unwrap_err(),
        ApiError::OutcomeUnknown
    );
    assert_eq!(budget.used(), 0);
    let source = Box::pin(futures_util::stream::iter([Ok(bytes::Bytes::from_static(
        b"event: item\ndata: 1\n\nevent: end\ndata: {}\n\n",
    ))]));
    let mut stream = decode_event_stream(source, ByteBudget::default(), budget.clone(), 128, execution.clone());
    assert_eq!(stream.next().await.unwrap().unwrap().json.as_bytes(), b"1");
    assert!(stream.next().await.is_none());
    drop(stream);
    let source = Box::pin(futures_util::stream::iter([Ok(bytes::Bytes::from_static(
        b"event: item\ndata: 1\n\n",
    ))]));
    let mut stream = decode_event_stream(source, ByteBudget::default(), budget.clone(), 128, execution);
    assert!(stream.next().await.unwrap().is_ok());
    assert!(stream.next().await.unwrap().is_err());
    drop(stream);
    assert_eq!(budget.used(), 0);
}

async fn delivery_probe(execution: Execution) {
    let registry = ApiRegistry::new(execution);
    let gate = gate();
    gate.release.add_permits(4);
    let operation = spec(OperationEffect::Read);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let origin = CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([9; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let mut leases = Vec::new();
    for _ in 0..4 {
        let call = registry.admit(&operation.id, origin.clone()).unwrap();
        leases.push(call.retain_admission());
        let input = call.input_budget().copy(b"null").unwrap();
        drop(call.invoke(input).await.unwrap());
    }
    registration.close().await;
    let replacement = registry.register(operation.clone(), gate).unwrap();
    assert!(matches!(
        registry.admit(&operation.id, origin.clone()),
        Err(ApiError::Capacity)
    ));
    leases.pop();
    drop(registry.admit(&operation.id, origin).unwrap());
    drop(leases);
    replacement.close().await;
    registry.close().await;
}

async fn mutation_probe(execution: Execution) {
    let registry = ApiRegistry::new(execution);
    let gate = gate();
    let operation = spec(OperationEffect::Mutation);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: DeviceId::parse("a".repeat(32)).unwrap(),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    for _ in 0..4 {
        let call = registry.admit(&operation.id, origin.clone()).unwrap();
        let input = call.input_budget().copy(b"null").unwrap();
        drop(call.invoke(input));
    }
    gate.entered.acquire_many(4).await.unwrap().forget();
    assert!(matches!(
        registry.admit(&operation.id, origin),
        Err(ApiError::Capacity)
    ));
    let mut closing = Box::pin(registration.close());
    assert!(closing.as_mut().now_or_never().is_none());
    gate.release.add_permits(4);
    closing.await;
    assert_eq!(gate.completed.load(Ordering::SeqCst), 4);
    registry.close().await;
}

async fn read_probe(execution: Execution) {
    let registry = ApiRegistry::new(execution);
    let gate = gate();
    let operation = spec(OperationEffect::Read);
    let registration = registry.register(operation.clone(), gate.clone()).unwrap();
    let call = registry.admit(&operation.id, CallOrigin::Local).unwrap();
    let input = call.input_budget().copy(b"null").unwrap();
    let waiter = call.invoke(input);
    gate.entered.acquire().await.unwrap().forget();
    registration.close().await;
    assert_eq!(waiter.await.unwrap_err(), ApiError::ShuttingDown);
    assert_eq!(gate.completed.load(Ordering::SeqCst), 0);
    registry.close().await;
}

async fn plugin_probe(execution: Execution) {
    let runtime = Runtime::with_execution(RuntimeLimits::default(), execution).unwrap();
    let factory = ResolvedFactory::linked(
        "api",
        "worker",
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
    let operation = spec(OperationEffect::Read);
    let registration = registrar.register(operation.clone(), gate()).unwrap();
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

#[wasm_bindgen]
pub async fn run_probe() -> std::result::Result<String, JsValue> {
    let execution = Execution::browser().map_err(|error| JsValue::from_str(&error.to_string()))?;
    bytes_probe();
    decoder_probe();
    compact_sse_probe();
    response_driver_probe(execution.clone()).await;
    connection::probe(execution.clone()).await;
    delivery_probe(execution.clone()).await;
    mutation_probe(execution.clone()).await;
    read_probe(execution.clone()).await;
    plugin_probe(execution).await;
    let timers = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(timers.pending_timers, 0);
    assert_eq!(timers.active_alarms, 0);
    Ok(serde_json::json!({ "status": "passed", "pending_timers": timers.pending_timers,
        "active_alarms": timers.active_alarms,
        "cases": ["exact JSON and last-clone leases", "detached mutation ownership", "device quota retention", "read retirement", "plugin withdrawal", "finite and SSE decoder", "delivery lease retirement", "shared negotiated client ownership", "shared finite and SSE response driver"] }).to_string())
}
