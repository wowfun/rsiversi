#![cfg(all(target_os = "linux", feature = "test-support"))]
use rsi_browser::{
    Assertion, BrowserPolicy, CheckOutcome, CheckSpec, NativeRuntime, RuntimeConfig,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::json;
use std::{
    collections::BTreeSet,
    io::BufRead,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
};
struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "explicit Linux Node/Chromium/systemd/Bubblewrap acceptance"]
#[expect(
    clippy::too_many_lines,
    reason = "Keep one complete ownership operation or acceptance scenario together"
)]
async fn native_preview_check_and_retirement() {
    let node = std::env::var("RSI_TEST_BROWSER_NODE").expect("explicit Node path");
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let requests_file = tmp.path().join("requests.json");
    let mut fixture = Fixture(
        Command::new(&node)
            .arg(assets.join("server.mjs"))
            .arg(&requests_file)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    std::io::BufReader::new(fixture.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port = port.trim().parse().unwrap();
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "process",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_process_local::ProcessLocalFactory),
            ),
            json!({}),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "sandbox",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
            ),
            json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]}),
        )
        .await
        .unwrap();
    let mut config = RuntimeConfig {
        node: node.into(),
        chromium_directory: std::env::var("RSI_TEST_BROWSER_CHROMIUM").unwrap().into(),
        package_directory: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime"),
        systemd_run: "/usr/bin/systemd-run".into(),
        user_runtime_directory: std::env::var("RSI_TEST_BROWSER_USER_RUNTIME")
            .unwrap()
            .into(),
        artifact_digest: "0".repeat(64),
    };
    config.artifact_digest = config.digest().unwrap();
    let browser = NativeRuntime::new(
        config,
        runtime
            .root()
            .lookup_local::<rsi_process::DuplexProcessContract>()
            .unwrap(),
        runtime
            .root()
            .lookup_local::<rsi_sandbox::SandboxContract>()
            .unwrap(),
    )
    .unwrap()
    .with_fixture_network(port);
    browser.prepare().await.unwrap();
    let policy = BrowserPolicy {
        entry_url: "https://preview.fixture.invalid/dialog/".into(),
        path_prefix: "/".into(),
        dependency_hosts: BTreeSet::new(),
    };
    let abandoned = browser
        .open(
            policy.clone(),
            &format!("dropped-native-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let weak = Arc::downgrade(&abandoned);
    drop(abandoned);
    assert!(
        weak.upgrade().is_none(),
        "private pumps must not retain the last public scope owner"
    );
    let scope = browser
        .open(
            policy,
            &format!("native-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    // Retire the owner before assertions, including first-failure evidence.
    let pass = scope
        .check(CheckSpec {
            entry_identity: "Deployment fixture".into(),
            assertions: vec![
                Assertion::TextVisible {
                    text: "Visible acceptance evidence".into(),
                },
                Assertion::RoleVisible {
                    role: "link".into(),
                    name: "Inspect next page".into(),
                },
            ],
        })
        .await;
    let fail = scope
        .check(CheckSpec {
            entry_identity: "Deployment fixture".into(),
            assertions: vec![Assertion::TextVisible {
                text: "Hidden acceptance text".into(),
            }],
        })
        .await;
    let unavailable = scope
        .check(CheckSpec {
            entry_identity: "Wrong deployment".into(),
            assertions: vec![Assertion::TextVisible {
                text: "Visible acceptance evidence".into(),
            }],
        })
        .await;
    let blocked = scope.navigate("https://localhost/").await;
    let observed = scope.observe().await;
    let closed = scope.close().await;
    drop(scope);
    let explorer = browser
        .open_exploration(
            BrowserPolicy {
                entry_url: "https://preview.fixture.invalid/".into(),
                path_prefix: "/".into(),
                dependency_hosts: BTreeSet::new(),
            },
            &format!("mcp-native-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let navigation = explorer
        .navigate("https://preview.fixture.invalid/dialog/")
        .await;
    let snapshot = explorer.observe().await;
    let exploration_blocked = explorer.navigate("https://localhost/").await;
    let retired = explorer.close().await;
    drop(explorer);
    let ssrf = browser
        .open(
            BrowserPolicy {
                entry_url: "https://preview.fixture.invalid/ssrf/".into(),
                path_prefix: "/".into(),
                dependency_hosts: BTreeSet::new(),
            },
            &format!("ssrf-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let ssrf_result = ssrf
        .check(CheckSpec {
            entry_identity: "Deployment fixture".into(),
            assertions: vec![Assertion::TextVisible {
                text: "Forbidden fetch settled".into(),
            }],
        })
        .await;
    ssrf.close().await.unwrap();
    drop(ssrf);
    let redirect = browser
        .open(
            BrowserPolicy {
                entry_url: "https://preview.fixture.invalid/scope/redirect/".into(),
                path_prefix: "/scope/".into(),
                dependency_hosts: BTreeSet::new(),
            },
            &format!("redirect-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let redirected = redirect
        .check(CheckSpec {
            entry_identity: "Deployment fixture".into(),
            assertions: vec![Assertion::TextVisible {
                text: "Visible acceptance evidence".into(),
            }],
        })
        .await;
    redirect.close().await.unwrap();
    drop(redirect);
    let escaped_explorer = browser
        .open_exploration(
            BrowserPolicy {
                entry_url: "https://preview.fixture.invalid/scope/redirect/".into(),
                path_prefix: "/scope/".into(),
                dependency_hosts: BTreeSet::new(),
            },
            &format!("escaped-mcp-native-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let readiness_peer = browser
        .open(
            BrowserPolicy {
                entry_url: "https://preview.fixture.invalid/".into(),
                path_prefix: "/".into(),
                dependency_hosts: BTreeSet::new(),
            },
            &format!("readiness-peer-{}", std::process::id()),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    browser.prepare().await.unwrap();
    assert!(browser.is_verified());
    readiness_peer.close().await.unwrap();
    drop(readiness_peer);
    let escaped_text = escaped_explorer
        .navigate("https://preview.fixture.invalid/scope/redirect/")
        .await;
    escaped_explorer.close().await.unwrap();
    assert!(
        escaped_text.is_err(),
        "a structured escaped URL must reject spoofed in-policy page text"
    );
    assert!(runtime.shutdown().await.is_clean());
    let checked = ssrf_result.unwrap().0;
    assert_eq!(checked.outcome, CheckOutcome::Pass, "{checked:?}");
    let hits: Vec<String> = serde_json::from_slice(&std::fs::read(requests_file).unwrap()).unwrap();
    assert!(
        !hits.iter().any(|u| u == "/secret"),
        "forbidden destination reached fixture backend"
    );
    let (blocked_result, images) = redirected.unwrap();
    assert_eq!(blocked_result.outcome, CheckOutcome::PolicyBlocked);
    assert!(blocked_result.snapshot.is_empty());
    assert!(blocked_result.assertions.is_empty());
    assert!(images.is_empty());
    closed.unwrap();
    let (result, artifacts) = pass.unwrap();
    assert_eq!(result.outcome, CheckOutcome::Pass, "{result:?}");
    assert_eq!(artifacts.len(), 1);
    assert!(result.dialogs_dismissed >= 1);
    if let Ok(path) = std::env::var("RSI_TEST_BROWSER_SCREENSHOT") {
        std::fs::write(path, &artifacts[0]).unwrap();
    }
    assert_eq!(fail.unwrap().0.outcome, CheckOutcome::AssertionFailed);
    assert_eq!(
        unavailable.unwrap().0.outcome,
        CheckOutcome::TargetUnavailable
    );
    assert!(blocked.is_err());
    assert!(observed.unwrap().contains("Deployment fixture"));
    retired.unwrap();
    let navigation = navigation.unwrap();
    let snapshot = snapshot.unwrap();
    assert!(navigation.contains("Deployment fixture"), "{navigation}");
    assert!(snapshot.contains("Deployment fixture"), "{snapshot}");
    assert!(exploration_blocked.is_err());
}
