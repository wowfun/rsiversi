#![cfg(target_os = "linux")]

use axum::{
    Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use std::io::{BufRead as _, Read as _, Write as _};
use std::process::{Command, Output};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Notify;

struct CliFixture {
    temporary: TempDir,
    workspace: std::path::PathBuf,
}

impl CliFixture {
    fn new(endpoint: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let config = temporary.path().join("config/rsi");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "rsi.agent": {
                    "default_model": {"deployment": "fixture", "model": "fixture-model"}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let host_directory = config.join("host-profiles/fixture");
        let application_directory = config.join("application-profiles/test-headless");
        let session_directory = config.join("application-profiles/test-session");
        std::fs::create_dir_all(&host_directory).unwrap();
        std::fs::create_dir_all(&application_directory).unwrap();
        std::fs::create_dir_all(&session_directory).unwrap();
        std::fs::write(
            host_directory.join("host.profile.toml"),
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
        std::fs::write(
            application_directory.join("application.toml"),
            "format = 1\napplication = \"headless\"\nhost_profile = \"fixture\"\n",
        )
        .unwrap();
        std::fs::write(
            session_directory.join("application.toml"),
            "format = 1\napplication = \"session\"\nhost_profile = \"fixture\"\n",
        )
        .unwrap();
        Self {
            temporary,
            workspace,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rsi"));
        command
            .current_dir(&self.workspace)
            .env("HOME", self.temporary.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temporary.path().join("config"))
            .env("XDG_STATE_HOME", self.temporary.path().join("state"))
            .env("XDG_CACHE_HOME", self.temporary.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.temporary.path().join("runtime"))
            .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
        command
    }

    fn tokio_command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rsi"));
        command
            .current_dir(&self.workspace)
            .env("HOME", self.temporary.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temporary.path().join("config"))
            .env("XDG_STATE_HOME", self.temporary.path().join("state"))
            .env("XDG_CACHE_HOME", self.temporary.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.temporary.path().join("runtime"))
            .env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
        command
    }

    fn require_approval(&self) {
        std::fs::write(
            self.temporary.path().join("config/rsi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "rsi.agent": {
                    "default_model": {"deployment": "fixture", "model": "fixture-model"},
                    "require_approval": true
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn configure_agent_preset_root(&self) {
        let root = self.temporary.path().join("configured-agent-presets");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            self.temporary.path().join("config/rsi/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "rsi.agent": {
                    "default_model": {"deployment": "fixture", "model": "fixture-model"}
                },
                "rsi.agent-presets": {
                    "default": "standard",
                    "roots": [{"path": root, "trust": "user"}]
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn assert_success(&self, arguments: &[&str]) -> Output {
        let output = self.run(arguments);
        assert!(
            output.status.success(),
            "{arguments:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for CliFixture {
    fn drop(&mut self) {
        let _ = self.command().args(["host", "stop", "--force"]).output();
    }
}

async fn chat() -> Response {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hello from daemon\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

async fn provider() -> (String, tokio::task::JoinHandle<()>) {
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

async fn failing_chat() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from("fixture provider failure"))
        .unwrap()
}

async fn failing_provider() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(failing_chat)),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), task)
}

#[derive(Clone)]
struct ApprovalProviderState {
    requested: Arc<Notify>,
}

async fn approval_chat(State(state): State<ApprovalProviderState>) -> Response {
    state.requested.notify_one();
    let arguments = serde_json::to_string(&serde_json::json!({
        "command": "printf approval-finished",
        "run_in_background": false
    }))
    .unwrap();
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices":[{
                "delta":{
                    "role":"assistant",
                    "tool_calls":[{
                        "index":0,
                        "id":"call-await-approval",
                        "type":"function",
                        "function":{"name":"bash","arguments":arguments}
                    }]
                },
                "finish_reason":null
            }]
        }),
        serde_json::json!({
            "choices":[{"delta":{},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":2,"completion_tokens":3}
        })
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

async fn approval_provider() -> (String, Arc<Notify>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requested = Arc::new(Notify::new());
    let state = ApprovalProviderState {
        requested: Arc::clone(&requested),
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(approval_chat))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), requested, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embedded_detach_keeps_ctrl_c_as_an_escape_from_a_pending_approval() {
    let (endpoint, requested, provider) = approval_provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.require_approval();
    let mut child = fixture
        .tokio_command()
        .args([
            "--profile",
            "test-session",
            "--session-id",
            "detach-approval-session",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let process_id = child.id().unwrap().to_string();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"request a tool\n:exit\n").await.unwrap();
    stdin.flush().await.unwrap();
    drop(stdin);
    tokio::time::timeout(std::time::Duration::from_secs(15), requested.notified())
        .await
        .expect("provider request proves the embedded turn is active");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-INT", &process_id])
            .status()
            .await
            .unwrap()
            .success()
    );
    let output = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait_with_output())
        .await
        .expect("Ctrl-C must remain an escape while embedded detach drains")
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embedded_session_reports_a_failed_turn_in_its_process_status() {
    let (endpoint, provider) = failing_provider().await;
    let fixture = CliFixture::new(&endpoint);
    let mut child = fixture
        .tokio_command()
        .args([
            "--profile",
            "test-session",
            "--session-id",
            "failed-session",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"fail this turn\n").await.unwrap();
    drop(stdin);

    let output = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("embedded Session must settle the failed turn after stdin closes")
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    provider.abort();
}

fn assert_remote_interactive_session(fixture: &CliFixture) {
    let mut interactive = fixture.command();
    let mut interactive = interactive
        .args([
            "--profile",
            "test-session",
            "--session-id",
            "interactive-session",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = interactive.stdin.take().unwrap();
    let stdout = interactive.stdout.take().unwrap();
    let stderr = interactive.stderr.take().unwrap();
    let (first_line_sender, first_line_receiver) = std::sync::mpsc::sync_channel(1);
    let stdout_reader = std::thread::spawn(move || {
        let mut stdout = std::io::BufReader::new(stdout);
        let mut bytes = Vec::new();
        stdout.read_until(b'\n', &mut bytes).unwrap();
        first_line_sender.send(bytes.clone()).unwrap();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    stdin.write_all(b"hello interactive\n").unwrap();
    stdin.flush().unwrap();
    let first_line = first_line_receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("interactive turn did not emit its first complete line");
    assert!(String::from_utf8_lossy(&first_line).contains("hello from daemon"));
    stdin
        .write_all(b":queue\n:approvals\n:help\n:exit\n")
        .unwrap();
    drop(stdin);
    let status = interactive.wait().unwrap();
    assert!(status.success());
    let stdout = stdout_reader.join().unwrap();
    assert!(String::from_utf8_lossy(&stdout).contains("hello from daemon"));
    let stderr = stderr_reader.join().unwrap();
    let diagnostics = String::from_utf8_lossy(&stderr);
    assert!(diagnostics.contains("queued: 0"));
    assert!(diagnostics.contains("no pending approvals"));
    assert!(diagnostics.contains(":cancel"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn named_headless_uses_explicit_daemon_and_lifecycle_commands() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);

    let list = fixture.assert_success(&["profile", "application", "list", "--output", "json"]);
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert!(
        list["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "test-headless" && row["source"] == "user")
    );
    fixture.assert_success(&["profile", "host", "preview", "fixture"]);
    fixture.assert_success(&["profile", "application", "copy", "headless", "copied"]);
    fixture.assert_success(&["profile", "application", "path", "copied"]);
    fixture.assert_success(&["profile", "application", "delete", "copied"]);

    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let status = fixture.assert_success(&["host", "status"]);
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(status.starts_with("running\tmode=Daemon"));
    let pid = status
        .split('\t')
        .find_map(|field| field.strip_prefix("pid="))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let end = stat.rfind(')').unwrap();
    let session = stat[end + 1..]
        .split_whitespace()
        .nth(3)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert_eq!(
        session, pid,
        "detached daemon must lead its own Unix session"
    );
    fixture.assert_success(&["host", "reload"]);

    let first = fixture.assert_success(&[
        "--profile",
        "test-headless",
        "hello",
        "--session-id",
        "cli-session",
        "--turn-id",
        "cli-turn",
        "--output",
        "jsonl",
    ]);
    let lines = String::from_utf8(first.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.first().unwrap()["type"], "session");
    assert!(lines.iter().any(|line| {
        line["type"] == "fact"
            && line["fact"]["type"] == "model_event"
            && line.to_string().contains("hello from daemon")
    }));
    assert_eq!(lines.last().unwrap()["type"], "outcome");

    let retried = fixture.assert_success(&[
        "--profile",
        "test-headless",
        "hello",
        "--resume",
        "cli-session",
        "--turn-id",
        "cli-turn",
        "--output",
        "jsonl",
    ]);
    assert!(String::from_utf8_lossy(&retried.stdout).contains("\"type\":\"outcome\""));
    let conflict = fixture.run(&[
        "--profile",
        "test-headless",
        "changed",
        "--resume",
        "cli-session",
        "--turn-id",
        "cli-turn",
    ]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("conflicts"));

    assert_remote_interactive_session(&fixture);

    fixture.assert_success(&["host", "stop"]);
    let status = fixture.assert_success(&["host", "status"]);
    assert_eq!(String::from_utf8_lossy(&status.stdout), "stopped\n");
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_profile_preview_matches_the_settings_backed_daemon_launch_key() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.configure_agent_preset_root();

    let preview =
        fixture.assert_success(&["profile", "host", "preview", "fixture", "--output", "json"]);
    let preview: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    let preview_key = preview["launch_key"].as_str().unwrap();

    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let status = fixture.assert_success(&["host", "status"]);
    let status = String::from_utf8(status.stdout).unwrap();
    let daemon_key = status
        .trim()
        .split('\t')
        .find_map(|field| field.strip_prefix("key="))
        .unwrap();
    assert_eq!(preview_key, daemon_key);

    fixture.assert_success(&["host", "stop"]);
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_uses_the_recorded_daemon_endpoint_across_runtime_directories() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);

    let output = fixture
        .command()
        .env("XDG_RUNTIME_DIR", "/tmp")
        .args([
            "--profile",
            "test-headless",
            "hello",
            "--session-id",
            "cross-runtime-session",
            "--turn-id",
            "cross-runtime-turn",
            "--output",
            "jsonl",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    fixture.assert_success(&["host", "stop"]);
    provider.abort();
}
