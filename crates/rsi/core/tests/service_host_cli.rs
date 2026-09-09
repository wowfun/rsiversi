#![cfg(target_os = "linux")]

#[path = "service_host_cli/commands.rs"]
mod commands;
#[path = "service_host_cli/serve.rs"]
mod serve;
#[path = "service_host_cli/tui.rs"]
mod tui;

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
        let cli_directory = config.join("application-profiles/test-cli");
        std::fs::create_dir_all(&host_directory).unwrap();
        std::fs::create_dir_all(&application_directory).unwrap();
        std::fs::create_dir_all(&cli_directory).unwrap();
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
            application_directory.join("application.profile.toml"),
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"connection\"\nplugin = \"rsi.application.connection\"\nconfig = { host_profile = \"fixture\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.headless\"\n",
        )
        .unwrap();
        std::fs::write(
            cli_directory.join("application.profile.toml"),
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"connection\"\nplugin = \"rsi.application.connection\"\nconfig = { host_profile = \"fixture\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.cli\"\n",
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
            "{arguments:?} failed\nstdout: {}\nstderr: {}\nowner log: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            self.owner_log()
        );
        output
    }

    fn owner_log(&self) -> String {
        use std::io::Read as _;
        let mut log = String::new();
        let path = self
            .temporary
            .path()
            .join("state/rsi/session-host/owner.log");
        let result = std::fs::File::open(path)
            .and_then(|file| file.take(64 * 1024).read_to_string(&mut log));
        result.map_or_else(|error| format!("unavailable: {error}"), |_| log)
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

fn assert_remote_interactive_session(fixture: &CliFixture) {
    let mut interactive = fixture.command();
    let mut interactive = interactive
        .args([
            "--profile",
            "test-cli",
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
        .recv_timeout(std::time::Duration::from_secs(10))
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
    let results = serde_json::Deserializer::from_slice(&stdout[first_line.len()..])
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(results, [serde_json::json!([]), serde_json::json!([])]);
    let stderr = stderr_reader.join().unwrap();
    let diagnostics = String::from_utf8_lossy(&stderr);
    assert!(!diagnostics.contains("inspection:"));
    assert!(!diagnostics.contains("approvals:"));
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
        "--message-id",
        "cli-message",
        "--output",
        "jsonl",
    ]);
    let lines = String::from_utf8(first.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines.first().unwrap()["type"], "message");
    assert_eq!(lines.get(1).unwrap()["type"], "turn");
    assert!(lines.iter().any(|line| {
        line["type"] == "fact"
            && line["fact"]["type"] == "model_event"
            && line.to_string().contains("hello from daemon")
    }));
    assert_eq!(lines.last().unwrap()["type"], "outcome");

    fixture.assert_success(&["host", "restart", "--profile", "fixture"]);
    let retried = fixture.assert_success(&[
        "--profile",
        "test-headless",
        "hello",
        "--resume",
        "cli-session",
        "--message-id",
        "cli-message",
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
        "--message-id",
        "cli-message",
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
            "--message-id",
            "cross-runtime-message",
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

#[test]
fn host_start_rejects_a_symlinked_owner_directory_without_chmod_of_its_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let fixture = CliFixture::new("http://127.0.0.1:9");
    let target = fixture.temporary.path().join("unrelated");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let state = fixture.temporary.path().join("state/rsi");
    std::fs::create_dir_all(&state).unwrap();
    symlink(&target, state.join("session-host")).unwrap();
    let output = fixture.run(&["host", "start", "--profile", "fixture"]);
    assert!(!output.status.success());
    assert_eq!(
        std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

struct JsonClient {
    child: tokio::process::Child,
    input: Option<tokio::process::ChildStdin>,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    seen: Vec<serde_json::Value>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interactive_failed_turn_is_reported_while_successful_detachment_exits_zero() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(|| async {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "injected provider failure",
                    )
                }),
            ),
        )
        .await
        .unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    let mut client = JsonClient::start(&fixture, &["--session-id", "failed-turn-detach"]);
    client.send("hello\n").await;
    let outcome = client.until(|value| value["type"] == "outcome").await;
    assert_eq!(outcome["outcome"]["status"], "failed");
    client.send(":exit\n").await;
    let observations = client.finish().await;
    assert!(
        observations
            .iter()
            .any(|value| value["type"] == "fact" && value["fact"]["type"] == "turn_terminal")
    );
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_observation_closes_the_attachment_even_while_stdin_is_open() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let mut client = JsonClient::start(&fixture, &["--session-id", "observer-exhaustion"]);
    client.send("hello\n").await;
    client.until(|value| value["type"] == "outcome").await;
    fixture.assert_success(&["host", "stop"]);
    let stopped =
        tokio::time::timeout(std::time::Duration::from_secs(10), client.child.wait()).await;
    assert!(
        stopped.is_ok(),
        "attachment kept accepting input after permanent observation loss"
    );
    assert!(!stopped.unwrap().unwrap().success());
    provider.abort();
}
impl JsonClient {
    fn start(fixture: &CliFixture, selection: &[&str]) -> Self {
        use tokio::io::AsyncBufReadExt as _;
        let mut child = fixture
            .tokio_command()
            .args(["--profile", "test-cli", "--output", "jsonl"])
            .args(selection)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take(),
            lines: tokio::io::BufReader::new(child.stdout.take().unwrap()).lines(),
            child,
            seen: Vec::new(),
        }
    }
    async fn send(&mut self, input: &str) {
        self.input
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .await
            .unwrap();
    }
    async fn until(&mut self, predicate: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("client closed JSONL stream");
                let value: serde_json::Value =
                    serde_json::from_str(&line).expect("stdout must contain only JSONL");
                assert_eq!(value["version"], 4);
                assert_ne!(value["type"], "error", "{value}");
                self.seen.push(value.clone());
                if predicate(&value) {
                    return value;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("client timed out; observations: {:?}", self.seen))
    }
    async fn interrupt(&self) {
        assert!(
            tokio::process::Command::new("/bin/kill")
                .args(["-INT", &self.child.id().unwrap().to_string()])
                .status()
                .await
                .unwrap()
                .success()
        );
    }
    async fn finish(mut self) -> Vec<serde_json::Value> {
        self.input.take();
        while let Some(line) =
            tokio::time::timeout(std::time::Duration::from_secs(15), self.lines.next_line())
                .await
                .unwrap()
                .unwrap()
        {
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["version"], 4);
            self.seen.push(value);
        }
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.child.wait_with_output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.seen
    }
}

