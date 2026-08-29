use std::{
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::Response,
    routing::any,
};
use futures_util::StreamExt as _;
use rsi_ai_openai::{OpenAiConfig, OpenAiResponsesAdapter};
use rsi_ai_protocol::{
    AiCapability, ContentDelta, LanguageEvent, LanguageModelLimits, LanguageRequest,
    MAX_CONTENT_BLOCKS, Message, PreparedCallSnapshot, RetryPolicy,
};
use rsi_ai_provider::{
    AbortSignal, DeferredLanguageCheckpoint, DeferredStatus, LanguageAdapter, MissingMediaResolver,
    PrepareContext,
};
use rsi_ai_transport::ReqwestTransport;
use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Call {
    method: Method,
    uri: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct ServerState {
    calls: Arc<Mutex<Vec<Call>>>,
    stream_attempts: Arc<Mutex<u32>>,
    submissions: Arc<Mutex<u32>>,
    wide_terminal_batch: bool,
    long_item_id: bool,
    control_padding_bytes: usize,
    incomplete_max_tokens: bool,
    poll_status: Option<&'static str>,
    control_id_bytes: Option<usize>,
}

async fn endpoint(
    State(state): State<ServerState>,
    method: Method,
    uri: Uri,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.calls.lock().expect("calls").push(Call {
        method: method.clone(),
        uri: uri.to_string(),
        body: body.to_vec(),
    });
    match (method, uri.path(), uri.query()) {
        (Method::POST, "/v1/responses", None) => {
            let mut submissions = state.submissions.lock().expect("submissions");
            *submissions += 1;
            Response::new(Body::from(
                json!({"id":format!("resp-bg-{submissions}"),"status":"queued"}).to_string(),
            ))
        }
        (Method::GET, "/v1/responses/resp-bg-1", None) => {
            Response::new(Body::from(
                json!({
                    "id": state.control_id_bytes
                        .map_or_else(|| "resp-bg-1".to_owned(), |bytes| "i".repeat(bytes)),
                    "status": state.poll_status.unwrap_or("in_progress"),
                    "padding":"x".repeat(state.control_padding_bytes)
                })
                .to_string(),
            ))
        }
        (Method::GET, "/v1/responses/resp-bg-1", Some("stream=true")) => {
            first_stream(&state)
        }
        (
            Method::GET,
            "/v1/responses/resp-bg-1",
            Some("stream=true&starting_after=2"),
        ) => Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(
                "data: {\"type\":\"response.completed\",\"sequence_number\":3,\"response\":{\"id\":\"resp-bg-1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n",
            ))
            .expect("resumed stream"),
        (Method::POST, "/v1/responses/resp-bg-1/cancel", None) => Response::new(Body::from(
            json!({"id":"resp-bg-1","status":"cancelled"}).to_string(),
        )),
        other => panic!("unexpected request: {other:?}"),
    }
}

fn first_stream(state: &ServerState) -> Response {
    let mut attempts = state.stream_attempts.lock().expect("attempts");
    *attempts += 1;
    assert_eq!(*attempts, 1);
    if state.incomplete_max_tokens {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(
                "data: {\"type\":\"response.incomplete\",\"sequence_number\":1,\"response\":{\"id\":\"resp-bg-1\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":4}}}\n\n",
            ))
            .expect("incomplete stream");
    }
    if state.wide_terminal_batch {
        let mut stream = String::new();
        for sequence in 1..=MAX_CONTENT_BLOCKS {
            write!(
                stream,
                "data: {}\n\n",
                json!({
                    "type": "response.output_text.delta",
                    "sequence_number": sequence,
                    "item_id": format!("msg-{sequence}"),
                    "content_index": 0,
                    "delta": "x",
                })
            )
            .expect("write content event");
        }
        write!(
            stream,
            "data: {}\n\n",
            json!({
                "type": "response.completed",
                "sequence_number": MAX_CONTENT_BLOCKS + 1,
                "response": {
                    "id": "resp-bg-1",
                    "status": "completed",
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                },
            })
        )
        .expect("write terminal event");
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(stream))
            .expect("wide terminal stream");
    }
    if state.long_item_id {
        let item_id = "i".repeat(rsi_ai_protocol::MAX_ID_BYTES);
        let stream = format!(
            "data: {}\n\n",
            json!({
                "type":"response.output_text.delta",
                "sequence_number":1,
                "item_id":item_id,
                "content_index":u64::MAX,
                "delta":"hello"
            })
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(stream))
            .expect("long item stream");
    }
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(concat!(
            "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"resp-bg-1\",\"status\":\"in_progress\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":2,\"item_id\":\"msg-1\",\"content_index\":0,\"delta\":\"hello\"}\n\n"
        )))
        .expect("first stream")
}

