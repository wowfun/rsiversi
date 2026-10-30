use super::*;
use rsi_credentials_local::SecretStore as _;

pub(super) fn backups(fixture: &CliFixture) -> Vec<std::path::PathBuf> {
    let mut entries = std::fs::read_dir(fixture.temporary.path().join("state/rsi"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with(".reset-") && name.ends_with("-agent")
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}
fn seed(fixture: &CliFixture) -> Vec<u8> {
    let root = fixture.temporary.path().join("state/rsi/agent");
    drop(rsi_agent_store_sqlite::SqliteStore::open(&root).unwrap());
    let db = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
    db.pragma_update(
        None,
        "user_version",
        rsi_agent_store_protocol::AGENT_STORE_SCHEMA_VERSION - 1,
    )
    .unwrap();
    drop(db);
    std::fs::read(root.join("sessions.sqlite3")).unwrap()
}

#[test]
fn reset_state_cli_archives_owned_state_and_preserves_credentials_and_settings() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let config = fixture.temporary.path().join("config/rsi/settings.json");
    let settings = std::fs::read(&config).unwrap();
    let credentials = fixture
        .temporary
        .path()
        .join("config/rsi/credentials/credentials.json");
    rsi_credentials_local::FileSecretStore::new(&credentials)
        .set(
            &rsi_credentials_protocol::CredentialRef::new("fixture", "retained").unwrap(),
            &rsi_credentials_protocol::SecretValue::new("isolated-secret").unwrap(),
        )
        .unwrap();
    let secrets = std::fs::read(&credentials).unwrap();
    let output = fixture.assert_success(&["--reset-state", "run", "test-cli", "--list"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Preserved backup:"));
    let saved = backups(&fixture);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        std::fs::read(saved[0].join("sessions.sqlite3")).unwrap(),
        before
    );
    assert_eq!(std::fs::read(config).unwrap(), settings);
    assert_eq!(std::fs::read(credentials).unwrap(), secrets);
    fixture.assert_success(&["run", "test-cli", "--list"]);
    assert_eq!(backups(&fixture), saved);
}

#[test]
fn reset_help_preserves_broken_configuration_and_store() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let launch = fixture.temporary.path().join("config/rsi/rsi.toml");
    std::fs::write(&launch, b"invalid [").unwrap();
    fixture.assert_success(&["--reset-config", "--reset-state", "tui", "--help"]);
    assert_eq!(std::fs::read(&launch).unwrap(), b"invalid [");
    assert_eq!(
        std::fs::read(
            fixture
                .temporary
                .path()
                .join("state/rsi/agent/sessions.sqlite3")
        )
        .unwrap(),
        before
    );
    assert!(backups(&fixture).is_empty());
    fixture.assert_success(&["--reset-config", "--reset-state", "config", "init"]);
    fixture.assert_success(&["config", "validate"]);
    assert_eq!(backups(&fixture).len(), 1);
}

#[test]
fn reset_does_not_follow_a_profile_to_an_external_store() {
    use std::io::Write as _;

    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let external = fixture.temporary.path().join("external-store");
    drop(rsi_agent_store_sqlite::SqliteStore::open(&external).unwrap());
    std::fs::write(external.join("keep"), b"outside managed home").unwrap();
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&profile)
            .unwrap(),
        "\n[[steps]]\nkind='patch'\ntarget='rsi-agent-store'\nconfig={{ root={} }}",
        serde_json::to_string(&external).unwrap()
    )
    .unwrap();
    fixture.assert_success(&["--reset-state", "run", "test-cli", "--list"]);
    assert_eq!(
        std::fs::read(external.join("keep")).unwrap(),
        b"outside managed home"
    );
    assert_eq!(
        std::fs::read(backups(&fixture)[0].join("sessions.sqlite3")).unwrap(),
        before
    );
}

#[test]
fn reset_rejects_active_host_and_restart_performs_one_reset() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    fixture.assert_success(&["--reset-state", "host", "start"]);
    let saved = backups(&fixture);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        std::fs::read(saved[0].join("sessions.sqlite3")).unwrap(),
        before
    );
    let blocked = fixture.run(&["--reset-state", "config", "validate"]);
    assert!(!blocked.status.success());
    assert_eq!(backups(&fixture), saved);
    fixture.assert_success(&["host", "reload"]);
    assert_eq!(backups(&fixture), saved);
    fixture.assert_success(&["--reset-state", "host", "restart"]);
    assert_eq!(backups(&fixture).len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_state_headless_starts_fresh_and_preserves_previous_history() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&[
        "run",
        "test-headless",
        "old message",
        "--session-id",
        "old-session",
    ]);
    fixture.assert_success(&["host", "stop"]);
    fixture.assert_success(&[
        "--reset-state",
        "run",
        "test-headless",
        "new message",
        "--session-id",
        "new-session",
    ]);
    let listed = fixture.assert_success(&["run", "test-cli", "--list"]);
    let sessions = String::from_utf8(listed.stdout).unwrap();
    assert!(sessions.contains("new-session"), "{sessions}");
    assert!(!sessions.contains("old-session"), "{sessions}");
    assert_eq!(backups(&fixture).len(), 1);
    provider.abort();
}
