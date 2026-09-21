use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_tui_permissions_detach_cancel_and_reap_with_or_without_native_model() {
    for configured in [true, false] {
        let fixture = CliFixture::new("http://127.0.0.1:1");
        if !configured {
            std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json"))
                .unwrap();
        }
        configure(&fixture);
        let mut terminal = TerminalClient::start(&fixture, &[]);
        terminal.capture_name = format!("external-model-{configured}");
        let native_hint = if configured {
            "Ctrl+J adds a line"
        } else {
            "No default model"
        };
        terminal.until(native_hint).await;
        terminal.send(b"/external\r");
        terminal.select_menu("Start fixture-external").await;
        terminal.until("Connected · Ready").await;
        terminal.send(b"permission\r");
        terminal.until("1 pending permissions").await;
        terminal.send(b"\x1b");
        terminal.until(native_hint).await;
        terminal.send(b"/attention\r");
        terminal.select_menu("Review permission").await;
        terminal.until("Read fixture · reject_always").await;
        terminal.select_menu("Read fixture · allow_always").await;
        terminal.until("fixture-permission-answered").await;
        terminal.until("Completed").await;
        terminal.send(b"permission\r");
        terminal.until("1 pending permissions").await;
        terminal.send(b"\x10");
        terminal.select_menu("Read fixture · reject_once").await;
        terminal.until("Completed").await;
        terminal.send(b"EXTERNAL_DRAFT_KEEP\x1b");
        terminal.until(native_hint).await;
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("new-count")).unwrap(),
            "new\n"
        );
        let pid = std::fs::read_to_string(fixture.workspace.join("peer-pids")).unwrap();
        assert!(std::path::Path::new("/proc").join(pid.trim()).exists());
        terminal.send(b"/external\r");
        terminal.select_menu("fixture-external").await;
        terminal.until("EXTERNAL_DRAFT_KEEP").await;
        terminal.resize(PtySize {
            rows: 28,
            cols: 48,
            pixel_width: 0,
            pixel_height: 0,
        });
        terminal
            .until_screen("narrow external composer", |screen| {
                screen.contains("EXTERNAL_DRAFT_KEEP") && screen.contains("Enter select · Esc back")
            })
            .await;
        terminal.resize(PtySize {
            rows: 30,
            cols: 110,
            pixel_width: 0,
            pixel_height: 0,
        });
        terminal.until("Ctrl+P permissions and controls").await;
        terminal.send(b"\x15wait\r");
        terminal.until("Running").await;
        terminal.send(b"\x03");
        terminal.until("Cancelled").await;
        terminal.send(b"\x10");
        terminal.select_menu("Close peer").await;
        terminal.until("Closed").await;
        assert!(!std::path::Path::new("/proc").join(pid.trim()).exists());
        terminal.send(b"\x1b");
        terminal.until(native_hint).await;
        terminal.send(b"\x04");
        terminal.finish().await;
    }
}

fn configure(fixture: &CliFixture) {
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/rsi/acp/agent.py")
        .canonicalize()
        .unwrap();
    let secret = serde_json::json!({"kind":"literal","value":"private-fixture-secret"});
    let config = serde_json::json!({"directory":fixture.temporary.path().join("state/rsi/acp"),"endpoints":[{"id":"fixture-external","enabled":true,"cwd":fixture.workspace,"sandbox":"danger-full-access","launch":{"program":"/usr/bin/python3","arguments":["-u",script,fixture.workspace,"normal"],"environment":{"FIXTURE_SECRET":secret}},"mcp_servers":[{"name":"private","launch":{"program":"/usr/bin/python3","environment":{"MCP_SECRET":secret}}}]}]});
    let mut document: toml::Value =
        toml::from_str(&std::fs::read_to_string(&profile).unwrap()).unwrap();
    document["steps"].as_array_mut().unwrap().push(
        toml::Value::try_from(
            serde_json::json!({"kind":"patch","target":"rsi-acp","config":config}),
        )
        .unwrap(),
    );
    std::fs::write(profile, toml::to_string(&document).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delegated_tool_card_opens_the_existing_host_conversation() {
    let (endpoint, state, provider) = gated_provider("external_agent").await;
    *state.arguments.lock().unwrap() =
        Some(serde_json::json!({"operation":"start","endpoint":"fixture-external"}));
    let fixture = CliFixture::new(&endpoint);
    configure(&fixture);
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "delegation-card"]);
    terminal.capture_name = "delegation-card".into();
    terminal.until("Ctrl+J adds a line").await;
    terminal.send(b"Delegate this task\r");
    tokio::time::timeout(Duration::from_secs(20), state.requested.notified())
        .await
        .unwrap();
    state.release.notify_one();
    terminal.until("hello from daemon").await;
    terminal.send(b"\t\x10");
    terminal.select_menu("Raw sources / full output").await;
    terminal.select_menu("Open external conversation").await;
    terminal.until("Connected · Ready").await;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("new-count")).unwrap(),
        "new\n"
    );
    assert_eq!(state.requests.lock().unwrap().len(), 2);
    terminal.capture();
    terminal.send(b"work\r");
    terminal.until("fixture-output").await;
    terminal.send(b"\x10");
    terminal.select_menu("Close peer").await;
    terminal.until("Closed").await;
    let pid = std::fs::read_to_string(fixture.workspace.join("peer-pids")).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid.trim()).exists());
    terminal.send(b"\x1b");
    terminal.until("Enter actions").await;
    terminal.send(b"\x1b");
    terminal.until("End follows output").await;
    terminal.send(b"\x04");
    terminal.finish().await;
    provider.abort();
}
