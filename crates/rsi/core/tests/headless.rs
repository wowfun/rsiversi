use async_trait::async_trait;
use axum::{
    Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use rsi::{
    OutputMode, RunEvent, RunImageOptions, RunOptions, RunningRsi, SessionSelection,
    StandardComposition,
};
use rsi_agent_session_protocol::{SessionFact, SessionFactBody, SessionId, TurnOutcome};
use rsi_agent_store_protocol::SessionStore as _;
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_credentials_local::SecretStore;
use rsi_credentials_protocol::{CredentialsError, Result as CredentialResult, SecretValue};
use rsi_host::HostPaths;
use rsi_jobs::{JobOutcome, JobSpec, JobTask};
use rsi_sandbox::SandboxMode;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct IgnoresCancellationUntilReleased {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl JobTask for IgnoresCancellationUntilReleased {
    async fn run(&self, _cancellation: CancellationToken) -> rsi_jobs::Result<serde_json::Value> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(serde_json::Value::Null)
    }
}

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
    )
    .with_credential_store(Arc::new(EmptySecretStore))
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

async fn job_gated_chat(State(entered): State<Arc<Notify>>) -> Response {
    entered.notified().await;
    chat().await
}

async fn job_gated_server(entered: Arc<Notify>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(job_gated_chat))
                .with_state(entered),
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
    assert_eq!(first.text_output(), "hello");
    assert!(first.durable_seq() >= first.facts().last().unwrap().seq());
    assert!(matches!(
        first.facts().first().unwrap().body(),
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
    let lines = first.jsonl_output().unwrap();
    let decoded = lines
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(decoded.first().unwrap()["type"], "session");
    assert_eq!(decoded.last().unwrap()["type"], "outcome");
    assert_eq!(
        decoded.iter().filter(|line| line["type"] == "fact").count(),
        first.facts().len()
    );
    assert!(
        decoded
            .iter()
            .filter(|line| line["type"] == "fact")
            .all(|line| line["durable_seq"].as_u64() == Some(first.durable_seq()))
    );

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
async fn unfinished_jobs_are_cancelled_before_terminal_and_timeout_fails_the_turn() {
    let job_entered = Arc::new(Notify::new());
    let job_release = Arc::new(Notify::new());
    let (endpoint, server) = job_gated_server(Arc::clone(&job_entered)).await;
    let fixture = fixture(&endpoint);
    let mut profile = std::fs::read_to_string(&fixture.profile).unwrap();
    profile.push_str(
        r#"

[[steps]]
kind = "patch"
target = "rsi-jobs"

[steps.config]
maximum_active_jobs = 2
shutdown_timeout_ms = 5
"#,
    );
    std::fs::write(&fixture.profile, profile).unwrap();
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let mut job = None;
    let report = running
        .run_turn_observed(
            RunOptions {
                task: "finish with a pending job".into(),
                session: SessionSelection::Fresh {
                    cwd: fixture.workspace.clone(),
                    session_id: None,
                },
                model: None,
                sandbox: None,
                output: OutputMode::Jsonl,
            },
            CancellationToken::new(),
            |event| {
                if let RunEvent::Session {
                    session_id,
                    turn_id,
                    ..
                } = event
                {
                    job = Some(running.submit_turn_job(
                        session_id,
                        turn_id,
                        JobSpec {
                            name: "ignore-cancel-briefly".into(),
                            task: Arc::new(IgnoresCancellationUntilReleased {
                                entered: Arc::clone(&job_entered),
                                release: Arc::clone(&job_release),
                            }),
                        },
                    )?);
                }
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        report.outcome(),
        TurnOutcome::Failed { code, .. } if code == "jobs.cancellation_timeout"
    ));
    assert!(matches!(
        report.facts().last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Failed { code, .. },
            ..
        } if code == "jobs.cancellation_timeout"
    ));
    job_release.notify_one();
    assert_eq!(job.unwrap().join().await, JobOutcome::Cancelled);
    assert!(running.shutdown().await.is_clean());
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
    let encoded = serde_json::to_string(image_fact).unwrap();
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
