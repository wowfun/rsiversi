#[path = "headless/child.rs"]
mod child;
use child::CommandOutput as _;
#[cfg(unix)]
use child::ObservedChild;

use axum::{
    Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use futures_util::StreamExt as _;
use rsi::{RunningRsi, StandardCodingTools, StandardComposition};
use rsi_agent_session_protocol::{
    AgentControlRecordBody, AgentMessageContent, AgentPresetId, MessageId, SessionFact,
    SessionFactBody, SessionId, TurnId, TurnOutcome, WorkspaceTrust,
};
use rsi_agent_store_protocol::SessionStore as _;
use rsi_agent_store_sqlite::SqliteStore;
use rsi_agent_turn_protocol::{MessageReceipt, ObservationCursor, SessionObservation};
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_credentials_local::SecretStore;
use rsi_credentials_protocol::{CredentialsError, Result as CredentialResult, SecretValue};
use rsi_host::HostPaths;
use rsi_sandbox::SandboxMode;
use rsi_session_protocol::{
    CreateSession, SessionHandle, SessionInput, SessionService as _, SubmitDirectImage,
    SubmitInput, TurnReceipt,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Notify;

const CHILD_PROVIDER_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug)]
struct EmptySecretStore;

impl SecretStore for EmptySecretStore {
    fn get(&self, _service: &str, _account: &str) -> CredentialResult<Option<SecretValue>> {
        Ok(None)
    }

    fn set(&self, _service: &str, _account: &str, _secret: &SecretValue) -> CredentialResult<()> {
        Err(CredentialsError::Store("read-only test store".into()))
    }

    fn unset(&self, _service: &str, _account: &str) -> CredentialResult<bool> {
        Err(CredentialsError::Store("read-only test store".into()))
    }
}

#[derive(Debug)]
struct Fixture {
    temporary: TempDir,
    paths: HostPaths,
    profile: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

fn fixture(endpoint: &str) -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("xdg-config/rsi");
    let state = temporary.path().join("xdg-state/rsi");
    let cache = temporary.path().join("xdg-cache/rsi");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "rsi.agent": {
                "default_model": {
                    "deployment": "fixture",
                    "model": "fixture-model"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let profile = config.join("host-profiles/test/host.profile.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    let application = config.join("application-profiles/test-headless/application.profile.toml");
    std::fs::create_dir_all(application.parent().unwrap()).unwrap();
    std::fs::write(
        &application,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"connection\"\nplugin = \"rsi.application.connection\"\nconfig = { host_profile = \"test\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.headless\"\n",
    )
    .unwrap();
    std::fs::write(
        &profile,
        format!(
            r#"format = 1

[[steps]]
kind = "plugin"
id = "fixture-provider"
plugin = "rsi.ai.provider.openai-compatible"

[steps.config]
deployment = "fixture"
endpoint = "{endpoint}"
path = "/v1/chat/completions"
allow_image_input = false
credential = {{ owner = "rsi.ai.provider.openai-compatible", slot = "default" }}

[steps.config.language_models.fixture-model]
context_window_tokens = 128000
default_output_reserve_tokens = 4096
max_output_reserve_tokens = 16384
"#
        ),
    )
    .unwrap();
    Fixture {
        paths: HostPaths::new(config, state, cache).unwrap(),
        profile,
        workspace,
        temporary,
    }
}

fn composition(paths: HostPaths) -> StandardComposition {
    StandardComposition::new(
        paths,
        BTreeMap::from([(
            "RSI_OPENAI_COMPATIBLE_API_KEY".into(),
            SecretValue::new("fixture-secret").unwrap(),
        )]),
        test_coding_tools(),
    )
    .with_credential_store(Arc::new(EmptySecretStore))
}

fn openai_composition(paths: HostPaths) -> StandardComposition {
    StandardComposition::new(
        paths,
        BTreeMap::from([(
            "OPENAI_API_KEY".into(),
            SecretValue::new("fixture-secret").unwrap(),
        )]),
        test_coding_tools(),
    )
    .with_credential_store(Arc::new(EmptySecretStore))
}

#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_wraps)] // Matches the non-Linux fixture seam where the standard coding generation is absent.
fn test_coding_tools() -> Option<StandardCodingTools> {
    Some(
        StandardCodingTools::new(
            std::fs::canonicalize("/bin/bash").unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap(),
            vec![("PATH".into(), "/usr/bin:/bin".into())],
        )
        .unwrap(),
    )
}

#[cfg(target_os = "linux")]
#[test]
fn standard_coding_tools_rejects_a_missing_bash_during_construction() {
    assert!(
        StandardCodingTools::new(
            std::path::PathBuf::from("/definitely/missing/rsi-bash"),
            std::env::current_exe().unwrap().canonicalize().unwrap(),
            Vec::new(),
        )
        .is_err()
    );
}

#[cfg(not(target_os = "linux"))]
fn test_coding_tools() -> Option<StandardCodingTools> {
    None
}

fn binary_command(binary: &str, fixture: &Fixture) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(binary);
    command
        .env("HOME", fixture.temporary.path())
        .env("XDG_CONFIG_HOME", fixture.paths.config().parent().unwrap())
        .env("XDG_STATE_HOME", fixture.paths.state().parent().unwrap())
        .env("XDG_CACHE_HOME", fixture.paths.cache().parent().unwrap())
        .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
    command
}

async fn chat() -> Response {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

async fn server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(chat)),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), task)
}

#[derive(Clone, Debug)]
struct ToolServerState {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

fn sse_response(events: impl IntoIterator<Item = serde_json::Value>) -> Response {
    let mut body = String::new();
    for event in events {
        writeln!(&mut body, "data: {event}\n").unwrap();
    }
    body.push_str("data: [DONE]\n\n");
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

fn tool_call_response(id: &str, name: &str, arguments: &serde_json::Value) -> Response {
    let arguments = serde_json::to_string(&arguments).unwrap();
    sse_response([
        serde_json::json!({
            "choices":[{
                "delta":{
                    "role":"assistant",
                    "tool_calls":[{
                        "index":0,
                        "id":id,
                        "type":"function",
                        "function":{"name":name,"arguments":arguments}
                    }]
                },
                "finish_reason":null
            }]
        }),
        serde_json::json!({
            "choices":[{"delta":{},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}
        }),
    ])
}

fn completed_chat_response(content: &str) -> Response {
    sse_response([
        serde_json::json!({
            "choices":[{
                "delta":{"role":"assistant","content":content},
                "finish_reason":null
            }]
        }),
        serde_json::json!({
            "choices":[{"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":1}
        }),
    ])
}

fn tool_message<'a>(request: &'a serde_json::Value, call_id: &str) -> &'a serde_json::Value {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
        .unwrap()
}

#[cfg(target_os = "linux")]
fn background_job_id(request: &serde_json::Value) -> &str {
    tool_message(request, "call-background-bash")["content"]
        .as_str()
        .unwrap()
        .strip_prefix("Started background Bash job ")
        .unwrap()
        .strip_suffix('.')
        .unwrap()
}

#[cfg(target_os = "linux")]
fn durable_tool_result<'a>(lines: &'a [serde_json::Value], call_id: &str) -> &'a serde_json::Value {
    lines
        .iter()
        .find(|line| {
            line["type"] == "fact"
                && line["fact"]["type"] == "tool_result"
                && line["fact"]["identity"]["call_id"] == call_id
        })
        .unwrap()
}

