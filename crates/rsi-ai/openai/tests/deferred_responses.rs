use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::Response,
    routing::any,
};
use futures_util::StreamExt as _;
use rsi_ai::{DeferredStatus, ModelRef, Registry};
use rsi_ai_auth::{CredentialManager, CredentialRequirement};
use rsi_ai_openai::{OpenAiConfig, OpenAiResponsesAdapter};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, LanguageModelLimits, LanguageRequest, Message};
use rsi_ai_provider::ProviderRegistration;
use rsi_ai_transport::ReqwestTransport;
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
        (Method::GET, "/v1/responses/resp-bg-1", None) => Response::new(Body::from(
            json!({"id":"resp-bg-1","status":"in_progress"}).to_string(),
        )),
        (Method::GET, "/v1/responses/resp-bg-1", Some("stream=true")) => {
            let mut attempts = state.stream_attempts.lock().expect("attempts");
            *attempts += 1;
            assert_eq!(*attempts, 1);
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(concat!(
                    "data: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"resp-bg-1\",\"status\":\"in_progress\"}}\n\n",
                    "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":2,\"item_id\":\"msg-1\",\"content_index\":0,\"delta\":\"hello\"}\n\n"
                )))
                .expect("first stream")
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

fn credential() -> CredentialRequirement {
    CredentialRequirement::new("openai", ["OPENAI_API_KEY"]).expect("requirement")
}

async fn model(state: ServerState) -> (rsi_ai::LanguageModel, ServerState) {
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
    let registration = ProviderRegistration::builder("openai", "openai")
        .expect("registration")
        .with_credential(credential())
        .with_language(OpenAiResponsesAdapter::new(config, transport))
        .build()
        .expect("provider");
    let registry = Registry::builder(
        CredentialManager::builder()
            .with_explicit("openai", "openai-secret")
            .expect("credential")
            .build(),
    )
    .register(registration)
    .expect("registration")
    .build()
    .expect("registry");
    (
        registry
            .language(ModelRef::new("openai", "gpt-5").expect("model"))
            .expect("language"),
        state,
    )
}

fn request() -> LanguageRequest {
    LanguageRequest::new(vec![Message::user_text("hello").expect("message")]).expect("request")
}

#[tokio::test]
async fn deferred_response_is_checkpointed_and_resumes_after_the_last_sequence() {
    let (model, state) = model(ServerState::default()).await;
    let prepared = model
        .prepare_deferred(request())
        .await
        .expect("prepare deferred");
    assert!(state.calls.lock().expect("calls").is_empty());

    let mut handle = prepared.submit().await.expect("submit");
    assert_eq!(handle.checkpoint().operation_id(), "resp-bg-1");
    assert_eq!(handle.checkpoint().status(), DeferredStatus::Queued);
    assert!(!handle.checkpoint().stream_created());
    assert_eq!(
        handle.poll().await.expect("poll"),
        DeferredStatus::InProgress
    );

    let checkpoint = {
        let mut generation = handle.resume().await.expect("initial stream");
        let created = generation.next().await.expect("created batch");
        assert!(created.events().is_empty());
        assert_eq!(created.checkpoint().sequence_number(), Some(1));
        let delta = generation.next().await.expect("delta batch");
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
    assert!(checkpoint.stream_created());

    let encoded = serde_json::to_vec(&checkpoint).expect("checkpoint JSON");
    let decoded = serde_json::from_slice(&encoded).expect("checkpoint decode");
    let mut restored = model
        .restore_deferred(decoded)
        .await
        .expect("restore without provider I/O");
    let mut resumed = restored.resume().await.expect("resume stream");
    let completed = resumed.next().await.expect("completed batch");
    assert!(matches!(
        completed.events().last(),
        Some(LanguageEvent::Finished { .. })
    ));
    assert_eq!(completed.checkpoint().sequence_number(), Some(3));
    assert_eq!(completed.checkpoint().status(), DeferredStatus::Completed);
    assert!(resumed.next().await.is_none());
    let final_checkpoint = resumed.finish_segment().expect("finish resumed segment");
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
        .prepare_deferred(request())
        .await
        .expect("prepare")
        .submit()
        .await
        .expect("submit first");
    assert_eq!(
        handle.cancel().await.expect("cancel first"),
        DeferredStatus::Cancelled
    );

    let calls = state.calls.lock().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].method, Method::POST);
    assert_eq!(calls[1].uri, "/v1/responses/resp-bg-1/cancel");
}
