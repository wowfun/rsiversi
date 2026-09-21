use super::*;
use rsi_credentials_local::SecretStore as _;
use std::fmt::Write as _;

pub(super) fn backups(fixture: &CliFixture) -> Vec<std::path::PathBuf> {
    let root = fixture.temporary.path().join("state/rsi");
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("agent.backup-")
        })
        .map(|path| path.join("store"))
        .collect()
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
fn reset_state_cli_preserves_configuration_and_initializes_empty_sessions() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let rejected = fixture.run(&["--profile", "test-cli", "--list"]);
    assert!(!rejected.status.success());
    let diagnostic = String::from_utf8(rejected.stderr).unwrap();
    assert!(
        diagnostic.contains(&format!(
            "expected {}, actual {}",
            rsi_agent_store_protocol::AGENT_STORE_SCHEMA_VERSION,
            rsi_agent_store_protocol::AGENT_STORE_SCHEMA_VERSION - 1
        )),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("--reset-state"), "{diagnostic}");
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
    let output = fixture.run(&["--profile", "test-cli", "--reset-state", "--list"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("previous state preserved at"));
    let saved = backups(&fixture);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        std::fs::read(saved[0].join("sessions.sqlite3")).unwrap(),
        before
    );
    assert_eq!(std::fs::read(config).unwrap(), settings);
    assert_eq!(std::fs::read(credentials).unwrap(), secrets);
    fixture.assert_success(&["--profile", "test-cli", "--list"]);
    assert_eq!(backups(&fixture), saved);
}

#[test]
fn reset_state_preflight_and_help_preserve_the_store() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let remote = fixture
        .temporary
        .path()
        .join("config/rsi/application-profiles/remote/application.profile.toml");
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    std::fs::write(remote, r#"format = 1
[[steps]]
kind = "plugin"
id = "connection"
plugin = "rsi.application.http"
config = { origin = "http://127.0.0.1:1", endpoint_id = "00000000000000000000000000000000", credential = { owner = "remote", slot = "device" }, allow_loopback_http = true }
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.cli"
"#).unwrap();
    let remote = fixture.run(&["--profile", "remote", "--reset-state", "--list"]);
    assert_eq!(remote.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&remote.stderr)
            .contains("unavailable for remote HTTP applications"),
        "{remote:?}"
    );
    for arguments in [
        vec!["tui", "--reset-state", "--unknown-option"],
        vec!["tui", "--reset-state", "--trust-workspace"],
        vec!["--profile", "tui", "--reset-state", "--trust-workspace"],
        vec!["--profile", "headless", "--reset-state"],
        vec!["--profile", "inspector", "runtime", "--reset-state"],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
    }
    let help = fixture.run(&["tui", "--reset-state", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--reset-state"));
    assert!(backups(&fixture).is_empty());
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
}

#[test]
fn reset_state_uses_the_profile_store_root_without_touching_the_default_store() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let original = seed(&fixture);
    let custom = fixture.temporary.path().join("custom-store");
    drop(rsi_agent_store_sqlite::SqliteStore::open(&custom).unwrap());
    std::fs::write(custom.join("retained-in-backup"), b"custom root").unwrap();
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    let mut source = std::fs::read_to_string(&profile).unwrap();
    write!(
        source,
        "\n[[steps]]\nkind = 'patch'\ntarget = 'rsi-agent-store'\nconfig = {{ root = {} }}\n",
        serde_json::to_string(&custom).unwrap()
    )
    .unwrap();
    std::fs::write(profile, source).unwrap();
    let output = fixture.assert_success(&["--profile", "test-cli", "--reset-state", "--list"]);
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(
        diagnostic.contains(custom.to_str().unwrap()),
        "{diagnostic}"
    );
    assert!(!custom.join("retained-in-backup").exists());
    assert!(backups(&fixture).is_empty());
    assert_eq!(
        std::fs::read(
            fixture
                .temporary
                .path()
                .join("state/rsi/agent/sessions.sqlite3")
        )
        .unwrap(),
        original
    );
    let backup = std::fs::read_dir(fixture.temporary.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("custom-store.backup-")
        })
        .unwrap();
    assert_eq!(
        std::fs::read(backup.join("store/retained-in-backup")).unwrap(),
        b"custom root"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_state_foreground_host_and_http_serve_report_the_backup() {
    use tokio::io::AsyncBufReadExt as _;
    for http in [false, true] {
        let fixture = CliFixture::new("http://127.0.0.1:1");
        let original = seed(&fixture);
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bind = reserved.local_addr().unwrap().to_string();
        let origin = format!("http://{bind}");
        let args = if http {
            vec![
                "--profile",
                "serve",
                "--reset-state",
                "--bind",
                &bind,
                "--origin",
                &origin,
                "--dev-http",
            ]
        } else {
            vec!["host", "serve", "--profile", "fixture", "--reset-state"]
        };
        drop(reserved);
        let mut child = fixture
            .tokio_command()
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = tokio::io::BufReader::new(child.stderr.take().unwrap()).lines();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("child ended before reset receipt");
                if line.contains("previous state preserved at") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        // The reset receipt precedes Store/application readiness; wait for the Host signal.
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !fixture
                .run(&["host", "status"])
                .stdout
                .starts_with(b"running\t")
            {
                assert!(child.try_wait().unwrap().is_none());
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        fixture.assert_success(&["host", "stop"]);
        let status = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "{status:?}");
        let saved = backups(&fixture);
        assert_eq!(saved.len(), 1);
        assert_eq!(
            std::fs::read(saved[0].join("sessions.sqlite3")).unwrap(),
            original
        );
    }
}

#[test]
fn reset_state_reports_the_backup_even_when_later_application_startup_fails() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = reserved.local_addr().unwrap().to_string();
    let output = fixture.run(&[
        "--profile",
        "serve",
        "--reset-state",
        "--bind",
        &bind,
        "--origin",
        &format!("http://{bind}"),
        "--dev-http",
    ]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("previous state preserved at"),
        "{output:?}"
    );
    let saved = backups(&fixture);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        std::fs::read(saved[0].join("sessions.sqlite3")).unwrap(),
        before
    );
}

