use super::*;
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the explicitly selected pinned rust-analyzer executable"]
async fn language_tui_opens_real_definition_and_closes_service_observation() {
    let (endpoint, state, provider) = gated_provider("lsp_query").await;
    let fixture = CliFixture::new(&endpoint);
    let position = configure_language(&fixture);
    *state.arguments.lock().unwrap() = Some(
        serde_json::json!({"operation":"hover","path":"src/main.rs","line":position["line"],"column":position["column"]}),
    );
    state.release.notify_one();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-language"]);
    terminal.capture_name = "language".into();
    terminal.until("Ctrl+J adds a line").await;
    find_definition(&mut terminal, &position).await;
    terminal.send(b"\r");
    terminal.select_menu("Open src/main.rs:2").await;
    terminal.until("Location: src/main.rs:2:12").await;
    terminal.until("pub struct Bird;").await;
    terminal.resize(PtySize {
        rows: 24,
        cols: 48,
        pixel_width: 0,
        pixel_height: 0,
    });
    terminal
        .until_screen("language narrow repaint", |screen| {
            screen.contains("pub struct Bird;")
                && screen.contains('┘')
                && !screen.contains("       ┌Detail")
        })
        .await;
    terminal.send(b"\r");
    terminal.select_menu("Close service view").await;
    terminal.until("Inspect standard service views").await;
    terminal.send(b"\x1b\x1b");
    terminal
        .until_screen("composer after service view", |screen| {
            !screen.contains("Enter select") && !screen.contains("Enter actions")
        })
        .await;
    assert!(
        state.requests.lock().unwrap().is_empty(),
        "inspection performs no model work"
    );
    terminal.resize(PtySize {
        rows: 30,
        cols: 110,
        pixel_width: 0,
        pixel_height: 0,
    });
    terminal.send(b"Use the language tool now\r");
    terminal.until("hello from daemon").await;
    terminal.send(b"\t\x10");
    terminal.select_menu("Card details").await;
    terminal.until("lsp_query").await;
    terminal.send(b"\r");
    terminal.select_menu("Recorded result").await;
    terminal.until("rsi.lsp.query").await;
    terminal.until("Bird").await;
    terminal.send(b"\x1b");
    terminal
        .until_screen("composer after typed language result", |screen| {
            !screen.contains("┌Detail") && !screen.contains("Enter select")
        })
        .await;
    terminal.send(b"\x04");
    terminal.finish().await;
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"].to_string().contains("Bird"))
    );
    provider.abort();
}

fn configure_language(fixture: &CliFixture) -> serde_json::Value {
    let analyzer = std::env::var_os("RSI_RUST_ANALYZER").expect("explicit pinned analyzer");
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../fixtures/rsi/lsp/prepare.py"
        ))
        .arg(&fixture.workspace)
        .arg(
            fixture
                .temporary
                .path()
                .join("config/rsi/host-profiles/fixture/host.profile.toml"),
        )
        .arg(analyzer)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let position: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    fixture.configure_agent_preset_root();
    let preset = fixture
        .temporary
        .path()
        .join("configured-agent-presets/language");
    std::fs::create_dir_all(&preset).unwrap();
    std::fs::write(
        preset.join("agent.profile.toml"),
        format!(
            "{}\n[[steps]]\nkind=\"plugin\"\nid=\"language\"\nplugin=\"rsi.lsp.tools\"\n",
            include_str!("../../../../../../plugins/rsi-agent-presets/standard/agent.profile.toml")
        ),
    )
    .unwrap();
    let settings = fixture.temporary.path().join("config/rsi/settings.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    value["rsi.agent-presets"]["default"] = "language".into();
    std::fs::write(settings, serde_json::to_vec(&value).unwrap()).unwrap();
    position
}

async fn find_definition(terminal: &mut TerminalClient, position: &serde_json::Value) {
    terminal.send(b"\x10");
    terminal.select_menu("Service extensions").await;
    terminal.until("Inspect standard service views").await;
    terminal.send(b"\r");
    terminal.select_menu("List service extensions").await;
    terminal.send(b"\r");
    terminal.select_menu("Code intelligence").await;
    terminal.until("Workspace file").await;
    for attempt in 0..4 {
        for (label, value) in [
            ("Line", position["line"].to_string()),
            ("Column", position["column"].to_string()),
        ] {
            terminal.send(b"\r");
            terminal.select_menu(&format!("Edit {label}")).await;
            terminal.until("Enter save · Esc discard").await;
            terminal.send(format!("\x05\x15{value}\r").as_bytes());
            terminal
                .until_screen("saved language field", |screen| {
                    !screen.contains("Enter save") && screen.contains(&format!("{label}: {value}"))
                })
                .await;
        }
        terminal.send(b"\r");
        terminal.select_menu("Find definition").await;
        terminal
            .until_screen("query or explicit stale form review", |screen| {
                !screen.contains("Working…")
                    && (screen.contains("Query: Definition")
                        || screen.contains("Service view changed"))
            })
            .await;
        if terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("Query: Definition")
        {
            break;
        }
        assert!(
            attempt < 3,
            "service presentation never stabilized for explicit review"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let empty = terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("No locations reported.");
        if !empty {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "language server never returned definition"
        );
        terminal.send(b"\r");
        terminal.select_menu("Repeat query").await;
        terminal
            .until_screen("completed repeated definition query", |screen| {
                screen.contains("Query: Definition") && !screen.contains("Working…")
            })
            .await;
    }
}