#[derive(Clone)]
struct GatedState {
    requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    requested: Arc<Notify>,
    release: Arc<Notify>,
    tool: &'static str,
    arguments: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
}
async fn gated_chat(
    State(state): State<GatedState>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let first = {
        let mut requests = state.requests.lock().unwrap();
        requests.push(request);
        requests.len() == 1
    };
    if !first {
        return chat().await;
    }
    state.requested.notify_one();
    state.release.notified().await;
    let arguments = state.arguments.lock().unwrap().clone().unwrap_or_else(|| match state.tool {
        "ask_user" => {
            serde_json::json!({"questions":[{"id":"choice","prompt":"Pick a color","options":["red","blue"]},{"id":"detail","prompt":"Why?","options":[]}]})
        }
        _ => serde_json::json!({"command":"printf first"}),
    });
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"cli-tool","type":"function","function":{"name":state.tool,"arguments":arguments.to_string()}}]},"finish_reason":null}]}),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3}})
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}
async fn gated_provider(tool: &'static str) -> (String, GatedState, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let state = GatedState {
        requests: Arc::default(),
        requested: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        tool,
        arguments: Arc::default(),
    };
    let service = Router::new()
        .route("/v1/chat/completions", post(gated_chat))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, service).await.unwrap();
    });
    (endpoint, state, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_queue_steering_cancel_and_observation_survive_client_reattachment() {
    let (endpoint, provider, task) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let mut client = JsonClient::start(&fixture, &["--session-id", "continuous"]);
    client.send("initial task\n").await;
    client.until(|v| v["type"] == "message").await;
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        provider.requested.notified(),
    )
    .await
    .unwrap();
    client
        .send("private followup\nremove me\n:steer current correction\n:queue\n")
        .await;
    client.until(|v| v["type"] == "message").await;
    let cancelled = client.until(|v| v["type"] == "message").await["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.until(|v| v["type"] == "message").await;
    let queue = client.until(|v| v["type"] == "inspection").await;
    let pending = queue["data"].as_array().unwrap();
    assert_eq!(pending.len(), 3);
    assert_eq!(pending[2]["delivery"], "steer");
    assert!(pending[2]["bound_turn_id"].is_string());
    client.send(&format!(":cancel {cancelled}\n:exit\n")).await;
    client.finish().await;
    let mut attached = JsonClient::start(&fixture, &["--resume", "continuous"]);
    let snapshot = attached.until(|v| v["type"] == "session").await;
    assert_eq!(snapshot["data"]["pending"].as_array().unwrap().len(), 2);
    let turn = snapshot["data"]["active_turn_id"].clone();
    attached.interrupt().await;
    attached.send(":status\n").await;
    let inspected = attached.until(|v| v["type"] == "inspection").await;
    assert_eq!(
        inspected["data"]["active_turn_id"], turn,
        "Ctrl-C on an attachment must not cancel another client's Turn"
    );
    provider.release.notify_one();
    for _ in 0..2 {
        attached.until(|v| v["type"] == "outcome").await;
    }
    attached.send(":history\n:history\n:exit\n").await;
    let observations = attached.finish().await;
    assert_eq!(
        observations
            .iter()
            .filter(|v| v["type"] == "outcome")
            .count(),
        2
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("current correction")
    );
    assert!(
        !requests[1]["messages"]
            .to_string()
            .contains("private followup")
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("private followup")
    );
    assert!(!requests[2]["messages"].to_string().contains("remove me"));
    drop(requests);
    let history = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--history",
        "continuous",
        "--output",
        "jsonl",
    ]);
    assert!(String::from_utf8_lossy(&history.stdout).contains("turn_terminal"));
    let listed = fixture.assert_success(&["--profile", "test-cli", "--list", "--output", "jsonl"]);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("continuous"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embedded_eof_interrupts_active_work_but_retains_queued_and_promoted_input() {
    let (endpoint, provider, task) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    let mut client = JsonClient::start(&fixture, &["--session-id", "eof-retained"]);
    client.send("initial task\n").await;
    client.until(|v| v["type"] == "message").await;
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        provider.requested.notified(),
    )
    .await
    .unwrap();
    client
        .send("next durable task\n:steer retained correction\n")
        .await;
    for _ in 0..2 {
        client.until(|v| v["type"] == "message").await;
    }
    client.finish().await;
    let mut resumed = JsonClient::start(&fixture, &["--resume", "eof-retained"]);
    resumed.until(|v| v["type"] == "session").await;
    // Resuming the Host schedules preserved input immediately, possibly before inspection.
    while resumed
        .seen
        .iter()
        .filter(|v| {
            v["type"] == "fact"
                && v["fact"]["type"] == "turn_terminal"
                && v["fact"]["outcome"]["status"] == "completed"
        })
        .count()
        < 2
    {
        resumed
            .until(|v| {
                v["type"] == "fact"
                    && v["fact"]["type"] == "turn_terminal"
                    && v["fact"]["outcome"]["status"] == "completed"
            })
            .await;
    }
    resumed.send(":exit\n").await;
    let observations = resumed.finish().await;
    assert!(observations.iter().any(|v| v["type"] == "fact"
        && v["fact"]["type"] == "turn_terminal"
        && v["fact"]["outcome"]["status"] != "completed"));
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("next durable task")
    );
    assert!(
        requests[2]["messages"]
            .to_string()
            .contains("retained correction")
    );
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn question_draft_interrupt_and_same_host_reconnect_preserve_the_pending_question() {
    let (endpoint, provider, task) = gated_provider("ask_user").await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let mut client = JsonClient::start(&fixture, &["--session-id", "question-draft"]);
    client.send("ask two questions\n").await;
    provider.release.notify_one();
    let interaction = client
        .until(|v| {
            v["type"] == "interactions"
                && v["data"]["questions"]
                    .as_array()
                    .is_some_and(|q| !q.is_empty())
        })
        .await;
    let id = interaction["data"]["questions"][0]["id"].as_str().unwrap();
    client.send(&format!(":answer {id}\n1\n")).await;
    client
        .until(|v| v["type"] == "answer_prompt" && v["data"]["index"] == 2)
        .await;
    client.interrupt().await;
    client.until(|v| v["type"] == "answer_abandoned").await;
    client.send(":exit\n").await;
    client.finish().await;
    let mut resumed = JsonClient::start(&fixture, &["--resume", "question-draft"]);
    let replay = resumed
        .until(|v| {
            v["type"] == "interactions"
                && v["data"]["questions"]
                    .as_array()
                    .is_some_and(|q| !q.is_empty())
        })
        .await;
    assert_eq!(replay["data"]["questions"][0]["id"], id);
    resumed
        .send(&format!(":answer {id}\n2\nbecause calm\n"))
        .await;
    resumed.until(|v| v["type"] == "question_answer").await;
    resumed.until(|v| v["type"] == "outcome").await;
    resumed.send(":exit\n").await;
    resumed.finish().await;
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(ToString::to_string)
        .collect::<String>();
    assert!(messages.contains("blue") && messages.contains("because calm"));
    assert!(!messages.contains("\\\"red\\\""));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_reviews_the_prepared_request_and_denial_never_starts_it() {
    for allow in [false, true] {
        let (endpoint, provider, task) = gated_provider("bash").await;
        let fixture = CliFixture::new(&endpoint);
        fixture.require_approval();
        let mut client = JsonClient::start(&fixture, &["--session-id", "review"]);
        client.send("run reviewed effect\n").await;
        provider.release.notify_one();
        let interaction = client
            .until(|v| {
                v["type"] == "interactions"
                    && v["data"]["approvals"]
                        .as_array()
                        .is_some_and(|q| !q.is_empty())
            })
            .await;
        let request = &interaction["data"]["approvals"][0];
        let id = request["id"].as_str().unwrap();
        assert_eq!(
            request["review"]["arguments"],
            serde_json::json!({"command":"printf first"})
        );
        assert_eq!(
            request["review"]["cwd"],
            fixture.workspace.to_str().unwrap()
        );
        assert_eq!(request["review"]["sandbox"], "workspace-write");
        assert_eq!(
            request["review"]["request_sha256"].as_str().unwrap().len(),
            64
        );
        assert!(
            !client
                .seen
                .iter()
                .any(|v| v["type"] == "fact" && v["fact"]["type"] == "tool_started")
        );
        let decision = if allow { "allow" } else { "deny" };
        let owner = request["subject"]["session_id"].as_str().unwrap();
        client
            .send(&format!(
                ":{decision} {owner} {id}\n:{decision} {owner} {id}\n"
            ))
            .await;
        for _ in 0..2 {
            let receipt = client.until(|v| v["type"] == "approval_answer").await;
            assert_eq!(receipt["data"]["accepted"], true);
        }
        if !client.seen.iter().any(|v| v["type"] == "outcome") {
            client.until(|v| v["type"] == "outcome").await;
        }
        client.send(":exit\n").await;
        let observations = client.finish().await;
        let intent = observations
            .iter()
            .find(|v| v["type"] == "fact" && v["fact"]["type"] == "tool_intent");
        if allow {
            let intent = intent.unwrap();
            assert_eq!(intent["fact"]["arguments"], request["review"]["arguments"]);
            assert_eq!(
                intent["fact"]["identity"]["request_sha256"],
                request["review"]["request_sha256"]
            );
            assert!(
                observations
                    .iter()
                    .any(|v| v["type"] == "fact" && v["fact"]["type"] == "tool_result")
            );
            let result = observations
                .iter()
                .find(|v| v["type"] == "fact" && v["fact"]["type"] == "tool_result")
                .unwrap();
            assert_eq!(result["fact"]["result"]["is_error"], false);
            assert_eq!(result["fact"]["result"]["value"]["stdout"]["text"], "first");
            assert_eq!(provider.requests.lock().unwrap().len(), 2);
        } else {
            assert!(intent.is_none());
            assert!(
                !observations
                    .iter()
                    .any(|v| v["type"] == "fact" && v["fact"]["type"] == "tool_started")
            );
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            assert!(
                observations
                    .iter()
                    .any(|v| v["type"] == "outcome" && v["outcome"]["status"] == "failed")
            );
        }
        task.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_restart_interrupts_a_human_wait_without_recreating_its_question() {
    let (endpoint, provider, task) = gated_provider("ask_user").await;
    let fixture = CliFixture::new(&endpoint);
    let mut client = JsonClient::start(&fixture, &["--session-id", "question-restart"]);
    client.send("ask before restart\n").await;
    provider.release.notify_one();
    let interaction = client
        .until(|v| {
            v["type"] == "interactions"
                && v["data"]["questions"]
                    .as_array()
                    .is_some_and(|q| !q.is_empty())
        })
        .await;
    let id = interaction["data"]["questions"][0]["id"].clone();
    client.finish().await;
    let mut resumed = JsonClient::start(&fixture, &["--resume", "question-restart"]);
    resumed
        .until(|v| v["type"] == "fact" && v["fact"]["type"] == "turn_terminal")
        .await;
    resumed.send(":questions\n").await;
    let questions = resumed.until(|v| v["type"] == "questions").await;
    assert_eq!(questions["data"], serde_json::json!([]));
    resumed.send(":exit\n").await;
    let observations = resumed.finish().await;
    assert!(observations.iter().all(|v| {
        v["type"] != "interactions"
            || !v["data"]["questions"]
                .as_array()
                .is_some_and(|q| q.iter().any(|question| question["id"] == id))
    }));
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "restart must not replay the unanswered model effect"
    );
    task.abort();
}
