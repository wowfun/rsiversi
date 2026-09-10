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
    std::fs::write(&manifest, format!("format = 2\nscope = 'agent'\nid = 'fixture.cli'\nplugin = 'fixture.native-cli'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
    let config = fixture.temporary.path().join("config/rsi");
    let preset = config.join("agent-presets/native");
    std::fs::create_dir_all(&preset).unwrap();
    std::fs::write(
        preset.join("agent.profile.toml"),
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'native'\nplugin = 'fixture.native-cli'\n",
    )
    .unwrap();
    fixture.assert_success(&["addon", "install", "native.toml", "--output", "json"]);
    assert_preset_health(&fixture, "broken");
    assert!(!fixture.run(&["addon", "refresh"]).status.success());
    fixture.assert_success(&["host", "start", "--profile", "fixture"]);
    let refreshed = fixture.assert_success(&["addon", "refresh"]);
    let refreshed: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["source_revision"], "1");
    assert_eq!(refreshed["changed"], false);
    assert_eq!(refreshed["selected"], 0);
    let initial = status(&fixture);
    assert_eq!(initial["health"], "ready");
    assert!(initial["desired"].as_array().unwrap().is_empty());
    fixture.assert_success(&["addon", "enable", "fixture.cli"]);
    assert_preset_health(&fixture, "healthy");
    let failed = wait_health(&fixture, "failed").await;
    assert_eq!(failed["desired"][0]["id"], "fixture.cli");
    assert_eq!(failed["source_revision"], "2");
    assert_eq!(failed["retained_failed_finalizations"], 0);
    let failed_refresh = fixture.run(&["addon", "refresh"]);
    assert!(!failed_refresh.status.success());
    assert!(String::from_utf8_lossy(&failed_refresh.stderr).contains("native load rejected"));
    fixture.assert_success(&["addon", "disable", "fixture.cli"]);
    let ready = wait_health(&fixture, "ready").await;
    assert!(ready["desired"].as_array().unwrap().is_empty());
    assert_eq!(ready["source_revision"], "3");
    assert_preset_health(&fixture, "broken");
    fixture.assert_success(&["host", "stop"]);
    std::fs::write(
        config.join("native-addons/state.json"),
        b"invalid native index",
    )
    .unwrap();
    assert!(!fixture.run(&["agent-preset", "list"]).status.success());
    fixture.assert_success(&["agent-preset", "default", "get"]);
    fixture.assert_success(&["agent-preset", "default", "clear"]);
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

fn assert_preset_health(fixture: &CliFixture, expected: &str) {
    let output = fixture.assert_success(&["agent-preset", "list", "--output", "json"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = value["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "native")
        .unwrap();
    assert_eq!(row["status"], expected);
}
