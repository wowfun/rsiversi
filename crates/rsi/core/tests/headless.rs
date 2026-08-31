use axum::{
    Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use rsi::{
    OutputMode, RunEvent, RunImageOptions, RunOptions, RunningRsi, SessionSelection,
    StandardCodingTools, StandardComposition,
};
use rsi_agent_session_protocol::{
    AgentPresetId, SessionFact, SessionFactBody, SessionId, TurnOutcome,
};
use rsi_agent_store_protocol::SessionStore as _;
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_credentials_local::SecretStore;
use rsi_credentials_protocol::{CredentialsError, Result as CredentialResult, SecretValue};
use rsi_host::HostPaths;
use rsi_sandbox::SandboxMode;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

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
    let profile = config.join("profile.toml");
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

fn background_job_id(request: &serde_json::Value) -> &str {
    tool_message(request, "call-background-bash")["content"]
        .as_str()
        .unwrap()
        .strip_prefix("Started background Bash job ")
        .unwrap()
        .strip_suffix('.')
        .unwrap()
}

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
}

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
        _ => completed_chat_response("all coding tools completed"),
    }
}

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

fn assert_versioned_run_events(events: &[RunEvent], observed_fact_count: usize) {
    let decoded = events
        .iter()
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(&event.json_line().unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(decoded.iter().all(|line| line["version"] == 2));
    assert_eq!(decoded.first().unwrap()["type"], "session");
    assert_eq!(decoded.last().unwrap()["type"], "outcome");
    assert_eq!(
        decoded.iter().filter(|line| line["type"] == "fact").count(),
        observed_fact_count
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_profile_runs_fresh_and_resume_through_durable_plugins() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let mut events = Vec::new();
    let first = running
        .run_turn_observed(
            RunOptions {
                task: "/status".into(),
                session: SessionSelection::Fresh {
                    cwd: fixture.workspace.clone(),
                    session_id: None,
                    agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
                },
                model: None,
                sandbox: None,
                output: OutputMode::Jsonl,
            },
            CancellationToken::new(),
            |event| {
                events.push(event.clone());
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(first.outcome(), &TurnOutcome::Completed);
    assert_eq!(first.exit_code(), 0);
    let observed_facts = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::Fact { fact, .. } => Some(fact),
            RunEvent::Session { .. } | RunEvent::Outcome { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(first.durable_seq() >= observed_facts.last().unwrap().seq());
    assert!(matches!(
        observed_facts.first().unwrap().body(),
        SessionFactBody::TurnAccepted { text, .. } if text == "/status"
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        RunEvent::Fact {
            fact,
            durable_seq,
            ..
        } if *durable_seq < fact.seq()
    )));
    assert!(observed_facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ModelEvent {
            event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                delta: rsi_ai_protocol::ContentDelta::Text(text),
                ..
            },
            ..
        } if text == "hello"
    )));
    assert_versioned_run_events(&events, observed_facts.len());

    let second = running
        .run_turn(
            RunOptions {
                task: "again".into(),
                session: SessionSelection::Resume {
                    session_id: first.session_id().clone(),
                    cwd: Some(fixture.workspace.clone()),
                },
                model: Some(ModelRef::new("fixture", "fixture-model").unwrap()),
                sandbox: Some(SandboxMode::ReadOnly),
                output: OutputMode::Text,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(second.outcome(), &TurnOutcome::Completed);
    assert_eq!(second.text_output(), "hello");
    assert!(second.facts().first().unwrap().seq() > first.durable_seq());
    assert!(matches!(
        second.facts().first().unwrap().body(),
        SessionFactBody::TurnAccepted {
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
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let first = running
        .run_turn(
            RunOptions {
                task: "first".into(),
                session: SessionSelection::Fresh {
                    cwd: fixture.workspace.clone(),
                    session_id: None,
                    agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
                },
                model: None,
                sandbox: None,
                output: OutputMode::Text,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let resumed = running
        .run_turn(
            RunOptions {
                task: "second".into(),
                session: SessionSelection::Resume {
                    session_id: first.session_id().clone(),
                    cwd: Some(other),
                },
                model: None,
                sandbox: None,
                output: OutputMode::Text,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(resumed.is_err());
    assert!(running.shutdown().await.is_clean());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_preserves_jsonl_text_and_success_stderr_contracts() {
    let (endpoint, server) = server().await;
    let fixture = fixture(&endpoint);
    let binary = env!("CARGO_BIN_EXE_rsi");
    let alternate_profile = fixture.paths.config().join("alternate.toml");
    std::fs::rename(&fixture.profile, &alternate_profile).unwrap();
    let first = binary_command(binary, &fixture)
        .args([
            "run",
            "/status",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--session-id",
            "session-binary",
            "--profile",
            alternate_profile.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
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
    assert_eq!(lines.first().unwrap()["type"], "session");
    assert_eq!(lines.first().unwrap()["session_id"], "session-binary");
    assert_eq!(lines.last().unwrap()["type"], "outcome");
    assert!(lines.iter().any(|line| {
        line["type"] == "fact"
            && line["durable_seq"].as_u64().unwrap() < line["fact"]["seq"].as_u64().unwrap()
    }));
    let session_id = lines.first().unwrap()["session_id"].as_str().unwrap();

    let second = binary_command(binary, &fixture)
        .args([
            "run",
            "again",
            "--resume",
            session_id,
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--profile",
            alternate_profile.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(second.stdout, b"hello\n");
    assert!(second.stderr.is_empty());

    let mut third = binary_command(binary, &fixture)
        .args([
            "run",
            "--stdin",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--profile",
            alternate_profile.to_str().unwrap(),
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
    assert!(third.stderr.is_empty());
    server.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn built_binary_patch_helper_requires_the_sole_marker_and_uses_one_line_protocol() {
    let binary = env!("CARGO_BIN_EXE_rsi");
    let workspace = tempfile::tempdir().unwrap();
    let patch = concat!(
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
        .output()
        .await
        .unwrap();
    assert_eq!(extra.status.code(), Some(2));
    assert!(extra.stdout.is_empty());
    assert!(String::from_utf8_lossy(&extra.stderr).contains("only the `run` command"));
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_runs_the_complete_real_coding_tool_flow() {
    let (endpoint, calls, requests, server) = tool_server().await;
    let fixture = fixture(&endpoint);
    let output = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "run",
            "exercise all coding tools",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--sandbox",
            "workspace-write",
            "--output",
            "jsonl",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 7);
    assert_eq!(
        std::fs::read(fixture.workspace.join("from-model.txt")).unwrap(),
        b"written through the complete tool loop\n"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 7);
    let mut tool_names = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    assert_eq!(
        tool_names,
        ["apply_patch", "bash", "job_kill", "job_list", "job_output"]
    );
    assert_eq!(
        tool_message(&requests[1], "call-foreground-bash")["content"],
        "foreground-complete"
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
            "run",
            "start work but forget to collect it",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--sandbox",
            "workspace-write",
            "--output",
            "jsonl",
        ])
        .output()
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
            "run",
            "attempt a patch and handle rejection",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--sandbox",
            "workspace-write",
            "--output",
            "jsonl",
        ])
        .output()
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
    assert_eq!(result["fact"]["result"]["value"], evidence);
    assert_eq!(lines.last().unwrap()["outcome"]["status"], "completed");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn built_binary_uses_fixed_failure_exit_classes() {
    let binary = env!("CARGO_BIN_EXE_rsi");
    let usage = tokio::process::Command::new(binary)
        .args(["run", "task", "--stdin"])
        .output()
        .await
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    assert!(usage.stdout.is_empty());
    assert!(String::from_utf8_lossy(&usage.stderr).contains("exactly one"));

    let missing_route = fixture("http://127.0.0.1:9");
    std::fs::write(&missing_route.profile, "format = 1\n").unwrap();
    let boot = tokio::process::Command::new(binary)
        .args([
            "run",
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
        .output()
        .await
        .unwrap();
    assert_eq!(boot.status.code(), Some(2));
    assert!(boot.stdout.is_empty());
    assert!(String::from_utf8_lossy(&boot.stderr).contains("not registered"));

    let (endpoint, server) = failed_server().await;
    let fixture = fixture(&endpoint);
    let failed = tokio::process::Command::new(binary)
        .args([
            "run",
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
        .output()
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
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rsi"))
        .args([
            "run",
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
        .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        first_request_started.notified(),
    )
    .await
    .expect("provider request proves the child installed its runtime signal path");
    let process_id = child.id().unwrap().to_string();
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-INT", &process_id])
            .status()
            .await
            .unwrap()
            .success()
    );
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .expect("SIGINT shutdown bound")
        .unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(output.stderr.is_empty());
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.first().unwrap()["type"], "session");
    assert!(
        lines
            .iter()
            .any(|line| { line["type"] == "fact" && line["fact"]["type"] == "cancel_requested" })
    );
    assert_eq!(lines.last().unwrap()["type"], "outcome");
    assert_eq!(lines.last().unwrap()["outcome"]["status"], "cancelled");
    server.abort();
}

fn assert_sigkill_recovery_order(facts: &[SessionFact]) {
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::TurnAccepted { .. }))
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
    let child = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "run",
            "first",
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--session-id",
            session_id,
            "--output",
            "jsonl",
        ])
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        first_request_started.notified(),
    )
    .await
    .expect("first provider request should start after its effect prefix is durable");
    let process_id = child.id().unwrap().to_string();
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-KILL", &process_id])
            .status()
            .await
            .unwrap()
            .success()
    );
    let killed = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .expect("SIGKILL process reap bound")
        .unwrap();
    assert!(!killed.status.success());

    let resumed = binary_command(env!("CARGO_BIN_EXE_rsi"), &fixture)
        .args([
            "run",
            "second",
            "--resume",
            session_id,
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--output",
            "jsonl",
        ])
        .output()
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
    let report = running
        .run_image(
            RunImageOptions {
                session: SessionSelection::Fresh {
                    cwd: fixture.workspace.clone(),
                    session_id: None,
                    agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
                },
                model: ModelRef::new("fixture", "gpt-image-1").unwrap(),
                request: ImageRequest::new("one pixel", 1).unwrap(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.outcome(), &TurnOutcome::Completed);
    let output = report.text_output();
    assert!(output.starts_with("media:"));
    assert_eq!(output.lines().count(), 1);
    let image_fact = report
        .facts()
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