#[cfg(target_os = "linux")]
fn assert_real_coding_results(lines: &[serde_json::Value], job_id: &str) {
    let foreground = durable_tool_result(lines, "call-foreground-bash");
    assert_eq!(foreground["fact"]["result"]["is_error"], false);
    assert_eq!(foreground["fact"]["result"]["value"]["status"], "exited");
    assert_eq!(
        foreground["fact"]["result"]["value"]["stdout"]["text"],
        "foreground-complete"
    );
    let background = durable_tool_result(lines, "call-background-bash");
    assert_eq!(background["fact"]["result"]["value"]["job_id"], job_id);
    assert_eq!(background["fact"]["result"]["value"]["status"], "running");
    let listed = durable_tool_result(lines, "call-job-list");
    let listed_job = listed["fact"]["result"]["value"]["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == job_id)
        .unwrap();
    assert_eq!(listed_job["status"], "running");
    assert_eq!(listed_job["reported"], false);
    let read = durable_tool_result(lines, "call-job-output");
    assert_eq!(read["fact"]["result"]["value"]["id"], job_id);
    assert_eq!(read["fact"]["result"]["value"]["status"], "running");
    assert_eq!(read["fact"]["result"]["value"]["reported"], false);
    let killed = durable_tool_result(lines, "call-job-kill");
    assert_eq!(killed["fact"]["result"]["value"]["id"], job_id);
    assert_eq!(killed["fact"]["result"]["value"]["status"], "cancelled");
    assert_eq!(
        killed["fact"]["result"]["value"]["terminal"]["status"],
        "cancelled"
    );
    assert_eq!(killed["fact"]["result"]["value"]["reported"], true);
    let patched = durable_tool_result(lines, "call-apply-patch");
    assert_eq!(patched["fact"]["result"]["is_error"], false);
    assert_eq!(patched["fact"]["result"]["value"]["status"], "applied");
    let listed = durable_tool_result(lines, "call-directory-list");
    assert_eq!(listed["fact"]["result"]["is_error"], false);
    assert!(
        listed["fact"]["result"]["value"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "from-model.txt")
    );
    let read = durable_tool_result(lines, "call-file-read");
    assert_eq!(read["fact"]["result"]["is_error"], false);
    assert_eq!(
        read["fact"]["result"]["value"]["text"],
        "written through the complete tool loop\n"
    );
}

#[cfg(target_os = "linux")]
async fn complete_coding_tools_then_chat(
    State(state): State<ToolServerState>,
    body: String,
) -> Response {
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    state.requests.lock().unwrap().push(request.clone());
    match state.calls.fetch_add(1, Ordering::SeqCst) {
        0 => tool_call_response(
            "call-foreground-bash",
            "bash",
            &serde_json::json!({"command":"printf foreground-complete"}),
        ),
        1 => tool_call_response(
            "call-background-bash",
            "bash",
            &serde_json::json!({
                "command":"printf background-ready; while :; do sleep 60; done",
                "run_in_background":true
            }),
        ),
        2 => tool_call_response("call-job-list", "job_list", &serde_json::json!({})),
        3 => tool_call_response(
            "call-job-output",
            "job_output",
            &serde_json::json!({"job_id":background_job_id(&request)}),
        ),
        4 => tool_call_response(
            "call-job-kill",
            "job_kill",
            &serde_json::json!({"job_id":background_job_id(&request)}),
        ),
        5 => {
            let patch = concat!(
                "*** Begin Patch\n",
                "*** Add File: from-model.txt\n",
                "+written through the complete tool loop\n",
                "*** End Patch\n"
            );
            tool_call_response(
                "call-apply-patch",
                "apply_patch",
                &serde_json::json!({"patch":patch}),
            )
        }
        6 => tool_call_response(
            "call-directory-list",
            "directory_list",
            &serde_json::json!({}),
        ),
        7 => tool_call_response(
            "call-file-read",
            "file_read",
            &serde_json::json!({"path":"from-model.txt"}),
        ),
        _ => completed_chat_response("all coding tools completed"),
    }
}

#[cfg(target_os = "linux")]
async fn background_then_chat(State(state): State<ToolServerState>, body: String) -> Response {
    state
        .requests
        .lock()
        .unwrap()
        .push(serde_json::from_str(&body).unwrap());
    if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        let arguments = serde_json::to_string(&serde_json::json!({
            "command":"printf background-complete",
            "run_in_background":true
        }))
        .unwrap();
        return sse_response([
            serde_json::json!({
                "choices":[{
                    "delta":{
                        "role":"assistant",
                        "tool_calls":[{
                            "index":0,
                            "id":"call-background-bash",
                            "type":"function",
                            "function":{"name":"bash","arguments":arguments}
                        }]
                    },
                    "finish_reason":null
                }]
            }),
            serde_json::json!({
                "choices":[{"delta":{},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":10,"completion_tokens":5}
            }),
        ]);
    }
    sse_response([
        serde_json::json!({
            "choices":[{
                "delta":{"role":"assistant","content":"finished without collecting it"},
                "finish_reason":null
            }]
        }),
        serde_json::json!({
            "choices":[{"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":4}
        }),
    ])
}

#[cfg(target_os = "linux")]
async fn rejected_patch_then_chat(State(state): State<ToolServerState>, body: String) -> Response {
    state
        .requests
        .lock()
        .unwrap()
        .push(serde_json::from_str(&body).unwrap());
    if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: missing.txt\n",
            "@@\n",
            "-old\n",
            "+new\n",
            "*** End Patch\n"
        );
        let arguments = serde_json::to_string(&serde_json::json!({"patch":patch})).unwrap();
        return sse_response([
            serde_json::json!({
                "choices":[{
                    "delta":{
                        "role":"assistant",
                        "tool_calls":[{
                            "index":0,
                            "id":"call-rejected-patch",
                            "type":"function",
                            "function":{"name":"apply_patch","arguments":arguments}
                        }]
                    },
                    "finish_reason":null
                }]
            }),
            serde_json::json!({
                "choices":[{"delta":{},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":10,"completion_tokens":5}
            }),
        ]);
    }
    sse_response([
        serde_json::json!({
            "choices":[{
                "delta":{"role":"assistant","content":"handled rejection"},
                "finish_reason":null
            }]
        }),
        serde_json::json!({
            "choices":[{"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":2}
        }),
    ])
}

#[cfg(target_os = "linux")]
async fn tool_server() -> (
    String,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = ToolServerState {
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/v1/chat/completions",
                    post(complete_coding_tools_then_chat),
                )
                .with_state(state),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), calls, requests, task)
}

#[cfg(target_os = "linux")]
async fn background_server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let state = ToolServerState {
        calls: Arc::clone(&calls),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(background_then_chat))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), calls, task)
}

