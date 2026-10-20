use super::*;
mod browser;
use std::time::Duration;

fn fixture_bundle(mode: &str) -> (tempfile::TempDir, crate::dist::BuiltApplication) {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("bundle");
    std::fs::create_dir(&bundle).unwrap();
    std::fs::create_dir(bundle.join("assets")).unwrap();
    let program = bundle.join(if cfg!(windows) { "rsi.exe" } else { "rsi" });
    let output = std::process::Command::new("rustc")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/development-peer.rs"))
        .args(["--edition=2024", "--crate-name", "development_peer", "-o"])
        .arg(program)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(bundle.join("mode"), mode).unwrap();
    (
        directory,
        crate::dist::BuiltApplication {
            bundle,
            family: "a".repeat(64),
        },
    )
}

#[tokio::test]
async fn watched_frontend_preflights_the_repository_manager_before_build_output() {
    let root = tempfile::tempdir().unwrap();
    let options = Options::parse(&["web".into()]).unwrap();
    let error = start(root.path().into(), root.path().into(), options)
        .await
        .unwrap_err();
    assert!(error.contains("setup web"));
    assert!(!root.path().join("target/rsi-dev").exists());
}

async fn created(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture readiness deadline");
}

#[tokio::test]
async fn interrupt_exit_is_classified_without_a_console_listener_race() {
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(0xc000_013a)
    };
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt as _;
        std::process::ExitStatus::from_raw(130 << 8)
    };
    let mut session = Session::new();
    session
        .children
        .spawn(async move { Ok(("Vite".into(), Some(status))) });
    let error = session
        .guard(
            std::future::pending::<Result<(), String>>(),
            &mut Signals::new().unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, INTERRUPTED);
    session.shutdown().await.unwrap();
}

async fn no_watch_case(mode: &'static str, success: bool) {
    let (directory, artifact) = fixture_bundle(mode);
    let mut signals = Signals::new().unwrap();
    let request = signals.test_request();
    let bundle = artifact.bundle.clone();
    let stopping = if success {
        Some(tokio::spawn(async move {
            created(&bundle.join("launched")).await;
            if mode == "descendant" {
                created(&bundle.join("heartbeat")).await;
            }
            request.cancel();
        }))
    } else {
        None
    };
    let options = Options::parse(&[
        "web".into(),
        "--port".into(),
        "0".into(),
        "--no-watch".into(),
        "--no-open".into(),
    ])
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        web(
            directory.path(),
            directory.path(),
            directory.path(),
            &"b".repeat(32),
            &artifact,
            &options,
            &mut signals,
        ),
    )
    .await
    .expect("ordinary runtime cleanup deadline");
    if let Some(stopping) = stopping {
        stopping.await.unwrap();
    }
    assert_eq!(result.is_ok(), success, "{result:?}");
    if success {
        assert_eq!(
            std::fs::read_to_string(artifact.bundle.join("closed")).unwrap(),
            "eof"
        );
    }
    if mode == "descendant" {
        let before = std::fs::read(artifact.bundle.join("heartbeat")).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            before,
            std::fs::read(artifact.bundle.join("heartbeat")).unwrap()
        );
    }
    if success {
        remove_owned_run(directory.path(), &artifact.bundle).unwrap();
        assert!(!artifact.bundle.exists());
    } else {
        assert!(artifact.bundle.exists());
    }
}

#[tokio::test]
async fn ordinary_no_watch_handshake_and_eof_shutdown() {
    no_watch_case("normal", true).await;
}
#[tokio::test]
async fn ordinary_runtime_rejects_wrong_order_and_settles_its_peer() {
    no_watch_case("wrong-order", false).await;
}
#[tokio::test]
async fn ordinary_runtime_reports_unexpected_child_exit() {
    no_watch_case("unexpected-exit", false).await;
}
#[tokio::test]
async fn ordinary_runtime_reaps_descendants_after_leader_exit() {
    no_watch_case("descendant", true).await;
}

#[tokio::test]
async fn watcher_finishes_its_eof_work_before_signals() {
    let (directory, artifact) = fixture_bundle("normal");
    let marker = directory.path().join("watcher-closed");
    let mut command = Command::new(binary(&artifact));
    command.arg("--watcher").arg(&marker).stdin(Stdio::piped());
    let mut child = Child::spawn_development(command).await.unwrap();
    let mut session = Session::new();
    session
        .watcher_inputs
        .push(child.process.stdin.take().unwrap());
    session.own(child, "renderer");
    session.shutdown().await.unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "eof");
}