fn context() -> PrepareContext {
    PrepareContext::new(
        PreparedCallSnapshot {
            call_id: "deferred-call".into(),
            deployment_id: "openai".into(),
            provider_family: "openai".into(),
            capability: AiCapability::Language,
            model: "gpt-5".into(),
            protocol: "openai-responses".into(),
            transport: "http".into(),
            endpoint_fingerprint: "test-endpoint".into(),
            config_generation: 1,
            credential_source: Some(CredentialSource::Keyring),
            retry_policy: RetryPolicy::default(),
            request_sha256: "0".repeat(64),
        },
        Some(ResolvedCredential {
            secret: SecretValue::new("openai-secret").expect("secret"),
            source: CredentialSource::Keyring,
        }),
        Arc::new(MissingMediaResolver),
        0,
    )
    .expect("test provider context")
}

async fn model(state: ServerState) -> (OpenAiResponsesAdapter, ServerState) {
    let app = Router::new()
        .route("/v1/responses", any(endpoint))
        .route("/v1/responses/{id}", any(endpoint))
        .route("/v1/responses/{id}/cancel", any(endpoint))
        .with_state(state.clone());
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
    let transport = Arc::new(ReqwestTransport::new().expect("transport"));
    (OpenAiResponsesAdapter::new(config, transport), state)
}

fn request() -> LanguageRequest {
    LanguageRequest::new(vec![Message::user_text("hello").expect("message")]).expect("request")
}

#[tokio::test]
async fn deferred_response_is_checkpointed_and_resumes_after_the_last_sequence() {
    let (model, state) = model(ServerState::default()).await;
    let prepared = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare deferred");
    assert!(state.calls.lock().expect("calls").is_empty());

    let mut handle = prepared.start(AbortSignal::new()).await.expect("submit");
    assert_eq!(handle.checkpoint().operation_id(), "resp-bg-1");
    assert_eq!(handle.checkpoint().status(), DeferredStatus::Queued);
    assert_eq!(
        handle.poll(AbortSignal::new()).await.expect("poll"),
        DeferredStatus::InProgress
    );

    let checkpoint = {
        let mut generation = handle
            .resume(AbortSignal::new())
            .await
            .expect("initial stream");
        let created = generation
            .next()
            .await
            .expect("created batch")
            .expect("created event");
        assert!(created.events().is_empty());
        assert_eq!(created.checkpoint().sequence_number(), Some(1));
        let delta = generation
            .next()
            .await
            .expect("delta batch")
            .expect("delta event");
        assert!(delta.events().iter().any(|event| matches!(
            event,
            LanguageEvent::ContentDelta {
                delta: ContentDelta::Text(text),
                ..
            } if text == "hello"
        )));
        assert_eq!(delta.checkpoint().sequence_number(), Some(2));
        delta.checkpoint().clone()
    };
    let encoded = serde_json::to_vec(&checkpoint).expect("checkpoint JSON");
    let decoded = serde_json::from_slice(&encoded).expect("checkpoint decode");
    let mut restored = model
        .restore_deferred(context(), decoded)
        .await
        .expect("restore without provider I/O");
    let mut resumed = restored
        .resume(AbortSignal::new())
        .await
        .expect("resume stream");
    let completed = resumed
        .next()
        .await
        .expect("completed batch")
        .expect("completed event");
    assert!(matches!(
        completed.events().last(),
        Some(LanguageEvent::Finished { .. })
    ));
    assert_eq!(completed.checkpoint().sequence_number(), Some(3));
    assert_eq!(completed.checkpoint().status(), DeferredStatus::Completed);
    assert!(resumed.next().await.is_none());
    let final_checkpoint = restored.checkpoint();
    assert_eq!(final_checkpoint.sequence_number(), Some(3));
    assert_eq!(final_checkpoint.status(), DeferredStatus::Completed);

    let calls = state.calls.lock().expect("calls");
    let submit_body: Value = serde_json::from_slice(&calls[0].body).expect("submit JSON");
    assert_eq!(submit_body["background"], true);
    assert_eq!(submit_body["stream"], false);
    assert!(calls.iter().any(|call| {
        call.method == Method::GET
            && call.uri == "/v1/responses/resp-bg-1?stream=true&starting_after=2"
    }));
}