#[cfg(target_os = "linux")]
async fn rejected_patch_server() -> (
    String,
    Arc<Mutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = ToolServerState {
        calls: Arc::new(AtomicUsize::new(0)),
        requests: Arc::clone(&requests),
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(rejected_patch_then_chat))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), requests, task)
}

async fn image() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"data":[{"b64_json":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="}]}"#,
        ))
        .unwrap()
}

async fn image_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/images/generations", post(image)),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), task)
}

#[derive(Clone, Debug)]
struct CrashServerState {
    calls: Arc<AtomicUsize>,
    first_request_started: Arc<Notify>,
}

async fn crash_then_chat(State(state): State<CrashServerState>) -> Response {
    if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        state.first_request_started.notify_one();
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
    chat().await
}

async fn crash_server() -> (
    String,
    Arc<Notify>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_request_started = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let state = CrashServerState {
        calls: Arc::clone(&calls),
        first_request_started: Arc::clone(&first_request_started),
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(crash_then_chat))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    (
        format!("http://{address}"),
        first_request_started,
        calls,
        task,
    )
}

async fn failed_chat() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"error":{"message":"fixture failure","type":"server_error","code":"server_error"}}"#,
        ))
        .unwrap()
}

async fn failed_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(failed_chat)),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), task)
}

async fn observe_after_acceptance(
    handle: &Arc<dyn SessionHandle>,
    receipt: &TurnReceipt,
) -> (Vec<rsi_agent_turn_protocol::ObservedFact>, TurnOutcome, u64) {
    observe_turn_after(handle, &receipt.turn_id, receipt.accepted_seq).await
}

async fn claim_message(handle: &Arc<dyn SessionHandle>, receipt: &MessageReceipt) -> (TurnId, u64) {
    let mut observation = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: receipt.observed_fact_seq,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let SessionObservation::Control { record, .. } =
                observation.next().await.unwrap().unwrap()
                && let AgentControlRecordBody::MessageClaimed {
                    message_id,
                    turn_id,
                    entered_fact_seq,
                    ..
                } = record.body()
                && message_id == &receipt.message_id
            {
                return (turn_id.clone(), *entered_fact_seq);
            }
        }
    })
    .await
    .expect("message reached its durable claim boundary")
}

