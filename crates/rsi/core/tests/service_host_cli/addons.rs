use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_cli_default_root_drives_only_the_running_managers_explicit_selection() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let manifest = fixture.workspace.join("native.toml");
    std::fs::write(
        fixture.workspace.join("artifact.bin"),
        b"deliberately invalid native code",
    )
    .unwrap();
    std::fs::write(&manifest, format!("format = 1\nid = 'fixture.cli'\nplugin = 'fixture.native-cli'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
    fixture.assert_success(&["addon", "install", "native.toml", "--output", "json"]);
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let initial = status(&fixture);
    assert_eq!(initial["health"], "ready");
    assert!(initial["desired"].as_array().unwrap().is_empty());
    fixture.assert_success(&["addon", "enable", "fixture.cli"]);
    let failed = wait_health(&fixture, "failed").await;
    assert_eq!(failed["desired"][0]["id"], "fixture.cli");
    assert_eq!(failed["source_revision"], "2");
    assert_eq!(failed["retained_failed_finalizations"], 0);
    fixture.assert_success(&["addon", "disable", "fixture.cli"]);
    let ready = wait_health(&fixture, "ready").await;
    assert!(ready["desired"].as_array().unwrap().is_empty());
    assert_eq!(ready["source_revision"], "3");
    fixture.assert_success(&["host", "stop"]);
    provider.abort();
}
fn status(fixture: &CliFixture) -> serde_json::Value {
    let output = fixture.assert_success(&["--profile", "inspector", "native"]);
    serde_json::from_slice(&output.stdout).unwrap()
}
async fn wait_health(fixture: &CliFixture, health: &str) -> serde_json::Value {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let value = status(fixture);
        if value["health"] == health {
            return value;
        }
        assert!(
            std::time::Instant::now() < until,
            "expected {health}: {value}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
