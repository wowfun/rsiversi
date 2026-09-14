use super::*;
use rsi_credentials_local::SecretStore as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incompatible_agent_store_reports_schema_and_preserves_existing_bytes() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let root = fixture.temporary.path().join("state/rsi/agent");
    drop(rsi_agent_store_sqlite::SqliteStore::open(&root).unwrap());
    let path = root.join("sessions.sqlite3");
    let version = rsi_agent_store_protocol::AGENT_STORE_SCHEMA_VERSION;
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", version - 1).unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let mut terminal = TerminalClient::launch(&fixture, &[], &["tui"]);
    terminal.until("Agent Store schema mismatch").await;
    terminal
        .until(&format!("expected {version}, actual {}", version - 1))
        .await;
    terminal.until("no automatic migration").await;
    let output = terminal.output.lock().unwrap();
    assert!(String::from_utf8_lossy(&output).contains(root.to_str().unwrap()));
    drop(output);
    let status = terminal.child.wait().unwrap();
    assert_eq!(status.exit_code(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_credential_store_retry_does_not_add_a_navigation_step() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json")).unwrap();
    let bus = format!(
        "unix:path={}",
        fixture.temporary.path().join("absent-bus").display()
    );
    let mut terminal = TerminalClient::launch_environment(
        &fixture,
        &[],
        &["tui"],
        &[("DBUS_SESSION_BUS_ADDRESS", Some(bus.as_str()))],
    );
    terminal.until("No session attached").await;
    let credential_path = fixture
        .temporary
        .path()
        .join("config/rsi/credentials/credentials.json");
    rsi_credentials_local::FileSecretStore::new(&credential_path)
        .set(
            &rsi_credentials_protocol::CredentialRef::new("fixture", "unused").unwrap(),
            &rsi_credentials_protocol::SecretValue::new("fixture").unwrap(),
        )
        .unwrap();
    std::fs::write(&credential_path, "{broken").unwrap();
    terminal.send(b"/login deepseek\r");
    terminal.until("credential file is invalid").await;
    terminal.until("Enter retry").await;
    assert!(
        !terminal
            .screen
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains('▏')
    );
    terminal.send(b"\r");
    terminal.until("credential file is invalid").await;
    std::fs::write(&credential_path, r#"{"version":1,"entries":[]}"#).unwrap();
    terminal.send(b"\r");
    terminal.until("Missing credential").await;
    terminal.send(b"\x1b");
    terminal.absent("Missing credential").await;
    terminal.send(b"/quit\r");
    terminal.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_configuration_is_interactive_through_both_tui_entries() {
    for (entry, missing_route) in [
        (&["tui"][..], false),
        (&["--profile", "tui"][..], false),
        (&["tui"][..], true),
    ] {
        let fixture = CliFixture::new("http://127.0.0.1:1");
        if !missing_route {
            std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json"))
                .unwrap();
        }
        let mut terminal = TerminalClient::launch(&fixture, &[], entry);
        terminal.until("No session attached").await;
        if missing_route {
            terminal
                .until("Default route fixture/fixture-model is unavailable")
                .await;
        }
        terminal.send(b"retained draft\r");
        terminal.until("Draft retained; nothing was sent").await;
        terminal.until("retained draft").await;
        terminal.send(b"\x10");
        terminal.until("Recent sessions").await;
        terminal.send(b"\x1b[B\x1b[B\r");
        terminal.until("┌Recent sessions").await;
        terminal
            .until("Enter opens · Ctrl+Y copy ID · Esc back")
            .await;
        terminal.send(b"\x1b");
        terminal.absent("┌Recent sessions").await;
        if missing_route {
            let pid = terminal.child.process_id().unwrap();
            assert!(
                Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
        } else {
            terminal.send(b"\x03");
        }
        terminal.finish().await;
        let mut restarted = TerminalClient::launch(&fixture, &[], entry);
        restarted.until("No session attached").await;
        assert!(
            !restarted
                .screen
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("retained draft")
        );
        restarted.send(b"/quit\r");
        restarted.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Login, explicit default effort and restart share one isolated local/daemon journey.
async fn login_discovery_default_first_message_and_restart_local_and_daemon() {
    for remote in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let lists = requests.clone();
        let router = Router::new().route("/v1/chat/completions", post(chat)).route("/v1/models", axum::routing::get(move |headers: axum::http::HeaderMap| {
            let lists = lists.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fixture-secret");
                lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(serde_json::json!({"data":[{"id":"setup-model","context_window_tokens":10000,"max_output_tokens":1000}]}))
            }
        }));
        let provider = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let fixture = CliFixture::new(&endpoint);
        std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json")).unwrap();
        if remote {
            fixture.assert_success(&["host", "start"]);
        }
        let mut terminal =
            TerminalClient::launch(&fixture, &["--session-id", "setup-session"], &["tui"]);
        terminal.until("No session attached").await;
        terminal.send(b"/login openai-compatible\r");
        terminal.until("API base URL").await;
        terminal.send(format!("{endpoint}\r").as_bytes());
        terminal.until("API key · masked").await;
        terminal.until("Enter reuses").await;
        // Explicit masked input persists a file entry, even with an environment fallback.
        terminal.send(b"fixture-secret\r");
        terminal.until("setup-model").await;
        terminal.send(b"\r");
        terminal.select_menu("Provider default").await;
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"first setup message\r");
        terminal.until("hello from daemon").await;
        terminal.send(b"/model\r");
        terminal.until("Model for next request").await;
        terminal.until("Enter select · ^S default · Esc back").await;
        terminal.send(b"\x1b");
        terminal.absent("Model for next request").await;
        terminal.send(b"\x04");
        terminal.finish().await;
        assert!(
            !String::from_utf8_lossy(&terminal.output.lock().unwrap()).contains("fixture-secret")
        );
        assert!(
            fixture
                .temporary
                .path()
                .join("config/rsi/credentials/credentials.json")
                .is_file()
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        let settings: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.temporary.path().join("config/rsi/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["rsi.agent"]["default_model"]["model"],
            "setup-model"
        );
        if remote {
            fixture.assert_success(&["host", "stop"]);
            assert!(
                fixture
                    .command()
                    .args(["host", "start"])
                    .env_remove("RSI_OPENAI_COMPATIBLE_API_KEY")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let mut restarted = TerminalClient::launch_environment(
            &fixture,
            &["--session-id", "setup-restart"],
            &["--profile", "tui"],
            &[("RSI_OPENAI_COMPATIBLE_API_KEY", None)],
        );
        restarted.until("Ctrl+J adds a line").await;
        restarted.send(b"message after file-only restart\r");
        restarted.until("hello from daemon").await;
        restarted.send(b"\x04");
        restarted.finish().await;
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "restart must use durable configuration without discovering again"
        );
        // Resume still works after removing the global default (restart Host to reload durable settings).
        if remote {
            fixture.assert_success(&["host", "stop"]);
        }
        std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json")).unwrap();
        let mut resumed =
            TerminalClient::launch(&fixture, &["--resume", "setup-session"], &["tui"]);
        resumed.until("first setup message").await;
        resumed.send(b"\x04");
        resumed.finish().await;
        provider.abort();
    }
}

#[derive(Debug, Default)]
struct TestSecrets(
    std::sync::Mutex<
        std::collections::BTreeMap<
            rsi_credentials_protocol::CredentialRef,
            rsi_credentials_protocol::SecretValue,
        >,
    >,
);
impl rsi_credentials_local::SecretStore for TestSecrets {
    fn get(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<Option<rsi_credentials_protocol::SecretValue>> {
        Ok(self.0.lock().unwrap().get(reference).cloned())
    }
    fn set(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
        secret: &rsi_credentials_protocol::SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(reference.clone(), secret.clone());
        Ok(())
    }
    fn unset(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<bool> {
        Ok(self.0.lock().unwrap().remove(reference).is_some())
    }
}

// Runs only as an explicitly selected isolated PTY child; ordinary test discovery is inert.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn injected_credentials_tui_child() {
    let Ok(mode) = std::env::var("RSI_TEST_SETUP_CHILD") else {
        return;
    };
    let paths = rsi_host::HostPaths::new(
        std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("rsi"),
        std::path::PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap()).join("rsi"),
        std::path::PathBuf::from(std::env::var_os("XDG_CACHE_HOME").unwrap()).join("rsi"),
    )
    .unwrap();
    let store = Arc::new(TestSecrets::default());
    let composition =
        rsi::StandardComposition::new(paths.clone(), std::collections::BTreeMap::new(), None)
            .with_credential_store(store.clone());
    let profiles = rsi::ProfileCatalog::new(paths.clone());
    let stop = tokio_util::sync::CancellationToken::new();
    let daemon = if mode == "daemon" {
        let profile = profiles
            .host(&rsi::HostProfileId::new("standard").unwrap())
            .unwrap();
        let owner = rsi_service_host::HostOwnerLease::try_acquire(
            rsi_service_host::ServiceHostPaths::from_host_paths(&paths).unwrap(),
        )
        .unwrap();
        let daemon = rsi::StandardServiceDaemon::start(composition.clone(), &profile, owner)
            .await
            .unwrap();
        Some(tokio::spawn(daemon.run(stop.clone())))
    } else {
        None
    };
    for id in ["secret-session", "secret-restart"] {
        let profile = profiles
            .application(&rsi::ApplicationProfileId::new("tui").unwrap())
            .unwrap();
        let running = rsi::start_application(
            composition.clone(),
            vec!["--session-id".into(), id.into()],
            profile.program().unwrap(),
        )
        .await
        .unwrap();
        let entry = running
            .lookup_local::<rsi_application::ApplicationRunContract>()
            .unwrap();
        assert_eq!(entry.run().await.unwrap(), 0);
        assert!(running.shutdown().await.is_clean());
    }
    assert_eq!(store.0.lock().unwrap().len(), 1);
    stop.cancel();
    if let Some(daemon) = daemon {
        daemon.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One observable login, rejected-key repair, submission and restart flow.
async fn masked_key_and_manual_limits_complete_setup_with_injected_store() {
    for mode in ["embedded", "daemon"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let rejections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rejected = rejections.clone();
        let router = Router::new()
            .route(
                "/v1/models",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let rejected = rejected.clone();
                    async move {
                        use axum::response::IntoResponse as _;
                        if headers["authorization"] == "Bearer rejected-fixture-key" {
                            rejected.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        assert_eq!(headers["authorization"], "Bearer never-in-scene-secret");
                        axum::Json(serde_json::json!({"data":[{"id":"unknown-capacity"}]}))
                            .into_response()
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(|headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer never-in-scene-secret");
                    chat().await
                }),
            );
        let provider = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut fixture = CliFixture::new(&endpoint);
        std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json")).unwrap();
        fixture.binary = std::env::current_exe().unwrap();
        let mut terminal = TerminalClient::launch_environment(
            &fixture,
            &[],
            &[
                "--exact",
                "tui::setup::injected_credentials_tui_child",
                "--nocapture",
            ],
            &[("RSI_TEST_SETUP_CHILD", Some(mode))],
        );
        terminal.until("No session attached").await;
        terminal.send(b"retained before setup\r");
        terminal.until("Draft retained; nothing was sent").await;
        terminal.send(b"\x10\r");
        terminal.until("choose provider").await;
        terminal.send(b"\x1b[B\x1b[B\r");
        terminal.until("API base URL").await;
        terminal.send(format!("{endpoint}\r").as_bytes());
        terminal.until("API key · masked").await;
        terminal.until("Missing credential").await;
        terminal.send(b"\x1b[200~rejected-fixture-key\x1b[201~\x19");
        terminal.until("••••").await;
        terminal.send(b"\r");
        terminal.until("invalid API input:").await;
        terminal.until("Change API key").await;
        terminal.send(b"\x1b[B\x1b[B\r");
        terminal.until("Saved key").await;
        terminal.send(b"\x1b[200~never-in-scene-secret\x1b[201~\x19");
        terminal.until("••••").await;
        terminal.send(b"\r");
        terminal.until("unknown-capacity").await;
        terminal.send(b"\r");
        terminal.until("Context window tokens").await;
        terminal.send(b"0\r");
        terminal.until("positive 32-bit integer").await;
        terminal.send(b"\x7f10000\r");
        terminal.until("Maximum output tokens").await;
        terminal.send(b"1000\r");
        terminal.select_menu("Provider default").await;
        terminal.until("unknown-capacity").await;
        terminal.until("retained before setup").await;
        terminal.send(b"\r");
        terminal.until("hello from daemon").await;
        terminal.send(b"\x04");
        terminal.absent("hello from daemon").await;
        terminal.until("Ctrl+J adds a line").await;
        terminal.send(b"second generation message\r");
        terminal.until("hello from daemon").await;
        terminal.send(b"\x04");
        terminal.finish().await;
        assert!(
            !terminal
                .output
                .lock()
                .unwrap()
                .windows(b"never-in-scene-secret".len())
                .any(|bytes| bytes == b"never-in-scene-secret")
        );
        assert!(
            !terminal
                .output
                .lock()
                .unwrap()
                .windows(b"rejected-fixture-key".len())
                .any(|bytes| bytes == b"rejected-fixture-key")
        );
        assert_eq!(rejections.load(std::sync::atomic::Ordering::SeqCst), 1);
        provider.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_clipboard_helper_does_not_block_help_input() {
    use std::os::unix::fs::PermissionsExt as _;
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    fixture.assert_success(&[
        "--profile",
        "test-headless",
        "seed",
        "--session-id",
        "clipboard-history",
    ]);
    std::fs::remove_file(fixture.temporary.path().join("config/rsi/settings.json")).unwrap();
    let helpers = fixture.temporary.path().join("clipboard-helpers");
    std::fs::create_dir(&helpers).unwrap();
    let entered = helpers.join("entered");
    let gate = helpers.join("gate");
    assert!(
        Command::new("mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success()
    );
    let helper = helpers.join("wl-copy");
    std::fs::write(&helper, "#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s' \"$$\" >\"$RSI_CLIPBOARD_ENTERED\"\nread value <\"$RSI_CLIPBOARD_GATE\"\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut terminal = TerminalClient::launch_environment(
        &fixture,
        &[],
        &["tui"],
        &[
            ("PATH", helpers.to_str()),
            ("RSI_CLIPBOARD_ENTERED", entered.to_str()),
            ("RSI_CLIPBOARD_GATE", gate.to_str()),
        ],
    );
    terminal.until("No session attached").await;
    terminal.send(b"/resume\r");
    terminal.until("clipboard-history").await;
    terminal.send(b"\x19");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    terminal.send(b"\x1b");
    terminal.absent("clipboard-history").await;
    terminal.send(b"/help\r");
    terminal.until("RSI · Help").await;
    let pid = std::fs::read_to_string(&entered).unwrap();
    let active = Command::new("kill")
        .args(["-0", &pid])
        .status()
        .unwrap()
        .success();
    terminal.send(b"\x03");
    terminal.absent("RSI · Help").await;
    terminal.until("No session attached").await;
    terminal.send(b"\x03");
    terminal.finish().await;
    provider.abort();
    assert!(
        active,
        "Help must render while the clipboard helper is still blocked on its explicit gate"
    );
}