async fn observe_turn_after(
    handle: &Arc<dyn SessionHandle>,
    turn_id: &TurnId,
    after_fact_seq: u64,
) -> (Vec<rsi_agent_turn_protocol::ObservedFact>, TurnOutcome, u64) {
    let mut observation = handle
        .observe(ObservationCursor {
            control_seq: 0,
            fact_seq: after_fact_seq,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut facts = Vec::new();
        loop {
            let update = observation.next().await.unwrap().unwrap();
            if let SessionObservation::Fact {
                fact,
                durable_fact_seq,
            } = update
            {
                assert!(
                    fact.seq() > after_fact_seq,
                    "subscription must begin strictly after durable acceptance"
                );
                let terminal = match fact.body() {
                    SessionFactBody::TurnTerminal {
                        turn_id: observed,
                        outcome,
                    } if observed == turn_id => Some(outcome.clone()),
                    _ => None,
                };
                facts.push(fact);
                if let Some(outcome) = terminal {
                    return (facts, outcome, durable_fact_seq);
                }
            }
        }
    })
    .await
    .expect("turn reached a durable terminal Fact")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_profile_runs_fresh_and_resume_through_durable_plugins() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let application = running.session_service().unwrap();
    let first_handle = application
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("fixture-created").unwrap(),
            agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    let first = first_handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-first").unwrap(),
            content: vec![SessionInput::Text {
                text: "/status".into(),
            }],
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let (first_turn_id, first_entered_fact_seq) = claim_message(&first_handle, &first).await;
    let (first_facts, first_outcome, first_durable_seq) =
        observe_turn_after(&first_handle, &first_turn_id, first_entered_fact_seq).await;
    assert_eq!(first_outcome, TurnOutcome::Completed);
    assert!(first_durable_seq >= first_facts.last().unwrap().seq());
    assert!(first_facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ModelEvent {
            event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                delta: rsi_ai_protocol::ContentDelta::Text(text),
                ..
            },
            ..
        } if text == "hello"
    )));
    let first_history = first_handle.history_before(None, 64).await.unwrap();
    assert!(first_history.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageEntered { content, .. }
            if content == &[AgentMessageContent::Text { text: "/status".into() }]
    )));

    let second_handle = application.attach(&first.session_id).await.unwrap();
    let second = second_handle
        .submit(SubmitInput {
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            message_id: MessageId::new("message-second").unwrap(),
            content: vec![SessionInput::Text {
                text: "again".into(),
            }],
            model: Some(ModelRef::new("fixture", "fixture-model").unwrap()),
            sandbox: Some(SandboxMode::ReadOnly),
        })
        .await
        .unwrap();
    let (second_turn_id, second_entered_fact_seq) = claim_message(&second_handle, &second).await;
    let (second_facts, second_outcome, _) =
        observe_turn_after(&second_handle, &second_turn_id, second_entered_fact_seq).await;
    assert_eq!(second_outcome, TurnOutcome::Completed);
    assert!(second_facts.first().unwrap().seq() > first_durable_seq);
    assert!(matches!(
        second_handle.history_before(None, 64).await.unwrap().facts.iter().find(|fact| matches!(fact.body(), SessionFactBody::MessageTurnAccepted { turn_id, .. } if turn_id == &second_turn_id)).unwrap().body(),
        SessionFactBody::MessageTurnAccepted {
            model: Some(model),
            sandbox: SandboxMode::ReadOnly,
            ..
        } if model.deployment() == "fixture" && model.model() == "fixture-model"
    ));

    assert!(running.shutdown().await.is_clean());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_standard_writer_fails_before_session_recovery() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let first = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let second = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile).await;
    assert!(second.is_err());
    assert!(first.shutdown().await.is_clean());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_rejects_a_different_canonical_workspace() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let other = fixture.temporary.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let binary = env!("CARGO_BIN_EXE_rsi");
    let first = binary_command(binary, &fixture)
        .args([
            "--profile",
            "test-headless",
            "first",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--session-id",
            "session-workspace-authority",
        ])
        .observed_output()
        .await
        .unwrap();
    assert!(first.status.success());
    let resumed = binary_command(binary, &fixture)
        .args([
            "--profile",
            "test-headless",
            "second",
            "--resume",
            "session-workspace-authority",
            "--cwd",
            other.to_str().unwrap(),
        ])
        .observed_output()
        .await;
    let resumed = resumed.unwrap();
    assert_eq!(resumed.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&resumed.stderr).contains("does not match the durable Session")
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_separates_jsonl_and_model_text_from_status_feedback() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let binary = env!("CARGO_BIN_EXE_rsi");
    let first = binary_command(binary, &fixture)
        .args([
            "--profile",
            "test-headless",
            "/status",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--session-id",
            "session-binary",
            "--output",
            "jsonl",
        ])
        .observed_output()
        .await
        .unwrap();
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stderr.is_empty());
    let stdout = String::from_utf8(first.stdout).unwrap();
    assert!(!stdout.contains("bubblewrap"));
    let lines = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.first().unwrap()["type"], "message");
    assert_eq!(lines.first().unwrap()["version"], 5);
    assert_eq!(lines.first().unwrap()["session_id"], "session-binary");
    assert_eq!(lines.get(1).unwrap()["type"], "turn");
    assert_eq!(lines.last().unwrap()["type"], "outcome");
    assert!(lines.iter().any(|line| {
        line["type"] == "fact"
            && line["durable_seq"].as_u64().unwrap() >= line["fact"]["seq"].as_u64().unwrap()
    }));
    let session_id = lines.first().unwrap()["session_id"].as_str().unwrap();

    let second = binary_command(binary, &fixture)
        .args([
            "--profile",
            "test-headless",
            "again",
            "--resume",
            session_id,
            "--cwd",
            fixture.workspace.to_str().unwrap(),
        ])
        .observed_output()
        .await
        .unwrap();
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(second.stdout, b"hello\n");
    let status = String::from_utf8(second.stderr).unwrap();
    assert!(status.contains("accepted:") && status.contains("outcome: Completed"));

    let mut third = binary_command(binary, &fixture)
        .args([
            "--profile",
            "test-headless",
            "--stdin",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    third
        .stdin
        .take()
        .unwrap()
        .write_all(b"/from-stdin")
        .await
        .unwrap();
    let third = third.wait_with_output().await.unwrap();
    assert!(third.status.success());
    assert_eq!(third.stdout, b"hello\n");
    assert!(
        String::from_utf8(third.stderr)
            .unwrap()
            .contains("outcome: Completed")
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_treats_help_after_separator_as_the_literal_task() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--",
            "--help",
        ])
        .observed_output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "hello\n");
    server.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn built_binary_patch_helper_requires_the_sole_marker_and_uses_one_line_protocol() {
    let binary = env!("CARGO_BIN_EXE_rsi");
    let workspace = tempfile::tempdir().unwrap();
    let patch = concat!(
        "{\"version\":2,\"evidence_bytes\":32768}\n",
        "*** Begin Patch\n",
        "*** Add File: direct.txt\n",
        "+direct helper\n",
        "*** End Patch\n"
    );
    let mut child = tokio::process::Command::new(binary)
        .arg("--rsi-run-as-apply-patch")
        .current_dir(workspace.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(patch.as_bytes())
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(output.stdout.ends_with(b"\n"));
    assert!(!output.stdout[..output.stdout.len() - 1].contains(&b'\n'));
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout[..output.stdout.len() - 1]).unwrap();
    assert_eq!(response["status"], "applied");
    assert!(
        response["evidence"]["diffs"][0]["unified_diff"]
            .as_str()
            .unwrap()
            .contains("+direct helper")
    );
    assert_eq!(
        std::fs::read(workspace.path().join("direct.txt")).unwrap(),
        b"direct helper\n"
    );

    let rejected = tokio::process::Command::new(binary)
        .arg("--rsi-run-as-apply-patch")
        .current_dir(workspace.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut rejected = rejected;
    rejected
        .stdin
        .take()
        .unwrap()
        .write_all(b"not a patch")
        .await
        .unwrap();
    let rejected = rejected.wait_with_output().await.unwrap();
    assert!(rejected.status.success());
    assert!(rejected.stderr.is_empty());
    assert!(rejected.stdout.ends_with(b"\n"));
    assert!(!rejected.stdout[..rejected.stdout.len() - 1].contains(&b'\n'));
    let response: serde_json::Value =
        serde_json::from_slice(&rejected.stdout[..rejected.stdout.len() - 1]).unwrap();
    assert_eq!(response["status"], "rejected");

    let extra = tokio::process::Command::new(binary)
        .args(["--rsi-run-as-apply-patch", "extra"])
        .current_dir(workspace.path())
        .observed_output()
        .await
        .unwrap();
    assert_eq!(extra.status.code(), Some(2));
    assert!(extra.stdout.is_empty());
    assert!(String::from_utf8_lossy(&extra.stderr).contains("unknown"));
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_runs_the_complete_real_coding_tool_flow() {
    let (endpoint, calls, requests, server) = tool_server().await;
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "exercise all coding tools",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--sandbox",
            "workspace-write",
            "--output",
            "jsonl",
        ])
        .observed_output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 9);
    assert_eq!(
        std::fs::read(fixture.workspace.join("from-model.txt")).unwrap(),
        b"written through the complete tool loop\n"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    let mut tool_names = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    assert_eq!(
        tool_names,
        [
            "apply_patch",
            "ask_user",
            "bash",
            "directory_list",
            "file_read",
            "followup_task",
            "interrupt_agent",
            "job_kill",
            "job_list",
            "job_output",
            "list_agents",
            "output_read",
            "report_goal",
            "send_message",
            "spawn_agent",
            "wait_agent",
        ]
    );
    assert_eq!(
        tool_message(&requests[1], "call-foreground-bash")["content"],
        "foreground-complete\n[status: exited; exit code: 0; signal: none]"
    );
    let job_id = background_job_id(&requests[2]).to_owned();
    assert!(
        tool_message(&requests[3], "call-job-list")["content"]
            .as_str()
            .unwrap()
            .contains(&job_id)
    );
    assert!(tool_message(&requests[4], "call-job-output")["content"].is_string());
    assert!(tool_message(&requests[5], "call-job-kill")["content"].is_string());
    assert!(
        tool_message(&requests[6], "call-apply-patch")["content"]
            .as_str()
            .unwrap()
            .contains("applied")
    );
    assert!(
        tool_message(&requests[7], "call-directory-list")["content"]
            .as_str()
            .unwrap()
            .contains("from-model.txt")
    );
    assert!(
        tool_message(&requests[8], "call-file-read")["content"]
            .as_str()
            .unwrap()
            .contains("written through the complete tool loop")
    );
    drop(requests);

    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_real_coding_results(&lines, &job_id);
    assert_eq!(lines.last().unwrap()["outcome"]["status"], "completed");
    server.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_blocks_success_when_background_work_was_not_collected() {
    let (endpoint, calls, server) = background_server().await;
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "start work but forget to collect it",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--sandbox",
            "workspace-write",
            "--output",
            "jsonl",
        ])
        .observed_output()
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("jobs.unreported_background_work"));
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let outcome = lines.last().unwrap();
    assert_eq!(outcome["type"], "outcome");
    assert_eq!(outcome["outcome"]["status"], "failed");
    assert_eq!(
        outcome["outcome"]["code"],
        "jobs.unreported_background_work"
    );
    assert!(lines.iter().any(|line| {
        line["type"] == "fact"
            && line["fact"]["type"] == "tool_result"
            && line["fact"]["result"]["value"]["status"] == "running"
    }));
    server.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_patch_evidence_is_complete_in_the_next_model_request() {
    let (endpoint, requests, server) = rejected_patch_server().await;
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "attempt a patch and handle rejection",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--sandbox",
            "workspace-write",
            "--output",
            "jsonl",
        ])
        .observed_output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.workspace.join("missing.txt").exists());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let tool_message = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let evidence: serde_json::Value =
        serde_json::from_str(tool_message["content"].as_str().unwrap()).unwrap();
    assert_eq!(evidence["status"], "rejected");
    assert_eq!(evidence["failure"]["operation"], 0);
    assert_eq!(evidence["failure"]["code"], "not_found");
    assert_eq!(evidence["failure"]["path"], "missing.txt");
    assert!(evidence["effects"].as_array().unwrap().is_empty());
    drop(requests);

    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let result = lines
        .iter()
        .find(|line| line["type"] == "fact" && line["fact"]["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["fact"]["result"]["is_error"], true);
    let mut stored = result["fact"]["result"]["value"].clone();
    let presentation = stored.as_object_mut().unwrap().remove("evidence").unwrap();
    assert_eq!(
        presentation,
        serde_json::json!({"version":1,"omitted":false,"diffs":[]})
    );
    assert_eq!(
        stored, evidence,
        "the model receives the complete ledger without presentation data"
    );
    assert_eq!(lines.last().unwrap()["outcome"]["status"], "completed");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_uses_fixed_failure_exit_classes() {
    let binary = env!("CARGO_BIN_EXE_rsi");
    let usage = tokio::process::Command::new(binary)
        .args(["--profile", "headless", "task", "--stdin"])
        .observed_output()
        .await
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    assert!(usage.stdout.is_empty());
    assert!(String::from_utf8_lossy(&usage.stderr).contains("exactly one"));

    let missing_route = fixture("http://127.0.0.1:9");
    std::fs::write(&missing_route.profile, "format = 1\n").unwrap();
    let boot = tokio::process::Command::new(binary)
        .args([
            "--profile",
            "test-headless",
            "task",
            "--cwd",
            missing_route.workspace.to_str().unwrap(),
        ])
        .env("HOME", missing_route.temporary.path())
        .env(
            "XDG_CONFIG_HOME",
            missing_route.paths.config().parent().unwrap(),
        )
        .env(
            "XDG_STATE_HOME",
            missing_route.paths.state().parent().unwrap(),
        )
        .env(
            "XDG_CACHE_HOME",
            missing_route.paths.cache().parent().unwrap(),
        )
        .observed_output()
        .await
        .unwrap();
    assert_eq!(boot.status.code(), Some(1));
    assert!(boot.stdout.is_empty());
    assert!(String::from_utf8_lossy(&boot.stderr).contains("not registered"));

    let (endpoint, server) = failed_server().await;
    let fixture = fixture(&endpoint);
    let failed = tokio::process::Command::new(binary)
        .args([
            "--profile",
            "test-headless",
            "fail",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .env("HOME", fixture.temporary.path())
        .env("XDG_CONFIG_HOME", fixture.paths.config().parent().unwrap())
        .env("XDG_STATE_HOME", fixture.paths.state().parent().unwrap())
        .env("XDG_CACHE_HOME", fixture.paths.cache().parent().unwrap())
        .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret")
        .observed_output()
        .await
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("provider.server"));
    let lines = String::from_utf8(failed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.last().unwrap()["type"], "outcome");
    assert_eq!(lines.last().unwrap()["outcome"]["status"], "failed");
    server.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_sigint_cancels_flushes_and_exits_130() {
    let (endpoint, first_request_started, _calls, server) = crash_server().await;
    let fixture = fixture(&endpoint);
    let mut child = ObservedChild::spawn(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rsi"))
            .args([
                "--profile",
                "test-headless",
                "wait",
                "--cwd",
                fixture.workspace.to_str().unwrap(),
                "--output",
                "jsonl",
            ])
            .env("HOME", fixture.temporary.path())
            .env("XDG_CONFIG_HOME", fixture.paths.config().parent().unwrap())
            .env("XDG_STATE_HOME", fixture.paths.state().parent().unwrap())
            .env("XDG_CACHE_HOME", fixture.paths.cache().parent().unwrap())
            .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret"),
    )
    .unwrap();
    child
        .wait_provider(
            first_request_started.notified(),
            CHILD_PROVIDER_START_TIMEOUT,
        )
        .await
        .unwrap();
    let process_id = child.id().to_string();
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-INT", &process_id])
            .status()
            .await
            .unwrap()
            .success()
    );
    child.signal_sent("SIGINT");
    let output = child.wait(std::time::Duration::from_secs(5)).await.unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(output.stderr.is_empty());
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.first().unwrap()["type"], "message");
    assert_eq!(lines.get(1).unwrap()["type"], "turn");
    assert!(
        lines
            .iter()
            .any(|line| { line["type"] == "fact" && line["fact"]["type"] == "cancel_requested" })
    );
    assert_eq!(lines.last().unwrap()["type"], "outcome", "{lines:#?}");
    assert_eq!(lines.last().unwrap()["outcome"]["status"], "cancelled");
    server.abort();
}

fn assert_sigkill_recovery_order(facts: &[SessionFact]) {
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::MessageTurnAccepted { .. }))
            .count(),
        2
    );
    let started = facts
        .iter()
        .position(|fact| matches!(fact.body(), SessionFactBody::ModelStarted { .. }))
        .unwrap();
    let interrupted = facts
        .iter()
        .position(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::TurnTerminal {
                    outcome: TurnOutcome::Interrupted { .. },
                    ..
                }
            )
        })
        .unwrap();
    let completed = facts
        .iter()
        .rposition(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::TurnTerminal {
                    outcome: TurnOutcome::Completed,
                    ..
                }
            )
        })
        .unwrap();
    assert!(started < interrupted && interrupted < completed);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_recovers_a_real_sqlite_prefix_after_sigkill() {
    let (endpoint, first_request_started, calls, server) = crash_server().await;
    let fixture = fixture(&endpoint);
    let session_id = "session-sigkill-recovery";
    let mut child =
        ObservedChild::spawn(binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture).args([
            "--profile",
            "test-headless",
            "first",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--session-id",
            session_id,
            "--output",
            "jsonl",
        ]))
        .unwrap();
    child
        .wait_provider(
            first_request_started.notified(),
            CHILD_PROVIDER_START_TIMEOUT,
        )
        .await
        .unwrap();
    let process_id = child.id().to_string();
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-KILL", &process_id])
            .status()
            .await
            .unwrap()
            .success()
    );
    child.signal_sent("SIGKILL");
    let killed = child.wait(std::time::Duration::from_secs(5)).await.unwrap();
    assert!(!killed.status.success());

    let resumed = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "second",
            "--resume",
            session_id,
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .observed_output()
        .await
        .unwrap();
    assert!(
        resumed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let store = SqliteStore::open(fixture.paths.state().join("agent")).unwrap();
    let session_id = SessionId::new(session_id).unwrap();
    let facts = store.read_facts(&session_id, 0, 256).await.unwrap();
    assert!(facts.caught_up());
    assert_sigkill_recovery_order(&facts.facts);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_image_turn_renders_only_the_durable_media_reference() {
    let (endpoint, server) = image_server().await;
    let fixture = fixture(&endpoint);
    std::fs::write(
        &fixture.profile,
        format!(
            r#"format = 1

[[steps]]
kind = "plugin"
id = "fixture-provider"
plugin = "rsi.ai.provider.openai"

[steps.config]
deployment = "fixture"
endpoint = "{endpoint}"
language = false
image = true
language_models = {{}}
credential = {{ owner = "rsi.ai.provider.openai", slot = "default" }}

"#
        ),
    )
    .unwrap();
    let running = RunningRsi::boot(openai_composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let application = running.session_service().unwrap();
    let handle = application
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("fixture-created").unwrap(),
            agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    let receipt = handle
        .generate_image(SubmitDirectImage {
            turn_id: rsi_agent_session_protocol::TurnId::new("turn-direct-image").unwrap(),
            model: ModelRef::new("fixture", "gpt-image-1").unwrap(),
            request: ImageRequest::new("one pixel", 1).unwrap(),
        })
        .await
        .unwrap();
    let (facts, outcome, _) = observe_after_acceptance(&handle, &receipt).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let image_fact = facts
        .iter()
        .find(|fact| matches!(fact.body(), SessionFactBody::ImageOutput { .. }))
        .unwrap();
    let encoded = serde_json::to_string(image_fact.as_ref()).unwrap();
    assert!(!encoded.contains("b64_json"));
    assert!(!encoded.contains("iVBOR"));
    assert!(running.shutdown().await.is_clean());
    server.abort();
}

#[test]
fn fixture_paths_remain_absolute() {
    let fixture = fixture("http://127.0.0.1:9");
    assert!(fixture.paths.config().is_absolute());
}

async fn question_then_chat(
    State(state): State<ToolServerState>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    state.requests.lock().unwrap().push(request);
    if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        tool_call_response(
            "ask-choice",
            "ask_user",
            &serde_json::json!({"questions":[{"id":"name","prompt":"Which name?","options":["one","two"]}]}),
        )
    } else {
        completed_chat_response("answer received")
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One real Host lifecycle per adapter validates the same question protocol.
async fn real_question_tool_and_inspection_have_local_and_uds_parity() {
    use rsi_api_http::LocalHttpService;
    use rsi_api_protocol::HostEpoch;
    use rsi_api_uds_client::{UdsClient, UdsClientConfig};
    use rsi_service_host::{
        HostOwnerLease, LocalApiServer, ServiceHostPaths, local_compatibility_key,
    };
    use rsi_session_api::SessionClient;
    use tokio_util::sync::CancellationToken;
    for remote in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = ToolServerState {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: requests.clone(),
        };
        let http = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(question_then_chat))
                    .with_state(state),
            )
            .await
            .unwrap();
        });
        let fixture = fixture(&endpoint);
        // macOS TMPDIR can exceed sockaddr_un even for a short fixture name.
        // Keep the socket in an isolated short /tmp directory on both Unix hosts.
        let socket_root = tempfile::Builder::new()
            .prefix("rsi-q-")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = ServiceHostPaths::from_host_paths_with_runtime(
            &fixture.paths,
            Some(socket_root.path()),
        )
        .unwrap();
        let epoch = HostEpoch::generate().unwrap();
        let owner = Arc::new(HostOwnerLease::try_acquire(paths.clone()).unwrap());
        let composition = composition(fixture.paths.clone())
            .with_service_owner(owner.clone(), epoch)
            .unwrap();
        let running = RunningRsi::boot(composition, &fixture.profile)
            .await
            .unwrap();
        let local = running.session_service().unwrap();
        let description = running.connection_description().unwrap();
        let compatibility = local_compatibility_key(&"a".repeat(64)).unwrap();
        let config = UdsClientConfig {
            socket: paths.socket().to_path_buf(),
            endpoint_id: description.endpoint_id.clone(),
            host_epoch: description.host_epoch.clone(),
            compatibility: compatibility.clone(),
        };
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        let service = LocalHttpService::new(
            execution.clone(),
            running.api_dispatch().unwrap(),
            description.as_ref(),
            compatibility,
        )
        .unwrap();
        let transport = LocalApiServer::bind(owner, service).unwrap();
        let stop = CancellationToken::new();
        let server = tokio::spawn(transport.serve(stop.clone()));
        let client = Arc::new(
            UdsClient::connect(execution.clone(), config.clone())
                .await
                .unwrap(),
        );
        let application: Arc<dyn rsi_session_protocol::SessionService> = if remote {
            Arc::new(SessionClient::new(client.clone()).unwrap())
        } else {
            local.clone()
        };
        let handle = application
            .create(CreateSession {
                workspace_id: running
                    .workspace_registry()
                    .unwrap()
                    .get_or_create(&fixture.workspace)
                    .await
                    .unwrap()
                    .id,
                session_id: SessionId::new("question-session").unwrap(),
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            })
            .await
            .unwrap();
        let receipt = handle
            .submit(SubmitInput {
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                message_id: MessageId::new("question-input").unwrap(),
                content: vec![SessionInput::Text {
                    text: "ask a question".into(),
                }],
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let (turn, entered) = claim_message(&handle, &receipt).await;
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let pending = handle.pending_questions().await.unwrap();
                if let Some(request) = pending.into_iter().next() {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let snapshot = handle.inspect().await.unwrap();
        assert_eq!(snapshot.active_turn_id.as_ref(), Some(&turn));
        assert_eq!(
            snapshot.activation_phase,
            Some(rsi_agent_store_protocol::StoreActivationPhase::Parked)
        );
        assert_eq!(
            snapshot.tree.session.session_id,
            *handle.header().await.unwrap().session_id()
        );
        let reconnected = Arc::new(UdsClient::connect(execution, config).await.unwrap());
        let reconnect = SessionClient::new(reconnected.clone()).unwrap();
        let attached = reconnect
            .attach(handle.header().await.unwrap().session_id())
            .await
            .unwrap();
        assert_eq!(
            attached.pending_questions().await.unwrap().as_slice(),
            std::slice::from_ref(&request)
        );
        let answer = rsi_user_questions_protocol::QuestionAnswer {
            answers: vec!["my free answer".into()],
        };
        assert!(
            attached
                .answer_question(&request.id, answer.clone())
                .await
                .unwrap()
        );
        assert!(handle.answer_question(&request.id, answer).await.unwrap());
        assert!(
            handle
                .answer_question(
                    &request.id,
                    rsi_user_questions_protocol::QuestionAnswer {
                        answers: vec!["conflict".into()]
                    }
                )
                .await
                .is_err()
        );
        let (facts, outcome, _) = observe_turn_after(&handle, &turn, entered).await;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert!(facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::ToolResult { result, .. } if result.value["answers"][0] == "my free answer")));
        assert!(
            tool_message(&requests.lock().unwrap()[1], "ask-choice")["content"]
                .as_str()
                .unwrap()
                .contains("my free answer")
        );
        assert!(handle.pending_questions().await.unwrap().is_empty());
        reconnected.close().await;
        client.close().await;
        stop.cancel();
        server.await.unwrap().unwrap();
        assert!(running.shutdown().await.is_clean());
        http.abort();
    }
}