#[tokio::test]
async fn deferred_response_cancel_is_an_explicit_single_request() {
    let (model, state) = model(ServerState::default()).await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit first");
    assert_eq!(
        handle
            .cancel(AbortSignal::new())
            .await
            .expect("cancel first"),
        DeferredStatus::Cancelled
    );

    let calls = state.calls.lock().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].method, Method::POST);
    assert_eq!(calls[1].uri, "/v1/responses/resp-bg-1/cancel");
}

#[tokio::test]
async fn deferred_poll_accepts_a_full_response_larger_than_one_mebibyte() {
    let (model, _) = model(ServerState {
        control_padding_bytes: 1024 * 1024,
        ..ServerState::default()
    })
    .await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");

    assert_eq!(
        handle.poll(AbortSignal::new()).await.expect("bounded poll"),
        DeferredStatus::InProgress
    );
}

#[tokio::test]
async fn deferred_control_id_is_bounded_before_typed_projection_retains_it() {
    let (model, _) = model(ServerState {
        control_id_bytes: Some(rsi_ai_protocol::MAX_ID_BYTES + 1),
        ..ServerState::default()
    })
    .await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");

    let error = handle
        .poll(AbortSignal::new())
        .await
        .expect_err("oversized retained id must fail at the projection boundary");
    assert_eq!(error.kind(), rsi_ai_protocol::ErrorKind::OutputValidation);
}

#[tokio::test]
async fn completed_poll_remains_resumable_until_the_terminal_event_cursor() {
    let (model, _) = model(ServerState {
        poll_status: Some("completed"),
        ..ServerState::default()
    })
    .await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");

    assert_eq!(
        handle.poll(AbortSignal::new()).await.expect("poll"),
        DeferredStatus::Completed
    );

    let mut first = handle
        .resume(AbortSignal::new())
        .await
        .expect("completed response still has historical output");
    while let Some(batch) = first.next().await {
        batch.expect("historical batch");
    }
    drop(first);
    assert_eq!(handle.checkpoint().sequence_number(), Some(2));
    assert_eq!(handle.checkpoint().status(), DeferredStatus::Completed);

    let mut terminal = handle
        .resume(AbortSignal::new())
        .await
        .expect("terminal event remains retrievable");
    let completed = terminal
        .next()
        .await
        .expect("terminal batch")
        .expect("terminal event");
    assert!(matches!(
        completed.events().last(),
        Some(LanguageEvent::Finished { .. })
    ));
    drop(terminal);

    assert!(
        handle.resume(AbortSignal::new()).await.is_err(),
        "a consumed terminal cursor cannot be resumed again"
    );
}