#[test]
fn reset_state_daemon_returns_receipt_rejects_active_owner_and_reloads_once() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let before = seed(&fixture);
    let output = fixture.run(&["host", "start", "--profile", "fixture", "--reset-state"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("previous state preserved at"),
        "{output:?}"
    );
    let saved = backups(&fixture);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        std::fs::read(saved[0].join("sessions.sqlite3")).unwrap(),
        before
    );
    let blocked = fixture.run(&["--profile", "test-cli", "--reset-state", "--list"]);
    assert_eq!(blocked.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("requires an idle Service Host"));
    fixture.assert_success(&["host", "reload"]);
    fixture.assert_success(&["--profile", "test-cli", "--list"]);
    assert_eq!(backups(&fixture), saved);
    let restarted = fixture.run(&["host", "restart", "--profile", "fixture", "--reset-state"]);
    assert!(restarted.status.success(), "{restarted:?}");
    assert!(String::from_utf8_lossy(&restarted.stderr).contains("previous state preserved at"));
    fixture.assert_success(&["host", "stop"]);
    assert_eq!(backups(&fixture).len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_state_headless_uses_a_fresh_store_and_preserves_previous_history() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&[
        "--profile",
        "test-headless",
        "old message",
        "--session-id",
        "old-session",
    ]);
    fixture.assert_success(&[
        "--profile",
        "test-headless",
        "--reset-state",
        "new message",
        "--session-id",
        "new-session",
    ]);
    let listed = fixture.run(&["--profile", "test-cli", "--list"]);
    let sessions = String::from_utf8(listed.stdout).unwrap();
    assert!(sessions.contains("new-session"), "{sessions}");
    assert!(!sessions.contains("old-session"), "{sessions}");
    assert_eq!(backups(&fixture).len(), 1);
    provider.abort();
}
