#![allow(unsafe_code)] // The benchmark-only allocator delegates directly to System.

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{Method, Uri},
    response::Response,
    routing::any,
};
use futures_util::StreamExt as _;
use rsi_ai_openai::{OpenAiConfig, OpenAiResponsesAdapter};
use rsi_ai_protocol::{
    AiCapability, LanguageAssembler, LanguageModelLimits, LanguageOutput, LanguageRequest, Message,
    PreparedCallSnapshot, ProviderExtension, RetryPolicy,
};
use rsi_ai_provider::{
    AbortSignal, DeferredLanguageCheckpoint, DeferredStatus, LanguageAdapter, MissingMediaResolver,
    PrepareContext,
};
use rsi_ai_transport::ReqwestTransport;
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};
use serde_json::json;
use std::alloc::{GlobalAlloc, Layout, System};
use std::convert::Infallible;
use std::hint::black_box;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const DELTAS: usize = 10_000;
const SAMPLES: usize = 10;
const RESPONSE_CHUNK_BYTES: usize = 64 * 1024;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

// SAFETY: Every operation delegates the unchanged layout and pointer to `System`; the atomic
// counter is observational and does not affect allocator ownership or validity.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: This method preserves the `GlobalAlloc` layout contract for `System`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: This method preserves the `GlobalAlloc` layout contract for `System`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` and `layout` came from the delegated `System` allocation.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: The original pair came from `System` and the requested size is forwarded.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone)]
struct ServerState {
    stream: Vec<Bytes>,
}

async fn endpoint(State(state): State<ServerState>, method: Method, uri: Uri) -> Response {
    match (method, uri.path(), uri.query()) {
        (Method::POST, "/v1/responses", None) => Response::new(Body::from(
            json!({"id":"resp-benchmark","status":"queued"}).to_string(),
        )),
        (Method::GET, "/v1/responses/resp-benchmark", None) => Response::new(Body::from(
            json!({"id":"resp-benchmark","status":"in_progress"}).to_string(),
        )),
        (Method::GET, "/v1/responses/resp-benchmark", Some("stream=true")) => {
            let stream =
                futures_util::stream::iter(state.stream.into_iter().map(Ok::<Bytes, Infallible>));
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(stream))
                .expect("stream response")
        }
        request => panic!("unexpected benchmark request: {request:?}"),
    }
}

