use super::*;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::time::{Duration, Instant};

struct TerminalClient {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    output: Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl TerminalClient {
    fn start(fixture: &CliFixture, arguments: &[&str]) -> Self {
        let directory = fixture
            .temporary
            .path()
            .join("config/rsi/application-profiles/test-tui");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("application.profile.toml"),
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"connection\"\nplugin = \"rsi.application.connection\"\nconfig = { host_profile = \"fixture\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.tui\"\n",
        )
        .unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_rsi"));
        command.args(["--profile", "test-tui"]);
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
        command.env("RSI_OPENAI_COMPATIBLE_API_KEY", "fixture-secret");
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let writer = pair.master.take_writer().unwrap();
        let mut source = pair.master.try_clone_reader().unwrap();
        let output = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = output.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match source.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let mut output = captured.lock().unwrap();
                        if output.len() + count <= 16 * 1024 * 1024 {
                            output.extend_from_slice(&bytes[..count]);
                        }
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
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    async fn until(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let output = String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned();
            if output.contains(text) {
                return;
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
                "terminal exited before {text}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
            output
                .windows(b"\x1b[?1049l".len())
                .any(|bytes| bytes == b"\x1b[?1049l"),
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
    terminal.until("Describe a change").await;
    terminal.send(b"\x1b[200~Repair UTF-8\nsecond line\x1b[201~\r");
    terminal.until("hello from daemon").await;
    terminal.send(b"\x10\x1b[B\x1b[B\r");
    terminal.until("fixture/fixture-model").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(100)).await;
    terminal
        .master
        .resize(PtySize {
            rows: 12,
            cols: 42,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    {
        let output = terminal.output.lock().unwrap();
        let output = String::from_utf8_lossy(&output);
        let frame = output.rsplit("\x1b[2J").next().unwrap();
        assert!(
            frame.contains("exit interrupts"),
            "resized header must remain on screen"
        );
        assert!(
            !frame.contains("\x1b[13;1H"),
            "resized frame must not write below row 12"
        );
    }
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
    terminal.until("Describe a change").await;
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
    terminal.until("Describe a change").await;
    terminal.send(b"Ask me a question\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("1 questions").await;
    terminal.send(b"\x10\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B\r");
    terminal.until("Live questions").await;
    terminal.send(b"\r");
    terminal.until("Live question").await;
    terminal.send(b"2\rbecause verified\r");
    terminal.until("hello from daemon").await;
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
    terminal.until("Describe a change").await;
    terminal.send(b"Run a command requiring approval\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("1 approvals").await;
    terminal.send(b"\x10\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B\r");
    terminal.until("Live approvals").await;
    terminal.send(b"\r");
    terminal.until("Review the prepared request").await;
    terminal.send(b"\r");
    terminal.until("Allow once").await;
    terminal.send(b"\r"); // Deny is the initial focused action.
    terminal.until("failed").await;
    terminal.until("Approval response accepted: true").await;
    terminal.send(b"\x1b");
    tokio::time::sleep(Duration::from_millis(80)).await;
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
    first.until("exit detaches").await;
    first.send(b"first task\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    first.send(b"foreign queued body\r");
    tokio::time::sleep(Duration::from_millis(250)).await;
    first.send(b"\x10\x1b[B\x1b[B\x1b[B\r");
    first.until("Pending inputs").await;
    first.send(b"\r");
    first.until("foreign queued body").await;
    first.send(b"\x04");
    first.finish().await;
    let mut resumed = TerminalClient::start(&fixture, &["--resume", "tui-resume"]);
    resumed.until("exit detaches").await;
    resumed.send(b"\x10\x1b[B\x1b[B\x1b[B\r");
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
