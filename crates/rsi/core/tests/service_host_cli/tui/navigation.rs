use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_command_expands_past_the_summary_and_bottom_scroll_is_stable() {
    let (endpoint, state, provider) = gated_provider("bash").await;
    *state.arguments.lock().unwrap() = Some(serde_json::json!({
        "command": format!("printf 'command result\\n'\n# {}\n# COMMAND-END", "long argument ".repeat(60))
    }));
    let fixture = CliFixture::new(&endpoint);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "long-command"]);
    terminal.capture_name = "long-command".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Run the long command\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("hello from daemon").await;
    let row = {
        let parser = terminal.screen.lock().unwrap();
        let screen = parser.screen();
        assert!(!screen.contents().contains("COMMAND-END"));
        screen
            .rows(0, 110)
            .position(|row| row.contains("Ran printf"))
            .unwrap()
            + 1
    };
    terminal.send(format!("\x1b[<0;3;{row}M\x1b[<0;3;{row}m").as_bytes());
    terminal.until("COMMAND-END").await;
    {
        let parser = terminal.screen.lock().unwrap();
        let rows = parser.screen().rows(0, 110).collect::<Vec<_>>();
        let end = rows
            .iter()
            .position(|row| row.contains("COMMAND-END"))
            .unwrap();
        assert!(
            !rows[end].contains("command result"),
            "command and result must occupy separate rows"
        );
        assert!(rows[end + 1].starts_with("command result"));
    }
    terminal.resize(PtySize {
        cols: 42,
        rows: 12,
        pixel_width: 0,
        pixel_height: 0,
    });
    scroll_to(&mut terminal, "COMMAND-END", b"\x1b[6~").await;
    terminal.send(b"\x1b[F");
    terminal.until("hello from daemon").await;
    let tail = terminal.screen.lock().unwrap().screen().contents();
    for _ in 0..8 {
        terminal.send(b"\x1b[<65;3;3M\x1b[6~");
    }
    // The queued editor edit is an acknowledgement barrier after all scroll events.
    terminal.send(b"scroll-ack");
    terminal.until("scroll-ack").await;
    let scrolled = terminal.screen.lock().unwrap().screen().contents();
    assert_eq!(
        tail.lines().take(7).collect::<Vec<_>>(),
        scrolled.lines().take(7).collect::<Vec<_>>()
    );
    terminal.send(b"\x1b[5~");
    terminal.absent("hello from daemon").await;
    terminal.send(b"\x15\x04");
    terminal.finish().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_tool_uses_an_action_summary_and_reveals_its_result() {
    let (endpoint, state, provider) = gated_provider("directory_list").await;
    *state.arguments.lock().unwrap() = Some(serde_json::json!({"path":"."}));
    let fixture = CliFixture::new(&endpoint);
    std::fs::write(fixture.workspace.join("sample.txt"), "fixture").unwrap();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "list-tool"]);
    terminal.capture_name = "directory-summary".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"List this directory\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("hello from daemon").await;
    terminal.until("Listed .").await;
    let y = {
        let parser = terminal.screen.lock().unwrap();
        let screen = parser.screen();
        assert!(!screen.contents().contains("directory_list ·"));
        assert!(!screen.contents().contains("sample.txt"));
        let row = u16::try_from(
            screen
                .rows(0, 110)
                .position(|row| row.contains("Listed ."))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            screen.cell(row, 0).unwrap().fgcolor(),
            vt100::Color::Idx(10)
        );
        row + 1
    };
    terminal.send(format!("\x1b[<0;3;{y}M\x1b[<0;3;{y}m").as_bytes());
    terminal.until("sample.txt").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expanded_tool_output_scrolls_with_wheel_and_page_keys() {
    for remote in [false, true] {
        let (endpoint, state, provider) = gated_provider("bash").await;
        *state.arguments.lock().unwrap() = Some(serde_json::json!({
            "command": "for i in $(seq 1 80); do printf 'row-%03d\\n' \"$i\"; done"
        }));
        let fixture = CliFixture::new(&endpoint);
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let mut terminal = TerminalClient::start(&fixture, &["--session-id", "scroll-tool"]);
        terminal.capture_name = format!(
            "expanded-scroll-{}",
            if remote { "remote" } else { "local" }
        );
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"/model-");
        terminal.until("/model-").await;
        assert!(
            !terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("/model-selection")
        );
        terminal.send(b"selection\r");
        terminal.until("Use /model or /effort").await;
        {
            let parser = terminal.screen.lock().unwrap();
            let screen = parser.screen();
            let row = u16::try_from(
                screen
                    .rows(0, 110)
                    .position(|row| row.contains("Unknown command"))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(screen.cell(row, 2).unwrap().fgcolor(), vt100::Color::Idx(9));
        }
        assert!(state.requests.lock().unwrap().is_empty());
        terminal.send(b"\x15Print numbered rows\r");
        tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
            .await
            .unwrap();
        state.release.notify_one();
        terminal.until("hello from daemon").await;
        terminal.until("Ran for i").await;
        let y = {
            let parser = terminal.screen.lock().unwrap();
            let screen = parser.screen();
            assert!(!screen.contents().contains("bash · completed"));
            screen
                .rows(0, 110)
                .position(|row| row.contains("Ran for i"))
                .unwrap()
                + 1
        };
        terminal.send(format!("\x1b[<0;3;{y}M\x1b[<0;3;{y}m").as_bytes());
        terminal.until("row-001").await;
        assert_eq!(
            terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .rows(0, 110)
                .position(|row| row.contains("Ran for i"))
                .unwrap()
                + 1,
            y
        );
        scroll_to(&mut terminal, "row-080", b"\x1b[<65;3;10M").await;
        scroll_to(&mut terminal, "row-001", b"\x1b[5~").await;
        terminal.resize(PtySize {
            cols: 42,
            rows: 12,
            pixel_width: 0,
            pixel_height: 0,
        });
        scroll_to(&mut terminal, "row-080", b"\x1b[6~").await;
        scroll_to(&mut terminal, "row-001", b"\x1b[<64;3;3M").await;
        terminal.send(b"\x1b[F");
        terminal.until("hello from daemon").await;
        terminal.send(b"\x04");
        terminal.finish().await;
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}

async fn scroll_to(terminal: &mut TerminalClient, needle: &str, input: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains(needle)
        {
            terminal.capture();
            return;
        }
        assert!(Instant::now() < deadline, "scroll never reached {needle}");
        terminal.send(input);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One PTY journey checks geometry, selection and actual requests in both Host modes.
async fn footer_selection_shows_declared_default_and_explicit_effort_without_notices() {
    for remote in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(super::super::commands::capture),
            )
            .with_state(requests.clone());
        let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut fixture = CliFixture::new(&endpoint);
        fixture.workspace = fixture.temporary.path().join("home/Projects/workspace");
        std::fs::create_dir_all(&fixture.workspace).unwrap();
        let path = fixture
            .temporary
            .path()
            .join("config/rsi/host-profiles/fixture/host.profile.toml");
        let mut profile = std::fs::read_to_string(&path).unwrap();
        profile.push_str("\n[steps.config.language_models.effort-model]\ncontext_window_tokens=128000\ndefault_output_reserve_tokens=4096\nmax_output_reserve_tokens=16384\n[steps.config.reasoning_efforts.effort-model]\nsupported=[\"low\",\"high\"]\ndefault=\"high\"\n");
        std::fs::write(path, profile).unwrap();
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let mut terminal = TerminalClient::start(&fixture, &["--session-id", "footer-cleanup"]);
        terminal.capture_name = format!("footer-{}", if remote { "remote" } else { "local" });
        terminal.until("fixture-model · default").await;
        terminal.until("~/Projects/workspace").await;
        terminal.send(b"/effort\r");
        terminal.select_menu("Provider default").await;
        terminal.until("fixture-model · default").await;
        for (cols, rows) in [(110, 35), (80, 24), (42, 12), (28, 9)] {
            terminal.resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            });
            terminal.send(format!("\x15size-{cols}-{rows}").as_bytes());
            terminal.until(&format!("size-{cols}-{rows}")).await;
            let screen = terminal.screen.lock().unwrap();
            let screen = screen.screen();
            assert!(!screen.contents().contains("OSC52"));
            let lines = screen.rows(0, cols).collect::<Vec<_>>();
            assert!(!lines[usize::from(rows - 1)].contains("Enter submits"));
            assert!(lines[usize::from(rows - 1)].contains("/help"));
            assert!(lines[usize::from(rows - 2)].starts_with("fixture-model · default"));
        }
        terminal.resize(PtySize {
            cols: 110,
            rows: 35,
            pixel_width: 0,
            pixel_height: 0,
        });
        terminal.send(b"\x15\x19/model\r");
        terminal.select_menu("fixture/effort-model").await;
        terminal.select_menu("Provider default (high)").await;
        terminal.until("effort-model · high").await;
        assert!(
            requests.lock().unwrap().is_empty(),
            "selection must not send a model request"
        );
        terminal.send(b"default effort request\r");
        terminal.until("hello from daemon").await;
        terminal.send(b"/eff\t");
        terminal.until("/effort").await;
        terminal.send(b"\r");
        terminal.until("Reasoning effort").await;
        terminal.select_menu("low").await;
        terminal.until("effort-model · low").await;
        terminal.send(b"explicit effort request\r");
        let deadline = Instant::now() + Duration::from_secs(20);
        while terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .matches("2 in / 3 out")
            .count()
            < 2
        {
            assert!(Instant::now() < deadline, "second request did not complete");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        terminal.capture();
        terminal.send(b"\x04");
        terminal.finish().await;
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["reasoning_effort"], "high");
        assert_eq!(requests[1]["reasoning_effort"], "low");
        let output = terminal.output.lock().unwrap();
        let bytes = output.complete();
        let output = String::from_utf8_lossy(&bytes);
        for removed in [
            "Describe a change",
            "Model and effort selected",
            "Select transcript text first",
            "Model selected.",
            "Setup closed.",
            "Beginning of retained history",
        ] {
            assert!(!output.contains(removed), "unexpected notice: {removed}");
        }
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}