#[tokio::test]
async fn max_token_incompletion_has_one_successful_terminal_status() {
    let (model, _) = model(ServerState {
        incomplete_max_tokens: true,
        ..ServerState::default()
    })
    .await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");
    let mut stream = handle.resume(AbortSignal::new()).await.expect("resume");
    let terminal = stream
        .next()
        .await
        .expect("terminal batch")
        .expect("valid terminal");
    assert!(matches!(
        terminal.events().last(),
        Some(LanguageEvent::Finished {
            reason: rsi_ai_protocol::FinishReason::MaxTokens,
            ..
        })
    ));
    assert_eq!(terminal.checkpoint().status(), DeferredStatus::Completed);
}

#[tokio::test]
async fn one_provider_event_can_finish_every_bounded_open_content_block() {
    let (model, _) = model(ServerState {
        wide_terminal_batch: true,
        ..ServerState::default()
    })
    .await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");
    let mut generation = handle.resume(AbortSignal::new()).await.expect("resume");

    for sequence in 1..=MAX_CONTENT_BLOCKS {
        let batch = generation
            .next()
            .await
            .expect("content batch")
            .expect("content event");
        assert_eq!(
            batch.checkpoint().sequence_number(),
            Some(u64::try_from(sequence).expect("sequence"))
        );
    }
    let completed = generation
        .next()
        .await
        .expect("terminal batch")
        .expect("terminal event");
    assert_eq!(completed.events().len(), MAX_CONTENT_BLOCKS + 2);
    assert!(matches!(
        completed.events().last(),
        Some(LanguageEvent::Finished { .. })
    ));
    let terminal_sequence = u64::try_from(MAX_CONTENT_BLOCKS + 1).expect("terminal sequence");
    assert_eq!(
        completed.checkpoint().sequence_number(),
        Some(terminal_sequence)
    );
    assert_eq!(completed.checkpoint().status(), DeferredStatus::Completed);
    assert!(generation.next().await.is_none());
    let checkpoint = handle.checkpoint();
    assert_eq!(checkpoint.sequence_number(), Some(terminal_sequence));
    assert_eq!(checkpoint.status(), DeferredStatus::Completed);
}

#[tokio::test]
async fn checkpoint_emitted_for_maximum_item_id_can_be_restored() {
    let (model, _) = model(ServerState {
        long_item_id: true,
        ..ServerState::default()
    })
    .await;
    let mut handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");
    let mut generation = handle.resume(AbortSignal::new()).await.expect("resume");
    let batch = generation
        .next()
        .await
        .expect("delta batch")
        .expect("delta event");
    let encoded = serde_json::to_vec(batch.checkpoint()).expect("checkpoint JSON");
    let decoded = serde_json::from_slice(&encoded).expect("checkpoint decode");
    model
        .restore_deferred(context(), decoded)
        .await
        .expect("self-emitted checkpoint must restore");
}

#[tokio::test]
async fn durable_checkpoint_decode_revalidates_before_restore() {
    let (model, _) = model(ServerState::default()).await;
    let handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");
    let mut encoded = serde_json::to_value(handle.checkpoint()).expect("checkpoint JSON");
    encoded["operation_id"] = Value::String(String::new());
    assert!(serde_json::from_value::<DeferredLanguageCheckpoint>(encoded).is_err());
}

#[tokio::test]
async fn restore_rejects_legacy_deferred_parser_block_keys() {
    let (model, _) = model(ServerState::default()).await;
    let handle = model
        .prepare_deferred(context(), "gpt-5".into(), request())
        .await
        .expect("prepare")
        .start(AbortSignal::new())
        .await
        .expect("submit");
    let mut encoded = serde_json::to_value(handle.checkpoint()).expect("checkpoint JSON");
    assert_eq!(encoded["provider_state"]["version"], 1);
    encoded["provider_state"]["value"] = json!({
        "next_index": 1,
        "open": [{"key": "msg-1:Text:0", "index": 0, "kind": "text"}],
        "saw_tool": false
    });
    let decoded = serde_json::from_value(encoded).expect("durable decode is structural");

    assert!(model.restore_deferred(context(), decoded).await.is_err());
}