fn main() {
    if cfg!(debug_assertions) {
        println!("deferred_parser: skipped outside an optimized cargo bench build");
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime");
    runtime.block_on(run());
}

async fn run() {
    let stream = benchmark_stream();
    let app = Router::new()
        .route("/v1/responses", any(endpoint))
        .route("/v1/responses/{id}", any(endpoint))
        .with_state(ServerState {
            stream: stream
                .as_bytes()
                .chunks(RESPONSE_CHUNK_BYTES)
                .map(Bytes::copy_from_slice)
                .collect(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
    let config = OpenAiConfig::new(format!("http://{address}"))
        .and_then(|config| {
            config.with_model_profile(
                "gpt-5",
                LanguageModelLimits::new(200_000, 4_096, 32_768).expect("model limits"),
            )
        })
        .expect("config");
    let model = OpenAiResponsesAdapter::new(
        config,
        Arc::new(ReqwestTransport::new().expect("transport")),
    );

    let baseline = consume_once(&model).await;
    let mut durations = Vec::with_capacity(SAMPLES);
    let mut allocation_counts = Vec::with_capacity(SAMPLES);
    let mut state_identity_changes = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let measured = consume_once(&model).await;
        assert_eq!(measured.output, baseline.output);
        assert_eq!(measured.checkpoint_wire, baseline.checkpoint_wire);
        durations.push(measured.elapsed);
        allocation_counts.push(measured.allocations);
        state_identity_changes.push(measured.state_identity_changes);
    }
    durations.sort_unstable();
    allocation_counts.sort_unstable();
    state_identity_changes.sort_unstable();
    let per_event = |duration: Duration| duration.as_nanos() / (DELTAS as u128 + 1);
    let allocations_per_event = decimal_ratio(
        allocation_counts[SAMPLES / 2],
        u64::try_from(DELTAS + 1).expect("event count fits u64"),
    );
    println!(
        "events={} ns_per_event_min={} ns_per_event_median={} ns_per_event_p95={} \
         allocations_median={} allocations_per_event={} \
         provider_state_identity_changes={} peak_simultaneous_state_handles=2",
        DELTAS + 1,
        per_event(durations[0]),
        per_event(durations[SAMPLES / 2]),
        per_event(durations[SAMPLES.saturating_mul(95).div_ceil(100) - 1]),
        allocation_counts[SAMPLES / 2],
        allocations_per_event,
        state_identity_changes[SAMPLES - 1],
    );
}

fn decimal_ratio(numerator: u64, denominator: u64) -> String {
    let whole = numerator / denominator;
    let thousandths = (numerator % denominator).saturating_mul(1_000) / denominator;
    format!("{whole}.{thousandths:03}")
}

struct Measurement {
    elapsed: Duration,
    allocations: u64,
    state_identity_changes: usize,
    output: LanguageOutput,
    checkpoint_wire: Vec<u8>,
}

async fn consume_once(model: &OpenAiResponsesAdapter) -> Measurement {
    let prepared = model
        .prepare_deferred(context(), "gpt-5".to_owned(), request())
        .await
        .expect("prepare deferred");
    let mut handle = prepared
        .start(AbortSignal::new())
        .await
        .expect("submit deferred");
    assert_eq!(
        handle.poll(AbortSignal::new()).await.expect("poll"),
        DeferredStatus::InProgress
    );
    let mut stream = handle
        .resume(AbortSignal::new())
        .await
        .expect("resume stream");
    let mut assembler = LanguageAssembler::new();
    let mut previous_state: Option<ProviderExtension> = None;
    let mut state_identity_changes = 0_usize;
    let mut last_checkpoint: Option<DeferredLanguageCheckpoint> = None;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Release);
    let started = Instant::now();
    while let Some(batch) = stream.next().await {
        let batch = batch.expect("valid benchmark event");
        for event in batch.events() {
            assembler.push(event).expect("valid normalized sequence");
        }
        if let Some(state) = batch.checkpoint().provider_state() {
            if previous_state
                .as_ref()
                .is_none_or(|previous| !std::ptr::eq(previous.value(), state.value()))
            {
                state_identity_changes += 1;
            }
            previous_state = Some(state.clone());
        }
        last_checkpoint = Some(batch.checkpoint().clone());
    }
    let output = assembler.finish().expect("terminal benchmark output");
    let elapsed = started.elapsed();
    COUNT_ALLOCATIONS.store(false, Ordering::Release);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    black_box(previous_state);
    Measurement {
        elapsed,
        allocations,
        state_identity_changes,
        output,
        checkpoint_wire: serde_json::to_vec(&last_checkpoint.expect("checkpoint"))
            .expect("checkpoint wire"),
    }
}

fn benchmark_stream() -> String {
    use std::fmt::Write as _;
    let mut stream = String::with_capacity(DELTAS * 180);
    for sequence in 1..=DELTAS {
        writeln!(
            stream,
            "data: {}\n",
            json!({
                "type": "response.output_text.delta",
                "sequence_number": sequence,
                "item_id": "message-1",
                "content_index": 0,
                "delta": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            })
        )
        .expect("write delta");
    }
    writeln!(
        stream,
        "data: {}\n",
        json!({
            "type": "response.completed",
            "sequence_number": DELTAS + 1,
            "response": {
                "id": "resp-benchmark",
                "status": "completed",
                "usage": {"input_tokens": 1, "output_tokens": DELTAS},
            },
        })
    )
    .expect("write terminal");
    stream
}

fn context() -> PrepareContext {
    PrepareContext::new(
        PreparedCallSnapshot {
            call_id: "deferred-benchmark".to_owned(),
            deployment_id: "openai".to_owned(),
            provider_family: "openai".to_owned(),
            capability: AiCapability::Language,
            model: "gpt-5".to_owned(),
            protocol: "openai-responses".to_owned(),
            transport: "http".to_owned(),
            endpoint_fingerprint: "benchmark".to_owned(),
            config_generation: 1,
            credential_source: Some(CredentialSource::Keyring),
            retry_policy: RetryPolicy::default(),
            request_sha256: "0".repeat(64),
        },
        Some(ResolvedCredential {
            secret: SecretValue::new("benchmark-secret").unwrap(),
            source: CredentialSource::Keyring,
        }),
        Arc::new(MissingMediaResolver),
        0,
    )
    .expect("benchmark context")
}

fn request() -> LanguageRequest {
    LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap()
}
