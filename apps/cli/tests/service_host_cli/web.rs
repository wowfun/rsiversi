use super::*;
#[cfg(feature = "paired-web-tests")]
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};

#[cfg(feature = "paired-web-tests")]
async fn start(fixture: &CliFixture, args: &[&str]) -> (tokio::process::Child, String, String) {
    let mut child = fixture
        .tokio_command()
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
        .await
        .unwrap()
        .unwrap();
    let Some(line) = line else {
        let output = child.wait_with_output().await.unwrap();
        panic!(
            "Web did not become ready: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let link = line.strip_prefix("rsi web: ").expect("readiness prefix");
    let (origin, ticket) = link.split_once("#rsi-launch=").unwrap();
    (child, origin.to_owned(), ticket.to_owned())
}
#[cfg(feature = "paired-web-tests")]
async fn request(origin: &str, path: &str, headers: &str) -> String {
    let address = origin.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(format!("POST {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nX-Rsi-Csrf: 1\r\nContent-Length: 0\r\nConnection: close\r\n{headers}\r\n").as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).await.unwrap();
    String::from_utf8(bytes).unwrap()
}
#[cfg(feature = "paired-web-tests")]
fn identity(response: &str) -> serde_json::Value {
    assert!(response.starts_with("HTTP/1.1 200"), "bootstrap failed");
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[cfg(feature = "paired-web-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_alias_and_profile_serve_default_assets_with_stable_authorized_identity() {
    let mut fixture = CliFixture::new("http://127.0.0.1:1");
    let binary = fixture.temporary.path().join("rsi");
    let paired = std::path::PathBuf::from(
        std::env::var_os("RSI_PAIRED_BUNDLE").expect("paired-web-tests requires RSI_PAIRED_BUNDLE: run pnpm -C apps/web build --debug, then set RSI_PAIRED_BUNDLE to its bundle directory"),
    );
    std::fs::copy(paired.join("rsi"), &binary).unwrap();
    fixture.binary = binary;
    let assets = fixture.temporary.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    for file in std::fs::read_dir(paired.join("assets")).unwrap() {
        let file = file.unwrap();
        std::fs::copy(file.path(), assets.join(file.file_name())).unwrap();
    }
    let mut first = None;
    for args in [
        vec!["web", "--port", "0", "--no-open"],
        vec!["--profile", "web", "--port", "0", "--no-open"],
    ] {
        let (child, origin, ticket) = start(&fixture, &args).await;
        let response = request(
            &origin,
            "/api/v1/browser-bootstrap",
            &format!("X-Rsi-Launch-Ticket: {ticket}\r\n"),
        )
        .await;
        let current = identity(&response);
        if let Some(first) = &first {
            assert_eq!(first, &current);
        } else {
            first = Some(current.clone());
        }
        let cookie = response
            .lines()
            .find_map(|line| line.strip_prefix("set-cookie: "))
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert_eq!(
            identity(
                &request(
                    &origin,
                    "/api/v1/browser-bootstrap",
                    &format!("Cookie: {cookie}\r\n")
                )
                .await
            ),
            current
        );
        assert!(
            request(
                &origin,
                "/api/v1/browser-bootstrap",
                &format!("X-Rsi-Launch-Ticket: {ticket}\r\n")
            )
            .await
            .starts_with("HTTP/1.1 401")
        );
        let grants = fixture.assert_success(&["--profile", "devices", "configuration", "list"]);
        assert!(
            String::from_utf8_lossy(&grants.stdout)
                .contains(current["device_id"].as_str().unwrap())
        );
        let conflict = fixture.run(&["web", "--port", "0", "--no-open"]);
        assert!(!conflict.status.success());
        assert!(!String::from_utf8_lossy(&conflict.stdout).contains("rsi web:"));
        fixture.assert_success(&["host", "stop"]);
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn web_help_and_failures_do_not_announce_readiness() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let help = fixture.assert_success(&["web", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--port"));
    assert!(!help.contains("headless"));
    assert!(!fixture.temporary.path().join("state").exists());
    let missing = fixture.temporary.path().join("missing");
    let failure = fixture.run(&["web", "--no-open", "--assets", missing.to_str().unwrap()]);
    assert!(!failure.status.success());
    assert!(!String::from_utf8_lossy(&failure.stdout).contains("rsi web:"));
    let message = String::from_utf8_lossy(&failure.stderr);
    assert!(message.contains("no build family"), "{message}");
    assert!(message.contains("pnpm -C apps/web build"), "{message}");
    assert!(!fixture.temporary.path().join("state").exists());
    let invalid = fixture.run(&["web", "--port", "invalid"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--port"));
    assert!(!String::from_utf8_lossy(&invalid.stdout).contains("rsi web:"));
}
