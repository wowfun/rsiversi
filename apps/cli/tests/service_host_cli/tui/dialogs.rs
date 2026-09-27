use super::*;

fn caret(terminal: &TerminalClient) -> (u16, u16) {
    let parser = terminal.screen.lock().unwrap();
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let positions = (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (col, row)))
        .filter(|(col, row)| screen.cell(*row, *col).unwrap().contents() == "▏")
        .collect::<Vec<_>>();
    assert_eq!(
        positions.len(),
        1,
        "only the focused input may show a caret: {}",
        screen.contents()
    );
    positions[0]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One local/daemon journey verifies dialog focus, context, filtering and real resize.
async fn fullscreen_dialogs_keep_context_and_filter_geometry_without_background_input() {
    for remote in [false, true] {
        let (endpoint, state, provider) = gated_provider("bash").await;
        let fixture = CliFixture::new(&endpoint);
        let path = fixture
            .temporary
            .path()
            .join("config/rsi/host-profiles/fixture/host.profile.toml");
        let mut profile = std::fs::read_to_string(&path).unwrap();
        profile.push_str("\n[steps.config.reasoning_efforts.fixture-model]\nsupported=[\"low\",\"high\"]\ndefault=\"high\"\n");
        std::fs::write(path, profile).unwrap();
        if remote {
            fixture.assert_success(&["host", "start", "--profile", "fixture"]);
        }
        let mut terminal = TerminalClient::start(&fixture, &["--session-id", "fullscreen-dialogs"]);
        terminal.capture_name = format!("dialogs-{}", if remote { "remote" } else { "local" });
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"Retain this conversation context\r");
        tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
            .await
            .unwrap();
        terminal.send(b"retained background draft");
        terminal.until("retained background draft").await;
        let original = caret(&terminal);
        terminal.send(b"\x10");
        terminal.until("Enter select").await;
        assert!(
            !terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains('▏'),
            "read-only menu cannot present a text caret"
        );
        terminal.send(b"\x1b[200~MUST_NOT_LEAK\x1b[201~\x13\x0fx");
        terminal.send(b"\x03");
        terminal.absent("Enter select").await;
        terminal.until("retained background draft").await;
        assert_eq!(caret(&terminal), original);
        assert!(
            !terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("MUST_NOT_LEAK")
        );
        // Ctrl+C closed the menu; the in-flight model call still completes normally.
        state.release.notify_one();
        terminal.until("hello from daemon").await;
        let requests = state.requests.lock().unwrap().len();
        assert_eq!(requests, 2);
        terminal.send(b"\x15/effort\r");
        terminal.until("Reasoning effort").await;
        terminal.until("Provider default (high)").await;
        let (_, initial_y) = caret(&terminal);
        assert_eq!(
            initial_y, original.1,
            "effort selection must reuse the bottom Composer"
        );
        assert!(
            terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("/effort ▏")
        );
        for filter in ["low", "not-a-choice", "high"] {
            terminal.send(format!("\x15{filter}").as_bytes());
            terminal.until(&format!("{filter}▏")).await;
            assert_eq!(caret(&terminal).1, initial_y);
        }
        for (cols, rows) in [(110, 35), (80, 24), (42, 12), (28, 9), (110, 30)] {
            terminal.resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            });
            terminal.send(format!("\x15size-{cols}-{rows}").as_bytes());
            terminal.until(&format!("size-{cols}-{rows}▏")).await;
            let (_, y) = caret(&terminal);
            assert_eq!(
                y,
                rows - 4,
                "resize must keep selection in the bottom Composer"
            );
            let contents = terminal.screen.lock().unwrap().screen().contents();
            assert!(contents.contains("Esc back"));
            if rows >= 24 {
                assert!(contents.contains("Retain this conversation context"));
            }
        }
        terminal.send(b"\x1b");
        terminal.absent("Reasoning effort").await;
        for (command, label) in [
            ("/model", "Model for next request"),
            ("/login", "Log in · choose provider"),
            ("/help", "RSI · Help"),
        ] {
            terminal.send(format!("{command}\r").as_bytes());
            terminal.until(label).await;
            let contents = terminal.screen.lock().unwrap().screen().contents();
            assert!(contents.contains("Retain this conversation context"));
            assert!(contents.contains("fixture-model · high"));
            let (_, y) = caret(&terminal);
            if command == "/model" {
                assert_eq!(y, 26);
            }
            terminal.send(b"\x1b");
            terminal.absent(label).await;
        }
        terminal.send(b"read-only draft");
        terminal.until("read-only draft").await;
        terminal.send(b"\x10");
        terminal.select_menu("Request usage").await;
        terminal.until("Session usage").await;
        assert!(
            !terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains('▏')
        );
        terminal.send(b"\x1b[200~MUST_NOT_LEAK\x1b[201~\x13\x0fq\x04");
        terminal.send(b"\x1b");
        terminal.absent("Session usage").await;
        terminal.until("read-only draft").await;
        assert!(
            !terminal
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("MUST_NOT_LEAK")
        );
        assert_eq!(state.requests.lock().unwrap().len(), requests);
        terminal.send(b"\x15\x04");
        terminal.finish().await;
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        provider.abort();
    }
}
