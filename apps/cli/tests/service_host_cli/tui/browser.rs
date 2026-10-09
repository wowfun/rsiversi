use super::*;
use std::io::BufRead as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit Linux confined browser and real TUI image-source acceptance"]
async fn session_browser_tui_uses_production_local_grant_and_renders_the_shared_png() {
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let node = std::env::var("RSI_TEST_BROWSER_NODE").unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/rsi/browser/tests/fixtures/session.mjs");
    let mut server = Server(
        std::process::Command::new(&node)
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let origin = format!("http://127.0.0.1:{}", port.trim());
    let (endpoint, _, provider) = gated_provider("history_search").await;
    let fixture = CliFixture::new(&endpoint);
    let runtime: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("RSI_TEST_BROWSER_CONFIG").unwrap()).unwrap(),
    )
    .unwrap();
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    let mut document: toml::Value =
        toml::from_str(&std::fs::read_to_string(&profile).unwrap()).unwrap();
    for step in [
        serde_json::json!({"kind":"patch","target":"session-browser","config":{"runtime":runtime}}),
        serde_json::json!({"kind":"patch","target":"session-browser","enabled":true}),
        serde_json::json!({"kind":"patch","target":"session-browser-ui","enabled":true}),
    ] {
        document["steps"]
            .as_array_mut()
            .unwrap()
            .push(toml::Value::try_from(step).unwrap());
    }
    std::fs::write(profile, toml::to_string(&document).unwrap()).unwrap();
    let mut terminal = TerminalClient::start(&fixture, &["--session-id", "tui-browser-proof"]);
    terminal.capture_name = "session-browser".into();
    let ready_by = Instant::now() + Duration::from_secs(90);
    while !terminal
        .screen
        .lock()
        .unwrap()
        .screen()
        .contents()
        .contains("Ctrl+J adds a line")
    {
        assert!(
            terminal.child.try_wait().unwrap().is_none(),
            "browser TUI exited during runtime preflight: {}",
            fixture.owner_log()
        );
        assert!(
            Instant::now() < ready_by,
            "browser TUI preflight did not finish: {}",
            fixture.owner_log()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    terminal.capture();
    terminal.send(b"\x10");
    terminal.select_menu("Service extensions").await;
    terminal.until("Inspect standard service views").await;
    terminal.send(b"\r");
    terminal.select_menu("List service extensions").await;
    terminal.until("Service extensions").await;
    terminal.send(b"\r");
    terminal.select_menu("Session browser").await;
    terminal.until("Page URL").await;
    let mut opened = false;
    for _ in 0..3 {
        terminal.send(b"\r");
        terminal.select_menu("Edit Page URL").await;
        terminal.send(format!("{origin}\r").as_bytes());
        terminal.until(&origin).await;
        terminal.send(b"\r");
        terminal
            .select_menu("Approve and open this exact local origin")
            .await;
        terminal
            .until_screen("browser opened or known model review", |screen| {
                screen.contains("Shared Session browser") || screen.contains("Service view changed")
            })
            .await;
        if terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("Shared Session browser")
        {
            opened = true;
            break;
        }
        // This refusal proves the stale action was not dispatched. Review and explicitly submit a new action.
        terminal.capture();
    }
    assert!(
        opened,
        "current browser model never admitted the explicit local grant"
    );
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Capture screenshot").await;
    terminal
        .until_screen("screenshot model publication", |screen| {
            screen.contains("Browser: live · imported ") && !screen.contains("imported 0 /")
        })
        .await;
    terminal.send(b"\r");
    terminal.select_menu("View current screenshot").await;
    terminal.until("grayscale terminal preview").await;
    terminal.capture();
    terminal.send(b"\r");
    terminal.select_menu("Close browser").await;
    terminal.until("closed").await;
    terminal.capture();
    terminal.send(b"\x1b\x04");
    terminal.finish().await;
    provider.abort();
}
