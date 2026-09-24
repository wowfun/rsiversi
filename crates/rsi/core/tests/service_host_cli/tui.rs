#[path = "tui/language.rs"]
mod language;
use super::*;
#[path = "tui/capture.rs"]
mod capture;
use capture::RawCapture;
#[path = "tui/dialogs.rs"]
mod dialogs;
#[path = "tui/export.rs"]
mod export;
#[path = "tui/external.rs"]
mod external;
#[path = "tui/history.rs"]
mod history;
#[path = "tui/live.rs"]
mod live;
#[path = "tui/navigation.rs"]
mod navigation;
#[path = "tui/profiles.rs"]
mod profiles;
#[path = "tui/resources.rs"]
mod resources;
#[path = "tui/setup.rs"]
mod setup;
#[path = "tui/workspace_review.rs"]
mod workspace_review;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::time::{Duration, Instant};

struct TerminalClient {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    output: Arc<std::sync::Mutex<RawCapture>>,
    reader: Option<std::thread::JoinHandle<()>>,
    screen: Arc<std::sync::Mutex<vt100::Parser>>,
    capture_name: String,
    capture_count: usize,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_agent_tree_reads_child_history_without_switching_session() {
    for remote in [false, true] {
        let (endpoint, state, provider) = gated_provider("spawn_agent").await;
        *state.arguments.lock().unwrap() = Some(serde_json::json!({
            "task_name":"inspect-child", "message":"Child inspector evidence", "fork_turns":"none"
        }));
        let fixture = CliFixture::new(&endpoint);
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let name = if remote {
            "tui-tree-remote"
        } else {
            "tui-tree-local"
        };
        let mut terminal = TerminalClient::start(&fixture, &["--session-id", name]);
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"Inspect a child task\r");
        tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
            .await
            .unwrap();
        state.release.notify_one();
        terminal.until("hello from daemon").await;
        tokio::time::timeout(Duration::from_secs(20), async {
            while state.requests.lock().unwrap().len() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        terminal.send(b"\x10");
        terminal.select_menu("Agent tree").await;
        terminal.until("Read-only snapshots of agents").await;
        terminal.send(b"\r");
        terminal.select_menu("Inspect agent tree").await;
        terminal.until("1–1 of 1").await;
        terminal.send(b"\r");
        terminal.select_menu("Inspect inspect-child").await;
        terminal.until("Direct children: 0–0 of 0").await;
        terminal.until("Agent path: Root › inspect-child").await;
        terminal.send(b"\r");
        terminal.select_menu("Read conversation").await;
        terminal.until("Child inspector evidence").await;
        terminal.until("Read-only history").await;
        terminal.send(b"\r");
        terminal.select_menu("Root agent").await;
        terminal.until(name).await;
        terminal.send(b"\x1b");
        terminal.absent("Activity snapshot").await;
        terminal.send(b"Continue parent conversation\r");
        terminal.until("Continue parent conversation").await;
        terminal.send(b"\x04");
        terminal.finish().await;
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_contributed_session_card_and_exact_source_pages() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-contributions"]);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"\x10");
    terminal.select_menu("Session details").await;
    terminal.until("Session: tui-contributions").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let text = format!("{}second-page-marker", "x".repeat(16 * 1024));
    terminal.send(format!("\x1b[200~{text}\x1b[201~\r").as_bytes());
    terminal.until("hello from daemon").await;
    terminal.send(b"\x10");
    terminal.select_menu("Card details").await;
    terminal.until("Block details").await;
    terminal.send(b"\r");
    terminal.until("Fact ").await;
    terminal.send(b"\r");
    terminal.until("0–16384 · more available").await;
    terminal.send(b"\r");
    terminal.until("Next page").await;
    terminal.send(b"\r");
    terminal.until("second-page-marker").await;
    terminal.until(" · end").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_fixed_enter_survives_session_switch_and_ctrl_s_submits() {
    for remote in [false, true] {
        verify_fixed_enter(remote).await;
    }
}

type SwitchProvider = (
    Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    tokio::sync::watch::Sender<usize>,
);

async fn switch_reply(
    State((requests, count)): State<SwitchProvider>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let index = {
        let mut requests = requests.lock().unwrap();
        requests.push(request);
        requests.len()
    };
    count.send_replace(index);
    let delta = serde_json::json!({"choices":[{"delta":{"role":"assistant","content":format!("switch-reply-{index}")},"finish_reason":null}]});
    Response::builder().status(StatusCode::OK).header("content-type", "text/event-stream")
        .body(Body::from(format!("data: {delta}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"))).unwrap()
}

async fn verify_fixed_enter(remote: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let (count, mut observed_count) = tokio::sync::watch::channel(0_usize);
    let router = Router::new()
        .route("/v1/chat/completions", post(switch_reply))
        .with_state((requests.clone(), count));
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    if remote {
        fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    }
    let name = if remote {
        "tui-enter-remote"
    } else {
        "tui-enter-local"
    };
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", name]);
    terminal.until("Ctrl+J adds a line").await;
    for (index, text) in ["first\x0asecond", "after switch\x0anext line"]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            terminal.action(0);
            terminal
                .until_screen("new Session composer", |screen| {
                    screen.contains("Ctrl+J adds a line")
                        && !screen.contains("Enter select")
                        && !screen.contains("switch-reply-1")
                })
                .await;
            // Opening details before Attached is consumed changes view_revision.
            // Open it once, only after the causal transition above.
            terminal.send(b"\x10");
            terminal.select_menu("Session details").await;
            terminal.until("Session: ").await;
            let details = terminal.screen.lock().unwrap().screen().contents();
            assert!(
                !details.contains(&format!("Session: {name}")),
                "session identity did not change: {details}"
            );
            terminal.send(b"\x1b");
            terminal.absent("Session: ").await;
        }
        terminal.send(text.as_bytes());
        terminal.until(text.split('\n').next_back().unwrap()).await;
        assert_eq!(requests.lock().unwrap().len(), index);
        terminal.send(b"\x13");
        tokio::time::timeout(
            Duration::from_secs(20),
            observed_count.wait_for(|count| *count == index + 1),
        )
        .await
        .unwrap()
        .unwrap();
        terminal.until(&format!("switch-reply-{}", index + 1)).await;
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), index + 1);
        assert!(
            requests[index]
                .to_string()
                .contains(&text.replace('\n', "\\n"))
        );
    }
    terminal.send(b"\x04");
    terminal.finish().await;
    if remote {
        fixture.assert_success(&["host", "stop"]);
    }
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_output_cards_read_both_raw_streams_and_pages() {
    let (endpoint, state, provider) = gated_provider("bash").await;
    *state.arguments.lock().unwrap() = Some(
        serde_json::json!({"command":"printf 'fixture stdout\\n'; printf '%16384s' '' | tr ' ' x; printf 'OUTPUT-NEXT\\000\\377\\n'; printf 'fixture stderr\\000\\377\\n' >&2; exit 7"}),
    );
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-output"]);
    terminal.capture_name = "tool-failure-marker".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Capture both raw streams\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("hello from daemon").await;
    terminal.until("Failed to run").await;
    {
        let parser = terminal.screen.lock().unwrap();
        let screen = parser.screen();
        let row = u16::try_from(
            screen
                .rows(0, 110)
                .position(|row| row.contains("Failed to run"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(screen.cell(row, 0).unwrap().contents(), "▸");
        assert_eq!(screen.cell(row, 0).unwrap().fgcolor(), vt100::Color::Idx(9));
        assert_eq!(screen.cell(row, 2).unwrap().fgcolor(), vt100::Color::Idx(7));
    }
    terminal.send(b"\t");
    terminal.send(b"\x10");
    terminal.select_menu("Card details").await;
    terminal.until("bash · command failed").await;
    terminal.until("Card details · Enter actions").await;
    terminal.send(b"\r");
    terminal.select_menu("Read stdout").await;
    terminal.until("Completed stdout").await;
    terminal.until("fixture stdout").await;
    terminal.send(b"\r");
    terminal.select_menu("Next page").await;
    terminal.until("OUTPUT-NEXT").await;
    // vt100 0.16 discards U+FFFD in Perform::print; verify those bytes at the PTY.
    terminal.until_ansi("��").await;
    terminal.send(b"\r");
    terminal.select_menu("View exact hex").await;
    terminal.until("00004000").await;
    terminal.until("00 ff").await;
    terminal.send(b"\x1b");
    terminal.absent("Completed stdout").await;
    terminal.send(b"\x10");
    terminal.select_menu("Card details").await;
    terminal.until("bash · command failed").await;
    terminal.until("Card details · Enter actions").await;
    terminal.send(b"\r");
    terminal.select_menu("Read stderr").await;
    terminal.until("fixture stderr").await;
    terminal.send(b"\r");
    terminal.select_menu("View exact hex").await;
    terminal
        .until("66 69 78 74 75 72 65 20 73 74 64 65 72 72 00 ff")
        .await;
    terminal.send(b"\x1b");
    terminal.absent("Completed stderr").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_files_pages_exact_bytes_and_explicit_refresh() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let directory = fixture.workspace.join("browse");
    std::fs::create_dir(&directory).unwrap();
    for index in 0..20 {
        std::fs::write(directory.join(format!("{index:02}.txt")), b"small").unwrap();
    }
    let mut bytes = b"FIRST-PAGE\n".to_vec();
    bytes.resize(4096, b'x');
    bytes.extend_from_slice(b"SECOND-PAGE\n\x00\x1b\xff\n");
    std::fs::write(directory.join("00.txt"), &bytes).unwrap();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-files"]);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"\x10");
    terminal.select_menu("Workspace files").await;
    terminal.until("Workspace-relative path").await;
    terminal.send(b"\r");
    terminal.select_menu("Edit Workspace-relative path").await;
    terminal.until("Enter save · Esc discard").await;
    terminal.send(b"browse");
    terminal.until("browse▏").await;
    terminal.send(b"\x10");
    terminal.until("┌Actions").await;
    terminal.send(b"\x1b[200~MUST_NOT_LEAK\x1b[201~");
    terminal.send(b"\x1b");
    terminal.absent("┌Actions").await;
    terminal.until("browse▏").await;
    assert!(
        !terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("MUST_NOT_LEAK")
    );
    terminal.send(b"\r");
    terminal.send(b"\r");
    terminal.select_menu("List directory").await;
    terminal.until("Entries: 0–16 of 20").await;
    terminal.send(b"\r");
    terminal.select_menu("Next page").await;
    terminal.until("Entries: 16–20 of 20").await;
    terminal.send(b"\r");
    terminal.select_menu("Previous page").await;
    terminal.until("Entries: 0–16 of 20").await;
    terminal.send(b"\r");
    terminal.select_menu("00.txt").await;
    terminal.until("Bytes: 0–4096").await;
    terminal.until("FIRST-PAGE").await;
    terminal.send(b"\r");
    terminal.select_menu("Next page").await;
    terminal.until("SECOND-PAGE").await;
    terminal.send(b"\r");
    terminal.select_menu("View exact hex").await;
    terminal.until("00001000  53 45 43 4f 4e 44").await;
    terminal.until("00 1b ff").await;
    std::fs::write(directory.join("00.txt"), b"REFRESHED-PAGE\n").unwrap();
    terminal.send(b"\r");
    terminal.select_menu("View text").await;
    terminal.until("Refresh to open a new snapshot").await;
    terminal.send(b"\r");
    terminal.select_menu("Refresh").await;
    terminal.until("REFRESHED-PAGE").await;
    terminal.send(b"\r");
    terminal.select_menu("Release snapshot").await;
    terminal.absent("REFRESHED-PAGE").await;
    terminal.send(b"\x1b");
    terminal.absent("Workspace-relative path").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_discovers_plan_changes_real_draft_then_durable_state() {
    plan_application(None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit RSI_WORKBENCH_BINARY built from the independent addon fixture"]
async fn independent_addon_tui_and_headless_show_the_same_durable_business_state() {
    let binary =
        std::env::var_os("RSI_WORKBENCH_BINARY").expect("build workbench-addon example first");
    plan_application(Some(binary.into())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit RSI_WORKBENCH_BINARY built from the independent addon fixture"]
async fn independent_addon_tui_renders_saved_typed_result_and_literal_unicode() {
    let binary =
        std::env::var_os("RSI_WORKBENCH_BINARY").expect("build workbench-addon example first");
    let (endpoint, state, provider) = gated_provider("fixture_echo").await;
    *state.arguments.lock().unwrap() =
        Some(serde_json::json!({"message":"中文 <script>literal</script>"}));
    let mut fixture = CliFixture::new(&endpoint);
    configure_independent(&mut fixture, binary.into());
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "typed-result"]);
    terminal.capture_name = "typed-result".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Record a typed result\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("hello from daemon").await;
    terminal.send(b"\t\x10");
    terminal.select_menu("Card details").await;
    terminal.until("fixture_echo").await;
    terminal.until("Card details · Enter actions").await;
    terminal.send(b"\r");
    terminal.select_menu("Recorded result").await;
    terminal.until("Recorded tool result").await;
    terminal.until("fixture.workbench.echo").await;
    terminal.until("Generation label").await;
    terminal.until("中文 <script>literal</script>").await;
    terminal.resize(PtySize {
        cols: 48,
        rows: 28,
        pixel_width: 0,
        pixel_height: 0,
    });
    terminal
        .until_screen("complete narrow result dialog", |screen| {
            screen.contains("Recorded tool result")
                && screen
                    .lines()
                    .any(|line| line.contains("┌Detail") && line.contains('┐'))
        })
        .await;
    terminal.send(b"\x1b");
    terminal.absent("Recorded tool result").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}

async fn plan_application(binary: Option<std::path::PathBuf>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let router = Router::new()
        .route("/v1/chat/completions", post(super::commands::capture))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut fixture = CliFixture::new(&endpoint);
    let independent = binary.is_some();
    if let Some(binary) = binary {
        configure_independent(&mut fixture, binary);
    }
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "pty-plan-command"]);
    terminal.until("Ctrl+J adds a line").await;
    inspect_plan_projection(&mut terminal, false, "Draft").await;
    terminal.send(b"\x10");
    terminal.select_menu("Session commands").await;
    terminal.until("Session commands").await;
    terminal.until("/plan").await;
    terminal.send(b"\x1b");
    let closed = Instant::now() + Duration::from_secs(5);
    while terminal
        .screen
        .lock()
        .unwrap()
        .screen()
        .contents()
        .contains("Session commands")
    {
        assert!(Instant::now() < closed, "command menu did not close");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    terminal.send(b"/pl");
    terminal.until("/plan").await;
    terminal.send(b"\t on\r");
    terminal.absent("/plan").await;
    inspect_plan_projection(&mut terminal, true, "Draft").await;
    assert!(requests.lock().unwrap().is_empty());
    terminal.send(b"inspect plan\r");
    terminal.until("hello from daemon").await;
    // The reply is streamed before the terminal control commit. Wait for the
    // footer's activity projection to settle before executing a versioned command.
    terminal
        .until_screen("idle after the first turn", |screen| {
            screen.lines().rev().nth(1).is_some_and(|footer| {
                footer.contains("fixture-model")
                    && !footer.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧".contains(ch))
            })
        })
        .await;

    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(
        requests.lock().unwrap()[0]
            .to_string()
            .contains("Plan mode is enabled")
    );
    terminal.send(b"/plan off\r");
    terminal.absent("/plan").await;
    inspect_plan_projection(&mut terminal, false, "Durable").await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    terminal.send(b"\x04");
    terminal.finish().await;
    if independent {
        assert!(
            requests.lock().unwrap()[0]
                .to_string()
                .contains("Independent workbench A")
        );
        independent_headless(&fixture);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].to_string().contains("Plan mode is enabled"));
        assert!(requests[1].to_string().contains("Independent workbench A"));
    }
    provider.abort();
}

fn configure_independent(fixture: &mut CliFixture, binary: std::path::PathBuf) {
    fixture.binary = binary;
    let config = fixture.temporary.path().join("config/rsi");
    let preset = config.join("agent-presets/workbench");
    std::fs::create_dir_all(&preset).unwrap();
    let profile = fixture.assert_success(&["fixture-profile"]);
    std::fs::write(preset.join("agent.profile.toml"), profile.stdout).unwrap();
    let settings_path = config.join("settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    settings["rsi.agent-presets"] = serde_json::json!({"default":"workbench"});
    std::fs::write(settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
}
fn independent_headless(fixture: &CliFixture) {
    let output = fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--commands",
        "--session-id",
        "addon-headless",
        "--output",
        "jsonl",
    ]);
    let list = super::commands::result(&output);
    let descriptor = list["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "plan")
        .unwrap();
    let command = serde_json::json!({"command":descriptor["id"],"request_id":"headless-plan-on","expected_revision":list["revision"],"arguments":"on"});
    let output = fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--command",
        &command.to_string(),
        "inspect addon",
        "--session-id",
        "addon-headless",
        "--output",
        "jsonl",
    ]);
    assert_eq!(
        super::commands::result(&output)["outcome"]["kind"],
        "draft_changed"
    );
    let events: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if let Some(report) = std::env::var_os("RSI_TUI_PTY_REPORT") {
        std::fs::write(
            std::path::PathBuf::from(report).join("independent-headless.jsonl"),
            &output.stdout,
        )
        .unwrap();
    }
    assert!(events.iter().any(|event| event["type"] == "fact"
        && event.to_string().contains("Plan mode is enabled")
        && event.to_string().contains("rsi.plan-policy.context")));
}

async fn inspect_plan_projection(terminal: &mut TerminalClient, enabled: bool, cursor: &str) {
    terminal.send(b"\x10");
    terminal.select_menu("Extension state").await;
    terminal.until("rsi.plan-policy.view").await;
    terminal.select_menu("rsi.plan-policy.view").await;
    terminal.until(&format!("\"enabled\": {enabled}")).await;
    terminal.until(cursor).await;
    terminal.send(b"\x1b");
    let deadline = Instant::now() + Duration::from_secs(5);
    while terminal
        .screen
        .lock()
        .unwrap()
        .screen()
        .contents()
        .contains("rsi.plan-policy.view")
    {
        assert!(Instant::now() < deadline, "extension detail did not close");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

impl TerminalClient {
    fn write_profile(fixture: &CliFixture, native: bool) {
        let directory = fixture
            .temporary
            .path()
            .join("config/rsi/application-profiles/test-tui");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("application.profile.toml"),
            r#"format = 1
[[steps]]
kind = "plugin"
id = "connection"
plugin = "rsi.application.connection"
config = { host_profile = "fixture" }
[[steps]]
kind = "plugin"
id = "ui"
plugin = "rsi.ui"
[[steps]]
kind = "plugin"
id = "ui-target"
plugin = "rsi.ui.target"
config = "application"
[[steps]]
kind = "plugin"
id = "setup"
plugin = "rsi.workbench.setup"
[[steps]]
kind = "plugin"
id = "plugins"
plugin = "rsi.workbench.plugins"
[[steps]]
kind = "plugin"
id = "session-ui"
plugin = "rsi.session.ui"
[[steps]]
kind = "plugin"
id = "tree-ui"
plugin = "rsi.session.tree.ui"
[[steps]]
kind = "plugin"
id = "files-ui"
plugin = "rsi.session.files.ui"
[[steps]]
kind = "plugin"
id = "service-ui"
plugin = "rsi.service.ui.client"
[[steps]]
kind = "plugin"
id = "workspace-review-ui"
plugin = "rsi.workspace.review.ui"
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.tui"
"#,
        )
        .unwrap();
        if native {
            let mut profile = std::fs::OpenOptions::new()
                .append(true)
                .open(directory.join("application.profile.toml"))
                .unwrap();
            profile.write_all(b"\n[steps.config]\npresentation = [{id='native',plugin='rsi.terminal.native'}, {id='adapter',plugin='rsi.terminal.portable'}]\n").unwrap();
        }
    }
    fn start(fixture: &CliFixture, arguments: &[&str]) -> Self {
        Self::start_presentation(fixture, arguments, false)
    }
    fn start_presentation(fixture: &CliFixture, arguments: &[&str], native: bool) -> Self {
        Self::write_profile(fixture, native);
        Self::launch(fixture, arguments, &["--profile", "test-tui"])
    }
    fn launch(fixture: &CliFixture, arguments: &[&str], entry: &[&str]) -> Self {
        Self::launch_environment(fixture, arguments, entry, &[])
    }
    fn launch_environment(
        fixture: &CliFixture,
        arguments: &[&str],
        entry: &[&str],
        environment: &[(&str, Option<&str>)],
    ) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(&fixture.binary);
        command.args(entry);
        command.args(arguments);
        command.cwd(&fixture.workspace);
        for (name, path) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            command.env(name, fixture.temporary.path().join(path));
        }
        command.env_remove("RSI_DEEPSEEK_API_KEY");
        command.env_remove("RSI_OPENAI_API_KEY");
        command.env_remove("DEEPSEEK_API_KEY");
        command.env_remove("OPENAI_API_KEY");
        command.env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
        for (name, value) in environment {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        command.env("TERM", "xterm-256color");
        command.env_remove("NO_COLOR");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let writer = pair.master.take_writer().unwrap();
        let mut source = pair.master.try_clone_reader().unwrap();
        let output = Arc::new(std::sync::Mutex::new(RawCapture::default()));
        let captured = output.clone();
        let screen = Arc::new(std::sync::Mutex::new(vt100::Parser::new(30, 110, 0)));
        let rendered = screen.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match source.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        captured.lock().unwrap().push(&bytes[..count]);
                        rendered.lock().unwrap().process(&bytes[..count]);
                    }
                }
            }
        });
        Self {
            master: pair.master,
            writer,
            child,
            output,
            reader: Some(reader),
            screen,
            capture_name: arguments.last().unwrap_or(&"tui").replace('/', "_"),
            capture_count: 0,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn action(&mut self, index: usize) {
        let mut keys = vec![0x10];
        keys.extend(b"\x1b[B".repeat(index));
        keys.push(b'\r');
        self.send(&keys);
    }

    async fn select_menu(&mut self, label: &str) {
        self.until("Enter select").await;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut previous = None;
        let mut clicked = false;
        let matches = |text: &str| {
            text == label
                || text.starts_with(&format!("{label} ·"))
                || text.starts_with(&format!("{label} ("))
        };
        loop {
            let (screen, selected, target) = {
                let parser = self.screen.lock().unwrap();
                let screen = parser.screen();
                let (rows, cols) = screen.size();
                let selected = (0..rows).find_map(|row| {
                    let text = (0..cols)
                        .filter_map(|col| screen.cell(row, col))
                        .filter(|cell| cell.bgcolor() == vt100::Color::Idx(237))
                        .map(vt100::Cell::contents)
                        .collect::<String>();
                    text.strip_prefix("› ").map(|label| label.trim().to_owned())
                });
                let target = (0..rows).find_map(|row| {
                    let cells = (0..cols)
                        .filter_map(|col| screen.cell(row, col).map(|cell| (col, cell)))
                        .filter(|(_, cell)| matches!(cell.bgcolor(), vt100::Color::Idx(235 | 237)))
                        .collect::<Vec<_>>();
                    let text = cells
                        .iter()
                        .map(|(_, cell)| cell.contents())
                        .collect::<String>();
                    let text = text
                        .strip_prefix("› ")
                        .or_else(|| text.strip_prefix("  "))?;
                    matches(text.trim()).then(|| (cells[0].0, row))
                });
                (screen.contents(), selected, target)
            };
            if selected.as_deref().is_some_and(&matches) {
                self.send(b"\r");
                self.absent(&format!("› {label}")).await;
                return;
            }
            if Instant::now() >= deadline {
                self.capture();
            }
            assert!(
                Instant::now() < deadline,
                "menu never selected {label}: {screen}"
            );
            if let Some((x, y)) = target {
                if !clicked {
                    self.send(
                        format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1)
                            .as_bytes(),
                    );
                    clicked = true;
                }
            } else if selected.is_some() && selected != previous {
                previous = selected;
                self.send(b"\x1b[B");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn until_ansi(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.output.lock().unwrap().contains(text.as_bytes()) {
                return;
            }
            assert!(Instant::now() < deadline, "PTY never emitted {text:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn until(&mut self, text: &str) {
        self.until_screen(text, |screen| screen.contains(text))
            .await;
    }

    async fn until_screen(&mut self, text: &str, matches: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let output = self.screen.lock().unwrap().screen().contents();
            if matches(&output) {
                self.capture();
                return;
            }
            if Instant::now() >= deadline {
                self.capture();
            }
            assert!(
                Instant::now() < deadline,
                "terminal never produced {text:?}: {}",
                output
                    .chars()
                    .rev()
                    .take(2048)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            );
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "terminal exited before {text}: {output}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn until_activity(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut frames = std::collections::BTreeSet::new();
        loop {
            {
                let parser = self.screen.lock().unwrap();
                let screen = parser.screen();
                let (rows, cols) = screen.size();
                let footer = screen.rows(0, cols).nth(usize::from(rows - 2)).unwrap();
                for ch in footer.chars().filter(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧".contains(*ch)) {
                    frames.insert(ch);
                }
                if frames.len() >= 2
                    && footer.split_whitespace().any(|part| {
                        part.strip_suffix('s')
                            .is_some_and(|n| n.parse::<u64>().is_ok())
                    })
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "active turn must animate and show elapsed time"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.capture();
    }

    async fn until_cell(&mut self, row: u16, column: u16, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if self
                .screen
                .lock()
                .unwrap()
                .screen()
                .cell(row, column)
                .is_some_and(|cell| cell.contents() == text)
            {
                self.capture();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "PTY did not display {text:?} at {row},{column}"
            );
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "terminal exited before expected cell"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn absent(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !self
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains(text)
            {
                self.capture();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "terminal still displays {text:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn resize(&self, size: PtySize) {
        let mut parser = self.screen.lock().unwrap();
        parser.screen_mut().set_size(size.rows, size.cols);
        self.master.resize(size).unwrap();
    }

    fn capture(&mut self) {
        let Ok(directory) = std::env::var("RSI_TUI_PTY_REPORT") else {
            return;
        };
        std::fs::create_dir_all(&directory).unwrap();
        self.capture_count += 1;
        let base = std::path::Path::new(&directory)
            .join(format!("{}-{:02}", self.capture_name, self.capture_count));
        let parser = self.screen.lock().unwrap();
        let screen = parser.screen();
        let (height, width) = screen.size();
        let cells: Vec<_> = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).map(|(x, y)| {
            let cell = screen.cell(y, x).unwrap();
            serde_json::json!({"x": x, "y": y, "text": cell.contents(), "fg": format!("{:?}", cell.fgcolor()), "bg": format!("{:?}", cell.bgcolor()), "bold": cell.bold(), "dim": cell.dim()})
        }).collect();
        std::fs::write(
            base.with_extension("json"),
            serde_json::to_vec(
                &serde_json::json!({"width": width, "height": height, "cells": cells}),
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::write(base.with_extension("txt"), screen.contents()).unwrap();
        drop(parser);
        let output = self.output.lock().unwrap();
        std::fs::write(base.with_extension("ansi"), &output.prefix).unwrap();
        std::fs::write(
            base.with_extension("tail.ansi"),
            output.tail.iter().copied().collect::<Vec<_>>(),
        )
        .unwrap();
        std::fs::write(base.with_extension("capture.json"), serde_json::to_vec(&serde_json::json!({
            "prefix_bytes": output.prefix.len(), "tail_bytes": output.tail.len(), "discarded_bytes": output.discarded,
        })).unwrap()).unwrap();
    }

    async fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "terminal did not exit");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        let output = self.output.lock().unwrap();
        assert!(
            output.contains(b"\x1b[?1049l"),
            "alternate screen was not restored"
        );
        let termios = self.master.get_termios().expect("PTY termios");
        assert!(
            format!("{:?}", termios.local_flags).contains("ICANON"),
            "raw mode was not restored"
        );
    }
}

impl Drop for TerminalClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_paste_submit_resize_model_menu_and_terminal_restore() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-paste"]);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"\x1b[200~Repair UTF-8\nsecond line\x1b[201~");
    terminal.until("Repair UTF-8").await;
    terminal.send(b"\x1a");
    terminal.absent("Repair UTF-8").await;
    terminal.send(b"\x1bz");
    terminal.until("Repair UTF-8").await;
    terminal.send(b"\r");
    terminal.until("hello from daemon").await;
    terminal.until("2 in / 3 out").await;
    terminal.send(b"\x12");
    terminal.until("Submitted input history").await;
    terminal.send(b"\r");
    terminal.absent("Submitted input history").await;
    terminal.send(b"\x1a");
    terminal.send(b"\x10");
    terminal.select_menu("Model and effort").await;
    terminal.until("fixture/fixture-model").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(100)).await;
    terminal.resize(PtySize {
        rows: 12,
        cols: 42,
        pixel_width: 0,
        pixel_height: 0,
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    {
        let output = terminal.output.lock().unwrap();
        let bytes = output.complete();
        let output = String::from_utf8_lossy(&bytes);
        let frame = output.rsplit("\x1b[2J").next().unwrap();
        assert!(
            terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("fixture-model"),
            "resized footer must retain the selected model"
        );
        assert!(
            !frame.contains("\x1b[13;1H"),
            "resized frame must not write below row 12"
        );
    }
    terminal.capture();
    terminal.send(b"\x04");
    terminal.finish().await;
    let history = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--history",
        "tui-paste",
        "--output",
        "jsonl",
    ]);
    let records: Vec<serde_json::Value> = String::from_utf8(history.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let accepted = records
        .iter()
        .filter(|record| record.to_string().contains("message_turn_accepted"))
        .count();
    assert_eq!(
        accepted, 1,
        "paste then Enter submitted more than once: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record.to_string().contains("Repair UTF-8\\nsecond line"))
    );
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_rejects_non_tty_before_host_boot_and_restores_after_sigterm() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &[]);
    terminal.until("Ctrl+J adds a line").await;
    let pid = terminal.child.process_id().unwrap();
    let process = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::kill_process(process, rustix::process::Signal::TERM).unwrap();
    terminal.finish().await;
    let output = fixture
        .command()
        .args(["--profile", "test-tui"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("terminal stdin and stdout"));
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_answers_live_questions_through_the_real_tool_and_provider_loop() {
    let (endpoint, state, provider) = gated_provider("ask_user").await;
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-question"]);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Ask me a question\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("1 questions").await;
    terminal.send(b"/attention\r");
    terminal.select_menu("Answer question").await;
    terminal.until("Live question").await;
    terminal.send(b"2\rbecause verified\r");
    terminal.until("hello from daemon").await;
    terminal.capture();
    terminal.send(b"\x04");
    terminal.finish().await;
    let requests = state.requests.lock().unwrap();
    assert!(requests.len() >= 2);
    assert!(requests[1].to_string().contains("because verified"));
    assert!(requests[1].to_string().contains("blue"));
    drop(requests);
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fullscreen_reviews_and_denies_a_live_prepared_approval() {
    let (endpoint, state, provider) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    fixture.require_approval();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-approval"]);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Run a command requiring approval\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("1 approvals").await;
    terminal.send(b"/attention\r");
    terminal.select_menu("Review permission").await;
    terminal.until("Review the prepared request").await;
    terminal.send(b"\r");
    terminal.until("Allow once").await;
    terminal.send(b"\r"); // Deny is the initial focused action.
    terminal.until("failed").await;
    terminal.absent("1 approvals").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
    terminal.capture();
    terminal.send(b"\x04");
    terminal.finish().await;
    assert_eq!(state.requests.lock().unwrap().len(), 1);
    let history = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--history",
        "tui-approval",
        "--output",
        "jsonl",
    ]);
    let history = String::from_utf8(history.stdout).unwrap();
    assert!(history.contains("approval.denied"));
    assert!(!history.contains("\"type\":\"tool_started\""));
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_detach_resume_reads_foreign_pending_body_and_ctrl_c_preserves_draft() {
    let (endpoint, state, provider) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let mut first = TerminalClient::start(&fixture, &["--session-id", "tui-resume"]);
    first.until("Ctrl+J adds a line").await;
    first.send(b"first task\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    first.send(b"foreign queued body\r");
    tokio::time::sleep(Duration::from_millis(250)).await;
    first.send(b"\x10");
    first.select_menu("Pending inputs").await;
    first.until("Pending inputs").await;
    first.send(b"\r");
    first.until("foreign queued body").await;
    first.send(b"\x04");
    first.finish().await;
    let mut resumed = TerminalClient::start(&fixture, &["--resume", "tui-resume"]);
    resumed.until("fixture-model").await;
    resumed.send(b"\x10");
    resumed.select_menu("Pending inputs").await;
    resumed.until("Pending inputs").await;
    resumed.send(b"\r");
    resumed.until("foreign queued body").await;
    resumed.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
    resumed.send(b"preserved local draft\x03");
    resumed.until("Cancellation").await;
    resumed.until("preserved local draft").await;
    // Cancel targets this attachment's active Turn, preserving the other client's input.
    state.release.notify_one();
    resumed.until("hello from daemon").await;
    resumed.send(b"\x15\x04");
    resumed.finish().await;
    let history = fixture.assert_success(&[
        "--profile",
        "test-cli",
        "--history",
        "tui-resume",
        "--output",
        "jsonl",
    ]);
    let history = String::from_utf8(history.stdout).unwrap();
    assert!(history.contains("foreign queued body"));
    assert!(history.contains("cancelled"));
    assert!(state.requests.lock().unwrap().len() >= 2);
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_presentation_reloads_in_the_running_tui_without_losing_draft_or_pending_turn() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let mut artifacts = Vec::new();
    for revision in ["a", "b"] {
        let target = root.join("target/terminal-native-test").join(revision);
        let mut build = Command::new(env!("CARGO"));
        build
            .args(["build", "--locked", "--manifest-path"])
            .arg(root.join("crates/rsi/terminal-native/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target);
        if revision == "b" {
            build.args(["--features", "revision-b"]);
        }
        let result = build.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        artifacts.push(target.join("debug").join(format!(
            "{}rsi_terminal_native{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        )));
    }
    let (endpoint, state, provider) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    let source = fixture.temporary.path().join("native-source");
    std::fs::create_dir(&source).unwrap();
    let manifest = source.join("native.toml");
    std::fs::write(&manifest,format!("format=2\nscope='application'\nid='dev.terminal'\nplugin='rsi.terminal.native'\ntarget='{}'\nartifact='artifact.bin'\n",rsi::native_addon_target())).unwrap();
    std::fs::copy(&artifacts[0], source.join("artifact.bin")).unwrap();
    let store =
        rsi::NativeAddonStore::open(fixture.temporary.path().join("config/rsi/native-addons"))
            .unwrap();
    let old = store.install(&manifest).unwrap().record.unwrap();
    store.enable_exact(&old, None).unwrap();
    let settings_path = fixture.temporary.path().join("config/rsi/settings.json");
    let saved_settings = std::fs::read(&settings_path).unwrap();
    std::fs::remove_file(&settings_path).unwrap();
    let mut home = TerminalClient::start_presentation(&fixture, &[], true);
    home.until("No session attached").await;
    home.send(b"native unattached draft\r");
    home.until("Draft retained; nothing was sent").await;
    home.send(b"\x03");
    home.finish().await;
    std::fs::write(settings_path, saved_settings).unwrap();
    let mut terminal =
        TerminalClient::start_presentation(&fixture, &["--session-id", "native-hot-reload"], true);
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"/markdown off\r");
    terminal.until("Markdown rendering off").await;
    terminal.send(b"hold this turn\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    terminal.until_activity().await;
    terminal.send(b"unsubmitted-draft-kept");
    terminal.until("unsubmitted-draft-kept").await;
    std::fs::copy(&artifacts[1], source.join("artifact.bin")).unwrap();
    let next = store.install(&manifest).unwrap().record.unwrap();
    store.enable_exact(&next, Some(&old)).unwrap();
    terminal.until_cell(0, 0, "B").await;
    terminal.until("unsubmitted-draft-kept").await;
    terminal.until_activity().await;
    assert_eq!(
        state.requests.lock().unwrap().len(),
        1,
        "reload must not replay a model request"
    );
    let output = terminal.output.lock().unwrap().complete();
    assert_eq!(
        output
            .windows(b"\x1b[?1049h".len())
            .filter(|bytes| *bytes == b"\x1b[?1049h")
            .count(),
        1,
        "terminal owner must not restart"
    );
    terminal.send(b"\x15/markdown\r");
    terminal.until("Markdown rendering on").await;
    terminal.send(b"unsubmitted-draft-kept");
    state.release.notify_one();
    terminal.until("hello from daemon").await;
    terminal.until("unsubmitted-draft-kept").await;
    terminal.send(b"\x10");
    terminal.select_menu("Exit").await;
    terminal.finish().await;
    provider.abort();
}