#[cfg(target_os = "linux")]
async fn full_output_then_chat(
    State(state): State<ToolServerState>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let call = state.calls.fetch_add(1, Ordering::SeqCst);
    state.requests.lock().unwrap().push(request.clone());
    match call {
        0 => tool_call_response(
            "large-bash",
            "bash",
            &serde_json::json!({"command":"printf 'prefix 中😀'; head -c 100000 /dev/zero | tr '\\0' x; printf suffix; exit 7"}),
        ),
        1 | 2 => {
            let feedback = tool_message(&request, "large-bash")["content"]
                .as_str()
                .unwrap();
            assert!(
                feedback.contains("[stdout truncated; showing retained tail]"),
                "feedback: {feedback}"
            );
            assert!(feedback.contains("exit code: 7"));
            assert!(!feedback.contains("prefix 中😀"));
            let id = feedback
                .split("[full stdout: ")
                .nth(1)
                .unwrap()
                .split(']')
                .next()
                .unwrap();
            let (name, offset, limit) = if call == 1 {
                ("read-prefix", 0, 12)
            } else {
                ("read-next", 12, 8)
            };
            tool_call_response(
                name,
                "output_read",
                &serde_json::json!({"id":id,"offset":offset,"limit":limit}),
            )
        }
        3 => {
            let prefix = tool_message(&request, "read-prefix")["content"]
                .as_str()
                .unwrap()
                .to_owned();
            let next = tool_message(&request, "read-next")["content"]
                .as_str()
                .unwrap()
                .to_owned();
            let raw = |text: &str| {
                hex::decode(
                    text.split("[raw bytes hex: ")
                        .nth(1)
                        .expect("split UTF-8 page lost its raw bytes")
                        .split(']')
                        .next()
                        .unwrap(),
                )
                .unwrap()
            };
            let mut bytes = raw(&prefix);
            bytes.extend(raw(&next));
            assert!(String::from_utf8(bytes).unwrap().starts_with("prefix 中😀"));
            assert!(
                tool_message(&request, "read-prefix")["content"]
                    .as_str()
                    .unwrap()
                    .contains("prefix 中")
            );
            assert!(
                tool_message(&request, "read-next")["content"]
                    .as_str()
                    .unwrap()
                    .contains("bytes: 12..20 of 100020")
            );
            completed_chat_response("full output read")
        }
        _ => panic!("unexpected model request"),
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_reads_full_command_output_across_raw_utf8_page_boundaries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let state = ToolServerState {
        calls: Arc::new(AtomicUsize::new(0)),
        requests: Arc::default(),
    };
    let http = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(full_output_then_chat))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "read complete output",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .observed_output_with_timeout(std::time::Duration::from_secs(20))
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let first = durable_tool_result(&lines, "read-prefix");
    let second = durable_tool_result(&lines, "read-next");
    assert_eq!(first["fact"]["result"]["value"]["next_offset"], 12);
    assert_eq!(second["fact"]["result"]["value"]["offset"], 12);
    assert_eq!(second["fact"]["result"]["value"]["next_offset"], 20);
    let bash = durable_tool_result(&lines, "large-bash");
    assert_eq!(bash["fact"]["result"]["value"]["exit_code"], 7);
    assert_eq!(bash["fact"]["result"]["is_error"], false);
    http.abort();
}

