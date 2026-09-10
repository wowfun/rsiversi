use super::*;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_host_executor_reload_retains_listener_and_emits_one_final_diagnostic() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    let mut child = fixture
        .tokio_command()
        .args(["host", "serve", "--profile", "fixture"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
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
    for maximum in [3, 2] {
        reload_executor_and_keep_listener(&fixture, &mut child, maximum);
        fixture.assert_success(&["--profile", "devices", "list"]);
    }
    fixture.assert_success(&["host", "stop"]);
    let output = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    let finals: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("Service Host diagnostics final=true"))
        .collect();
    assert_eq!(finals.len(), 1, "{stderr}");
    for line in finals {
        let accepted: u64 = line
            .split_whitespace()
            .find_map(|field| field.strip_prefix("accepted_connections="))
            .unwrap()
            .parse()
            .unwrap();
        assert!(accepted > 0, "{line}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_serve_supports_local_clients_reload_and_exact_host_stop() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = reserved.local_addr().unwrap().to_string();
    let origin = format!("http://{bind}");
    write_web_profile(&fixture);
    drop(reserved);
    let mut child = fixture
        .tokio_command()
        .args([
            "--profile",
            "test-serve",
            "--bind",
            &bind,
            "--origin",
            &origin,
            "--dev-http",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    verify_serve(&fixture, &mut child).await;
    fixture.assert_success(&["host", "stop"]);
    let output = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tokio::net::TcpStream::connect(&bind).await.is_err());
    provider.abort();
    let _ = provider.await;
}

fn write_web_profile(fixture: &CliFixture) {
    let assets = fixture.temporary.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<main>Web asset fixture</main>").unwrap();
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/application-profiles/test-serve/application.profile.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(
        profile,
        format!(
            r#"format = 1
[[steps]]
kind = "plugin"
id = "service"
plugin = "rsi.application.service"
config = {{ host_profile = "fixture" }}
[[steps]]
kind = "plugin"
id = "assets"
plugin = "rsi.web.assets"
config = {{ directory = {}, files = ["index.html"] }}
[[steps]]
kind = "plugin"
id = "http"
plugin = "rsi.application.serve-web"
"#,
            serde_json::to_string(&assets).unwrap()
        ),
    )
    .unwrap();
}

async fn verify_serve(fixture: &CliFixture, child: &mut tokio::process::Child) {
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(ready["event"], "serving");
    let address = ready["bind"].as_str().unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(format!("POST /api/v1/connection/describe/1 HTTP/1.1\r\nHost: {address}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        socket.take(64 * 1024).read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 401"));
    let mut asset_socket = tokio::net::TcpStream::connect(address).await.unwrap();
    asset_socket
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut page = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        asset_socket.take(8192).read_to_string(&mut page),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(page.starts_with("HTTP/1.1 200"));
    assert!(page.ends_with("<main>Web asset fixture</main>"));
    check_device_management(fixture, address, &format!("http://{address}")).await;
    fixture.assert_success(&["host", "status"]);
    fixture.assert_success(&["host", "reload"]);
    reload_executor_and_keep_listener(fixture, child, 3);
    check_device_management(fixture, address, &format!("http://{address}")).await;
    reload_executor_and_keep_listener(fixture, child, 2);
    check_device_management(fixture, address, &format!("http://{address}")).await;
    let mut client = JsonClient::start(fixture, &["--session-id", "after-serve-reload"]);
    client.send("hello through the shared Service Host\n").await;
    client.until(|value| value["type"] == "outcome").await;
    client.finish().await;
}

fn reload_executor_and_keep_listener(
    fixture: &CliFixture,
    child: &mut tokio::process::Child,
    maximum_active_turns: usize,
) {
    use std::os::unix::fs::MetadataExt as _;
    let metadata_path = fixture
        .temporary
        .path()
        .join("state/rsi/session-host/owner.json");
    let owner = std::fs::read(&metadata_path).unwrap();
    let metadata: rsi_service_host::HostOwnerMetadata = serde_json::from_slice(&owner).unwrap();
    let socket = metadata.socket_path.as_ref().unwrap();
    // Hold the old socket inode so the replacement cannot reuse its number.
    let _previous_socket = rustix::fs::open(
        socket,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    let previous_inode = std::fs::metadata(socket).unwrap().ino();
    let path = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    let original = source.split("\n# reload fixture\n").next().unwrap();
    std::fs::write(&path, format!("{original}\n# reload fixture\n[[steps]]\nkind = \"patch\"\ntarget = \"rsi-agent-executor\"\nconfig = {{ executor_id = \"rsi-agent-executor\", maximum_active_turns = {maximum_active_turns} }}\n")).unwrap();
    fixture.assert_success(&["host", "reload"]);
    assert!(
        child.try_wait().unwrap().is_none(),
        "service stopped after executor reload"
    );
    assert_eq!(
        std::fs::metadata(socket).unwrap().ino(),
        previous_inode,
        "executor reload replaced an independent listener"
    );
    fixture.assert_success(&["host", "status"]);
    assert_eq!(
        std::fs::read(metadata_path).unwrap(),
        owner,
        "reload changed process or endpoint identity"
    );
}

async fn request(
    address: &str,
    operation: &str,
    token: &str,
    epoch: Option<&str>,
    body: &str,
) -> (u16, Vec<u8>) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let epoch = epoch.map_or_else(String::new, |epoch| {
        format!("X-Rsi-Host-Epoch: {epoch}\r\n")
    });
    let head = format!(
        "POST /api/v1/{operation}/1 HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nX-Rsi-Wire-Version: 1\r\n{epoch}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket
        .write_all(format!("{head}{body}").as_bytes())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut reader = tokio::io::BufReader::new(socket.take(2 * 1024 * 1024));
        let mut header = String::new();
        while !header.ends_with("\r\n\r\n") {
            assert_ne!(reader.read_line(&mut header).await.unwrap(), 0);
            assert!(header.len() <= 64 * 1024);
        }
        let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        let length: usize = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length <= 2 * 1024 * 1024 - header.len());
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await.unwrap();
        (status, body)
    })
    .await
    .unwrap()
}

async fn check_device_management(fixture: &CliFixture, address: &str, origin: &str) {
    let registered = fixture.assert_success(&["--profile", "devices", "register", "test browser"]);
    let registered: serde_json::Value = serde_json::from_slice(&registered.stdout).unwrap();
    let device_id = registered["id"].as_str().unwrap();
    let token = registered["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    let roster = fixture.assert_success(&["--profile", "devices", "list"]);
    let roster: serde_json::Value = serde_json::from_slice(&roster.stdout).unwrap();
    assert_eq!(roster[0]["id"], device_id);
    assert!(roster[0].get("token").is_none());
    let description = request(
        address,
        "connection/describe",
        token,
        None,
        r#"{"wire_version":1}"#,
    )
    .await;
    assert_eq!(description.0, 200);
    let description: serde_json::Value = serde_json::from_slice(&description.1).unwrap();
    assert_eq!(description["endpoint_id"], registered["endpoint_id"]);
    let epoch = description["host_epoch"].as_str().unwrap();
    let catalog = request(address, "connection/operations", token, Some(epoch), "{}").await;
    assert_eq!(catalog.0, 200);
    let catalog: rsi_api_protocol::OperationCatalog = serde_json::from_slice(&catalog.1).unwrap();
    assert!(
        catalog
            .operations()
            .iter()
            .all(|spec| spec.access == rsi_api_protocol::OperationAccess::Authenticated)
    );
    for operation in [
        "devices/register",
        "devices/list",
        "devices/revoke",
        "inspector/runtime",
        "inspector/profile",
        "inspector/factories",
        "inspector/native",
        "native-addons/refresh",
    ] {
        assert_eq!(
            request(address, operation, token, Some(epoch), "{}")
                .await
                .0,
            401
        );
    }
    run_remote_headless(fixture, origin, &registered).await;
    fixture.assert_success(&["--profile", "devices", "revoke", device_id]);
    assert_eq!(
        request(
            address,
            "connection/describe",
            token,
            None,
            r#"{"wire_version":1}"#
        )
        .await
        .0,
        401
    );
}

async fn run_remote_headless(fixture: &CliFixture, origin: &str, registered: &serde_json::Value) {
    let token = registered["token"].as_str().unwrap();
    let session = format!("through-http-{}", registered["id"].as_str().unwrap());
    let remote_profile = fixture
        .temporary
        .path()
        .join("remote-config/rsi/application-profiles/remote/application.profile.toml");
    std::fs::create_dir_all(remote_profile.parent().unwrap()).unwrap();
    std::fs::write(remote_profile, format!(r#"format = 1
[[steps]]
kind = "plugin"
id = "credentials"
plugin = "rsi.credentials.local"
config = {{ service = "rsi-fixture-{}", environment = [{{ reference = {{ owner = "remote", slot = "device" }}, variable = "RSI_API_DEVICE_TOKEN" }}] }}
[[steps]]
kind = "plugin"
id = "connection"
plugin = "rsi.application.http"
config = {{ origin = "{origin}", endpoint_id = "{}", credential = {{ owner = "remote", slot = "device" }}, allow_loopback_http = true }}
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.headless"
"#, origin.replace([':', '/'], "-"), registered["endpoint_id"].as_str().unwrap())).unwrap();
    let remote_state = fixture.temporary.path().join("remote-state");
    let remote_cache = fixture.temporary.path().join("remote-cache");
    let remote = fixture
        .tokio_command()
        .env(
            "XDG_CONFIG_HOME",
            fixture.temporary.path().join("remote-config"),
        )
        .env("XDG_STATE_HOME", &remote_state)
        .env("XDG_CACHE_HOME", &remote_cache)
        .env("RSI_API_DEVICE_TOKEN", token)
        .args([
            "--profile",
            "remote",
            "hello through native HTTP",
            "--session-id",
            &session,
            "--cwd",
            fixture.workspace.to_str().unwrap(),
            "--trust-workspace",
            "--output",
            "jsonl",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        remote.status.success(),
        "remote stderr: {}",
        String::from_utf8_lossy(&remote.stderr)
    );
    assert!(
        remote
            .stdout
            .windows(b"\"type\":\"outcome\"".len())
            .any(|bytes| bytes == b"\"type\":\"outcome\"")
    );
    assert!(
        !remote_state.exists(),
        "remote client created local backend state"
    );
    assert!(
        !remote_cache.join("rsi/agent-presets").exists(),
        "remote client materialized backend presets"
    );
    let cached = std::fs::read_dir(remote_cache.join("rsi"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(cached, [std::ffi::OsString::from("native-applications")]);
    fixture.assert_success(&["host", "status"]);
}