async fn transcript_response(
    State(requests): State<Arc<std::sync::Mutex<Vec<serde_json::Value>>>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> Response {
    let number = {
        let mut requests = requests.lock().unwrap();
        requests.push(request);
        requests.len()
    };
    if number == 1 {
        return chat().await;
    }
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Reasoning first row\\nReasoning second row\\nReasoning third row\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Second answer body\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":5}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Actual mouse events, color cells, metadata order and idle timer behavior in both Host modes.
async fn transcript_metadata_and_thinking_mouse_clicks_have_source_grounded_display() {
    for remote in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(transcript_response))
            .with_state(requests.clone());
        let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let fixture = CliFixture::new(&endpoint);
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let mut terminal =
            TerminalClient::start(&fixture, &["--session-id", "transcript-refinement"]);
        terminal.capture_name = format!("transcript-{}", if remote { "remote" } else { "local" });
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"First question\r");
        terminal.until("2 in / 3 out").await;
        assert!(
            !terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("Thinking"),
            "no placeholder for absent provider reasoning"
        );
        terminal.send(b"Second question\r");
        terminal.until("4 in / 5 out").await;
        terminal.absent("Reasoning second row").await;
        let thinking = {
            let parser = terminal.screen.lock().unwrap();
            let screen = parser.screen();
            let rows = screen.rows(0, 110).collect::<Vec<_>>();
            let row = |needle: &str| rows.iter().position(|text| text.contains(needle)).unwrap();
            assert!(row("First question") < row("hello from daemon"));
            assert!(row("hello from daemon") < row("2 in / 3 out"));
            assert!(row("2 in / 3 out") < row("Second question"));
            assert_eq!(row("Thinking") + 2, row("Second answer body"));
            assert!(rows[row("Thinking") + 1].trim().is_empty());
            let thinking = u16::try_from(row("Thinking")).unwrap();
            assert_eq!(screen.cell(thinking, 0).unwrap().contents(), "▸");
            assert_eq!(
                screen.cell(thinking, 0).unwrap().fgcolor(),
                vt100::Color::Idx(10)
            );
            assert_eq!(
                screen.cell(thinking, 2).unwrap().fgcolor(),
                vt100::Color::Idx(8)
            );
            assert!(row("Second answer body") < row("4 in / 5 out"));
            assert_eq!(screen.cell(0, 0).unwrap().contents(), "▄");
            assert_eq!(screen.cell(0, 0).unwrap().fgcolor(), vt100::Color::Idx(235));
            assert_eq!(screen.cell(1, 0).unwrap().bgcolor(), vt100::Color::Idx(235));
            let (height, _) = screen.size();
            assert_eq!(screen.cell(height - 4, 0).unwrap().contents(), "›");
            assert_eq!(screen.cell(height - 4, 2).unwrap().contents(), "▏");
            assert_eq!(screen.cell(1, 0).unwrap().contents(), "›");
            for message in ["First question", "Second question"] {
                let timestamp = u16::try_from(row(message) + 1).unwrap();
                assert_eq!(screen.cell(timestamp, 0).unwrap().contents(), "•");
                assert_eq!(
                    screen.cell(timestamp, 0).unwrap().bgcolor(),
                    vt100::Color::Default
                );
                if message == "Second question" {
                    assert_eq!(usize::from(timestamp) + 2, row("Thinking"));
                }
                assert!(rows[usize::from(timestamp) + 1].trim().is_empty());
            }
            let metadata = u16::try_from(row("4 in / 5 out")).unwrap();
            assert_eq!(screen.cell(metadata, 0).unwrap().contents(), "•");
            assert_eq!(
                screen.cell(metadata, 0).unwrap().bgcolor(),
                vt100::Color::Default
            );
            assert_eq!(
                screen.cell(metadata, 109).unwrap().bgcolor(),
                vt100::Color::Default
            );
            u16::try_from(row("Thinking")).unwrap()
        };
        terminal.send(format!("\x1b[<0;3;{}M\x1b[<0;3;{}m", thinking + 1, thinking + 1).as_bytes());
        terminal.until("Reasoning second row").await;
        // Expansion must preserve the clicked summary row.
        let expanded_row = terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .rows(0, 110)
            .position(|row| row.contains("Thinking"))
            .unwrap()
            + 1;
        assert_eq!(expanded_row, usize::from(thinking) + 1);
        {
            let parser = terminal.screen.lock().unwrap();
            let rows = parser.screen().rows(0, 110).collect::<Vec<_>>();
            let thinking = u16::try_from(expanded_row - 1).unwrap();
            assert_eq!(parser.screen().cell(thinking, 0).unwrap().contents(), "▾");
            assert_eq!(
                parser.screen().cell(thinking, 0).unwrap().fgcolor(),
                vt100::Color::Idx(10)
            );
            let last = rows
                .iter()
                .position(|row| row.contains("Reasoning third row"))
                .unwrap();
            assert!(rows[last + 1].trim().is_empty());
            assert!(rows[last + 2].contains("Second answer body"));
        }
        let thinking = expanded_row;
        terminal.send(format!("\x1b[<0;3;{thinking}M\x1b[<0;3;{thinking}m").as_bytes());
        terminal.absent("Reasoning second row").await;
        terminal.send(b"\x1b[F");
        terminal.absent("Browsing history").await;
        // Observe two periodic inspections; no background read may appear as working feedback.
        let until = Instant::now() + Duration::from_millis(2200);
        while Instant::now() < until {
            let contents = terminal.screen.lock().unwrap().screen().contents();
            for absent in ["Working", "Loading", "Copied", "Copy failed"] {
                assert!(
                    !contents.contains(absent),
                    "unexpected idle feedback: {absent}"
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        terminal.send(b"\x1b[5~\x1b[5~");
        terminal.send(b"\x1b[F");
        terminal.absent("Browsing history").await;
        terminal.capture();
        terminal.send(b"\x04");
        terminal.finish().await;
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(
            !String::from_utf8_lossy(&terminal.output.lock().unwrap().complete())
                .contains("Beginning of retained history")
        );
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_reasoning_keeps_red_disclosure_markers_after_expand_and_resume() {
    for remote in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/v1/chat/completions", post(|| async {
            Response::builder().status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from(concat!(
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Partial reasoning\"},\"finish_reason\":null}]}\n\n",
                    "data: {invalid json}\n\n"
                ))).unwrap()
        }));
        let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let fixture = CliFixture::new(&endpoint);
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let check = |terminal: &TerminalClient, marker| {
            let parser = terminal.screen.lock().unwrap();
            let screen = parser.screen();
            let row = u16::try_from(
                screen
                    .rows(0, 110)
                    .position(|row| row.contains("Thinking"))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(screen.cell(row, 0).unwrap().contents(), marker);
            assert_eq!(screen.cell(row, 0).unwrap().fgcolor(), vt100::Color::Idx(9));
            assert_eq!(screen.cell(row, 2).unwrap().fgcolor(), vt100::Color::Idx(8));
            row
        };
        let mut terminal = TerminalClient::start(&fixture, &["--session-id", "failed-reasoning"]);
        terminal.capture_name = format!(
            "reasoning-failure-{}",
            if remote { "remote" } else { "local" }
        );
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"Trigger failed reasoning\r");
        terminal.until("Model error").await;
        let row = check(&terminal, "▸") + 1;
        terminal.send(format!("\x1b[<0;1;{row}M\x1b[<0;1;{row}m").as_bytes());
        terminal.until("Partial reasoning").await;
        assert_eq!(check(&terminal, "▾") + 1, row);
        terminal.send(b"\x04");
        terminal.finish().await;
        let mut resumed = TerminalClient::start(&fixture, &["--resume", "failed-reasoning"]);
        resumed.capture_name = format!(
            "reasoning-failure-resumed-{}",
            if remote { "remote" } else { "local" }
        );
        resumed.until("Model error").await;
        check(&resumed, "▸");
        resumed.send(b"\x04");
        resumed.finish().await;
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn child_attach_restores_parent_draft_and_explicit_send_resumes_only_child() {
    for remote in [false, true] {
        let (endpoint, state, provider) = gated_provider("spawn_agent").await;
        *state.arguments.lock().unwrap() = Some(
            serde_json::json!({"task_name":"inspect-child","message":"Child navigation evidence","fork_turns":"none"}),
        );
        let child_entered = Arc::new(Notify::new());
        let release_child = Arc::new(Notify::new());
        *state.request_gate.lock().unwrap() = Some(RequestGate {
            // The child has no forked history; parent requests retain this input.
            matches: |request| !request.to_string().contains("Create a child task"),
            entered: child_entered.clone(),
            release: release_child.clone(),
        });
        let fixture = CliFixture::new(&endpoint);
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let name = if remote {
            "navigate-remote"
        } else {
            "navigate-local"
        };
        let mut terminal = TerminalClient::start(&fixture, &["--session-id", name]);
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"Create a child task\r");
        tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
            .await
            .unwrap();
        state.release.notify_one();
        terminal.until("hello from daemon").await;
        tokio::time::timeout(Duration::from_secs(20), child_entered.notified())
            .await
            .unwrap();
        // Freeze the parent's terminal before the child replies. Otherwise completion
        // may enter its Tool follow-up Step, legitimately using three requests.
        parent_idle(&mut terminal, "parent terminal before child reply").await;
        release_child.notify_one();
        terminal.until("Completion from").await;
        parent_idle(&mut terminal, "parent completed the child notice").await;
        // Reading the tree's idle phase confirms both ordinary tool follow-ups settled.
        terminal.send(b"parent draft retained");
        terminal.until("parent draft retained").await;
        terminal.send(b"\x10");
        terminal.select_menu("Subagent sessions").await;
        terminal.until("1 · inspect-child · idle").await;
        assert_eq!(state.requests.lock().unwrap().len(), 4);
        terminal.select_menu("1 · inspect-child").await;
        terminal.until("Child navigation evidence").await;
        terminal.absent("parent draft retained").await;
        terminal.send(b"child draft retained");
        terminal.until("child draft retained").await;
        terminal.send(b"\x10");
        terminal.select_menu("Return to parent session").await;
        terminal.until("parent draft retained").await;
        terminal.absent("child draft retained").await;
        terminal.send(b"\x10");
        terminal.select_menu("Agent tree usage").await;
        terminal.until("2 / 2 sessions read").await;
        terminal.until("4 requests").await;
        assert_eq!(
            state.requests.lock().unwrap().len(),
            4,
            "browsing and metrics cannot execute a Session"
        );
        terminal.send(b"\x1b");
        terminal.absent("2 / 2 sessions read").await;
        terminal.send(b"\x10");
        terminal.select_menu("Subagent sessions").await;
        terminal.select_menu("1 · inspect-child").await;
        terminal.until("child draft retained").await;
        terminal.send(b"\r");
        let resumed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if state.requests.lock().unwrap().len() >= 5 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        terminal.capture();
        assert!(
            resumed.is_ok(),
            "child submission did not reach provider: {}",
            terminal.screen.lock().unwrap().screen().contents()
        );
        let request = state.requests.lock().unwrap()[4].to_string();
        assert!(request.contains("child draft retained"));
        assert!(!request.contains("parent draft retained"));
        terminal.until("2 in / 3 out").await;
        terminal.send(b"\x10");
        terminal.select_menu("Return to parent session").await;
        terminal.until("parent draft retained").await;
        terminal.send(b"\x15\x04");
        terminal.finish().await;
        assert!(
            (5..=6).contains(&state.requests.lock().unwrap().len()),
            "only the explicit child turn and its parent completion may execute"
        );
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}

async fn parent_idle(terminal: &mut TerminalClient, label: &str) {
    terminal
        .until_screen(label, |screen| {
            screen.lines().rev().nth(1).is_some_and(|footer| {
                footer.contains("fixture-model")
                    && !footer.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧".contains(ch))
            })
        })
        .await;
}
