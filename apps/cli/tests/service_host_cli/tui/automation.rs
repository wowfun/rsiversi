//! Explicit paired-product PTY projection; seed is prepared through public owners.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit paired binary, private Automation seed and PTY report"]
async fn model_free_home_reads_automation_and_current_readiness() {
    let mut fixture = CliFixture::new("http://127.0.0.1:1");
    fixture.binary = std::env::var("RSI_TEST_AUTOMATION_BINARY").unwrap().into();
    let seed = std::path::PathBuf::from(std::env::var("RSI_TEST_AUTOMATION_SEED").unwrap());
    assert!(seed.is_absolute());
    let runtime: serde_json::Value =
        serde_json::from_slice(&std::fs::read(seed.join("runtime-config.json")).unwrap()).unwrap();
    std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json")).unwrap();
    std::fs::write(fixture.temporary.path().join("config/rsi/host-profiles/fixture/host.profile.toml"),
        format!("format=1\n[[steps]]\nkind=\"patch\"\ntarget=\"automation\"\nconfig_json={}\n[[steps]]\nkind=\"patch\"\ntarget=\"automation\"\nenabled=true\n",
            serde_json::to_string(&serde_json::json!({"directory":seed,"runtime":runtime}).to_string()).unwrap())).unwrap();
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let mut terminal = TerminalClient::start(&fixture, &[]);
    terminal.until("/login").await;
    terminal.send(b"/automation\r");
    terminal.until("Deployment checks").await;
    terminal.until("Attempt 1").await;
    terminal.send(b"\t");
    terminal.until("\"entries\"").await;
    terminal.send(b"\x1b[6~");
    terminal.until("browser_readiness").await;
    terminal.until("available").await;
    terminal.send(b"\t\x1b[B\r");
    terminal.until("Create new attempt").await;
    terminal.send(b"\t");
    for _ in 0..12 {
        let before = terminal.screen.lock().unwrap().screen().contents();
        if before.contains("Expected visible text is missing") {
            break;
        }
        terminal.send(b"\x1b[6~");
        terminal
            .until_screen("detail page advanced", |screen| screen != before)
            .await;
    }
    terminal.until("Expected visible text is missing").await;
    terminal.send(b"\t\x1b[B\r");
    terminal.until("Deployment checks").await;
    terminal.until("Control receipt · Attempt 2: queued").await;
    terminal
        .until_screen("resulting Attempt actions", |screen| {
            screen.contains("Cancel attempt") || screen.contains("Create new attempt")
        })
        .await;
    terminal.send(b"\x1b");
    terminal.absent("Deployment checks").await;
    terminal.send(b"\x03");
    terminal.finish().await;
    fixture.assert_success(&["host", "stop"]);
}