#[tokio::test]
async fn forced_but_confirmed_settlement_succeeds() {
    let (_directory, artifact) = fixture_bundle("normal");
    let mut command = Command::new(binary(&artifact));
    command.arg("--hang").stdin(Stdio::null());
    let mut child = Child::spawn_development(command).await.unwrap();
    let status = child
        .stop_after_eof_with_timeout(Duration::from_millis(50))
        .await
        .unwrap();
    assert!(!status.success());
    assert!(child.process.try_wait().unwrap().is_some());
}

#[test]
fn ordinary_product_runtime_fixture() {
    let Some(directory) = std::env::var_os("RSI_TEST_DEV_RUNTIME_CHILD") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let bundle = PathBuf::from(std::env::var_os("RSI_TEST_DEV_BUNDLE").unwrap());
    assert!(bundle.is_absolute());
    let receipt = bundle.join("receipt.json");
    assert!(std::fs::metadata(&receipt).unwrap().len() <= 1024 * 1024);
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
    assert_eq!(receipt["kind"], "web");
    assert_eq!(receipt["profile"], "debug");
    let artifact = crate::dist::BuiltApplication {
        bundle,
        family: receipt["family_sha256"].as_str().unwrap().into(),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut signals = Signals::new().unwrap();
            let request = signals.test_request();
            // This helper process owns stdin until exit. A plain reader thread
            // lets a startup failure leave the runtime without waiting for EOF.
            let input = std::thread::spawn(move || {
                use std::io::Read as _;
                let mut byte = [0];
                let _ = std::io::stdin().read(&mut byte);
                request.cancel();
            });
            let watching = std::env::var_os("RSI_TEST_DEV_WATCH").is_some_and(|value| value == "1");
            let mut arguments = vec![
                "web".into(),
                "--port".into(),
                "0".into(),
                "--no-open".into(),
            ];
            if !watching {
                arguments.push("--no-watch".into());
            }
            let options = Options::parse(&arguments).unwrap();
            let root = if watching {
                crate::dist::command_path(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../..")
                        .canonicalize()
                        .unwrap(),
                )
            } else {
                directory.clone()
            };
            web(
                &root,
                &directory,
                &directory,
                &"b".repeat(32),
                &artifact,
                &options,
                &mut signals,
            )
            .await
            .unwrap();
            input.join().unwrap();
        });
}

#[tokio::test]
#[ignore = "requires RSI_TEST_DEV_BUNDLE pointing to a paired debug Web publication"]
async fn ordinary_product_runtime_smoke() {
    ordinary_product_smoke(false).await;
}

#[tokio::test]
#[ignore = "requires a paired debug Web publication and prepared live frontend tools"]
async fn ordinary_product_watched_smoke() {
    ordinary_product_smoke(true).await;
}

async fn ordinary_product_smoke(watch: bool) {
    use tokio::io::AsyncBufReadExt as _;
    let bundle =
        PathBuf::from(std::env::var_os("RSI_TEST_DEV_BUNDLE").expect("set RSI_TEST_DEV_BUNDLE"));
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "dev::lifecycle_tests::ordinary_product_runtime_fixture",
            "--nocapture",
        ])
        .env_clear()
        .env("RSI_TEST_DEV_RUNTIME_CHILD", directory.path())
        .env("RSI_TEST_DEV_BUNDLE", bundle)
        .env("RSI_TEST_DEV_WATCH", if watch { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for key in [
        "PATH",
        "Path",
        "SystemRoot",
        "SystemDrive",
        "WINDIR",
        "COMSPEC",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        let path = directory.path().join(key.to_ascii_lowercase());
        std::fs::create_dir(&path).unwrap();
        command.env(key, path);
    }
    let mut child = Child::spawn_development(command).await.unwrap();
    let input = child.process.stdin.take().unwrap();
    let mut output = BufReader::new(child.process.stdout.take().unwrap());
    let result = tokio::time::timeout(STARTUP_TIMEOUT, async {
        loop {
            let mut line = String::new();
            if output.read_line(&mut line).await.unwrap() == 0 {
                return false;
            }
            if let Some((_, url)) = line.split_once("rsi web: ") {
                let (origin, _) = url.trim().split_once('#').expect("one-use launch link");
                browser::assert_public_document(origin);
                if watch {
                    browser::assert_browser_handoff(url.trim());
                }
                return true;
            }
        }
    })
    .await;
    drop(input);
    let status = child.stop_after_eof().await.unwrap();
    if !result.as_ref().is_ok_and(|ready| *ready) || !status.success() {
        eprintln!(
            "product smoke failed; retained {}",
            directory.keep().display()
        );
    }
    assert!(
        result.is_ok_and(|ready| ready),
        "ordinary product did not become ready"
    );
    assert!(
        status.success(),
        "ordinary product cleanup failed: {status}"
    );
}