#[cfg(target_os = "linux")]
async fn child_question_chat(
    State(state): State<ToolServerState>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    state.requests.lock().unwrap().push(request.clone());
    let messages = request["messages"].as_array().unwrap();
    let has_tool = |id: &str| {
        messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == id)
    };
    let child = messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"]
                .to_string()
                .contains("child-only-question")
    });
    if child {
        if has_tool("child-question") {
            completed_chat_response("child returned question to parent")
        } else {
            tool_call_response(
                "child-question",
                "ask_user",
                &serde_json::json!({"questions":[{"id":"child","prompt":"Forbidden child prompt"}]}),
            )
        }
    } else if !has_tool("spawn-question-child") {
        tool_call_response(
            "spawn-question-child",
            "spawn_agent",
            &serde_json::json!({"task_name":"question-child","message":"child-only-question","fork_turns":"none"}),
        )
    } else if !has_tool("wait-question-child") {
        tool_call_response(
            "wait-question-child",
            "wait_agent",
            &serde_json::json!({"timeout_ms":5000}),
        )
    } else {
        completed_chat_response("root finished")
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn child_question_is_a_model_visible_error_without_a_human_waiter() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = ToolServerState {
        calls: Arc::new(AtomicUsize::new(0)),
        requests: requests.clone(),
    };
    let http = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(child_question_chat))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "--profile",
            "test-headless",
            "spawn a worker and wait",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .observed_output_with_timeout(std::time::Duration::from_secs(20))
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = requests.lock().unwrap();
    let result = requests
        .iter()
        .flat_map(|request| request["messages"].as_array().unwrap())
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == "child-question")
        .expect("child must receive a question Tool result");
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("Only the root Agent")
    );
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!lines.iter().any(|v| {
        v["type"] == "interactions"
            && v["data"]["questions"]
                .as_array()
                .is_some_and(|questions| !questions.is_empty())
    }));
    http.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_sigint_exits_with_an_undrained_stdout_pipe() {
    verify_undrained_stdout(None).await;
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_sigint_after_model_completion_still_stops_blocked_output() {
    verify_undrained_stdout(Some(0)).await;
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_sigint_after_model_completion_with_a_full_renderer_queue() {
    verify_undrained_stdout(Some(64)).await;
}
#[cfg(unix)]
async fn verify_undrained_stdout(deltas: Option<usize>) {
    use std::time::Duration;
    use tokio::io::AsyncReadExt as _;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let sent = Arc::new(Notify::new());
    let notify = sent.clone();
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/v1/chat/completions", post(move || {
            let notify = notify.clone();
            async move {
                let chunk = format!("data: {}\n\n", serde_json::json!({"choices":[{"delta":{"role":"assistant","content":"x".repeat(200_000)},"finish_reason":null}]}));
                let first = futures_util::stream::once(async move {
                    notify.notify_one();
                    Ok::<_, std::io::Error>(chunk)
                });
                let tail = if let Some(count) = deltas {
                    let chunks = futures_util::stream::iter(0..count).then(|_| async {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        Ok(format!("data: {}\n\n", serde_json::json!({"choices":[{"delta":{"content":"more"},"finish_reason":null}]})))
                    });
                    chunks.chain(futures_util::stream::once(async {
                        Ok("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n".into())
                    })).boxed()
                } else { futures_util::stream::pending().boxed() };
                Response::builder().header("content-type", "text/event-stream")
                    .body(Body::from_stream(first.chain(tail))).unwrap()
            }
        }))).await.unwrap();
    }));
    let fixture = fixture(&format!("http://{address}"));
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args([
            "--profile",
            "test-headless",
            "wait",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--session-id",
            "backpressure",
            "--output",
            "jsonl",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fixture.temporary.path())
        .env("XDG_CONFIG_HOME", fixture.paths.config().parent().unwrap())
        .env("XDG_STATE_HOME", fixture.paths.state().parent().unwrap())
        .env("XDG_CACHE_HOME", fixture.paths.cache().parent().unwrap())
        .env("XDG_RUNTIME_DIR", fixture.temporary.path().join("runtime"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/absent", fixture.temporary.path().display()),
        )
        .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(CHILD_PROVIDER_START_TIMEOUT, sent.notified())
        .await
        .unwrap();
    // Confirm streamed model bytes reached the pipe, then retain it without draining.
    let mut stdout = child.stdout.take().unwrap();
    let mut prefix = vec![0; 4096];
    tokio::time::timeout(Duration::from_secs(5), stdout.read_exact(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    assert!(prefix.windows(32).any(|bytes| bytes == [b'x'; 32]));
    if let Some(count) = deltas {
        wait_for_completed_output(&fixture, count).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(child.try_wait().unwrap().is_none());
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-INT", &child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let result = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    assert!(
        result.is_ok(),
        "SIGINT cleanup waited for the external stdout consumer"
    );
    assert_eq!(result.unwrap().unwrap().code(), Some(130));
    drop(stdout);
    server.abort();
    let _ = server.await;
}

#[cfg(unix)]
async fn wait_for_completed_output(fixture: &Fixture, count: usize) {
    use std::time::Duration;
    // Read-only WAL visibility proves completion without acquiring the running
    // Store's exclusive writer lease or unblocking the product's output pipe.
    let connection = rusqlite::Connection::open_with_flags(
        fixture.paths.state().join("agent/sessions.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (facts, terminal): (i64, i64) = connection.query_row(
                    "SELECT count(*), coalesce(sum(fact_kind = 'terminal'), 0) FROM facts WHERE session_id = ?1",
                    ["backpressure"], |row| Ok((row.get(0)?, row.get(1)?)),
                ).unwrap();
                if terminal > 0 { assert!(facts > i64::try_from(count).unwrap()); break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
}
