#![cfg(any(target_os = "linux", windows))]
#![allow(dead_code, unused_imports)] // Shared isolated CLI fixture supports other native tests.
mod support;
use axum::{
    Router, body::Body, extract::State, http::StatusCode, response::Response, routing::post,
};
use sha2::{Digest as _, Sha256};
use std::{
    process::{Command, Output},
    sync::Arc,
};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Notify;
include!("service_host_cli/fixture.rs");
#[path = "resident_listeners/desktop.rs"]
mod desktop;
#[path = "resident_listeners/frontend.rs"]
mod frontend;
#[path = "resident_listeners/source_build.rs"]
mod source_build;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_clients_share_existing_host_and_last_exit_stops_observation() {
    use std::time::Duration;
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    fixture.assert_success(&["host", "start"]);
    let owner_path = fixture
        .temporary
        .path()
        .join("state/rsi/session-host/owner.json");
    let owner = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&owner_path).unwrap()).unwrap()
    };
    let initial = owner();
    let output = fixture
        .command()
        .args(["web", "--no-open", "--port", "0"])
        .env("DEEPSEEK_API_KEY", "fixture-secret-never-print")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("retains different tool environment or credential fallbacks"));
    assert!(!diagnostic.contains("fixture-secret-never-print"));
    assert!(!diagnostic.contains("DEEPSEEK_API_KEY"));
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    let original = std::fs::read_to_string(&profile).unwrap();
    let change = |generation| {
        std::fs::write(&profile, format!("{original}\n# generation {generation}\n")).unwrap();
    };
    change(1);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(owner()["service_revision"], initial["service_revision"]);
    let mut first = watch_web(&fixture).await;
    let wait_change = |previous: serde_json::Value| {
        let owner_path = owner_path.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let current: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&owner_path).unwrap()).unwrap();
                    if current["service_revision"] != previous["service_revision"] {
                        return current;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("active observer must publish accepted source identity")
        }
    };
    let after_first = wait_change(initial.clone()).await;
    let mut second = watch_web(&fixture).await;
    assert!(first.try_wait().unwrap().is_none());
    assert!(second.try_wait().unwrap().is_none());
    first.kill().await.unwrap();
    change(2);
    let after_second = wait_change(after_first).await;
    second.kill().await.unwrap();
    // Allow native EOF and its retained lease to be observed before the next edit.
    tokio::time::sleep(Duration::from_millis(300)).await;
    change(3);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        owner()["service_revision"],
        after_second["service_revision"]
    );
    let reload = fixture.assert_success(&["host", "reload"]);
    let reply: serde_json::Value = serde_json::from_slice(&reload.stdout).unwrap();
    assert_eq!(reply["status"], "reloaded");
    let after_reload = wait_change(after_second).await;
    assert_eq!(after_reload["pid"], initial["pid"]);
    assert_eq!(after_reload["host_epoch"], initial["host_epoch"]);
    let mut third = watch_web(&fixture).await;
    fixture.assert_success(&["host", "stop"]);
    let status = tokio::time::timeout(Duration::from_secs(8), third.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !status.success(),
        "Host loss terminates the observation attachment"
    );
}

async fn watch_web(fixture: &CliFixture) -> tokio::process::Child {
    let mut child = fixture
        .tokio_command()
        .args(["--watch", "web", "--no-open", "--port", "0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(210), lines.next_line())
        .await
        .unwrap()
        .unwrap();
    assert!(
        line.as_deref()
            .is_some_and(|line| line.starts_with("rsi web: ")),
        "{line:?}\n{}",
        fixture.owner_log()
    );
    child
}

fn install_web(fixture: &CliFixture) {
    let package = fixture.temporary.path().join("package");
    let assets = package.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    let specification: serde_json::Value =
        serde_json::from_str(include_str!("../../../crates/rsi/web-assets/bundle.json")).unwrap();
    let mut bootstrap = serde_json::Map::new();
    for file in specification["bootstrap"].as_array().unwrap() {
        let name = file["name"].as_str().unwrap();
        std::fs::write(assets.join(name), name.as_bytes()).unwrap();
        bootstrap.insert(
            name.into(),
            format!("{:x}", Sha256::digest(name.as_bytes())).into(),
        );
    }
    std::fs::write(
        assets.join("rsi-build.json"),
        serde_json::to_vec(&serde_json::json!({
            "format":1, "family_sha256":"b".repeat(64), "bootstrap":bootstrap,
        }))
        .unwrap(),
    )
    .unwrap();
    let mut renderers = Vec::new();
    for (index, name) in ["standard.js", "file-preview.js"].into_iter().enumerate() {
        std::fs::write(assets.join(name), name).unwrap();
        renderers.push(serde_json::json!({
            "id":format!("fixture.renderer{index}"),"abi":1,"entry":name,
            "files":[{"name":name,"sha256":format!("{:x}",Sha256::digest(name))}],
            "schemas":[{"name":format!("fixture.model{index}"),"version":1}],
            "capabilities":["invoke"],"surfaces":["pane"],
        }));
    }
    std::fs::write(
        assets.join("ui-renderers.json"),
        serde_json::to_vec(&serde_json::json!({"format":1,"renderers":renderers})).unwrap(),
    )
    .unwrap();
    let files: Vec<_> = std::fs::read_dir(&assets)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let bytes = std::fs::read(entry.path()).unwrap();
            serde_json::json!({"path":format!("assets/{}",entry.file_name().to_str().unwrap()),
            "bytes":bytes.len(),"digest":format!("{:x}",Sha256::digest(&bytes)),"executable":false})
        })
        .collect();
    std::fs::write(
        package.join("package.json"),
        serde_json::to_vec(&serde_json::json!({
            "format":2,"name":"web","version":"fixture","target":"portable","dependencies":[],
            "files":files,"exports":{"web":{"kind":"web_assets","path":"assets"}},
            "requires":rsi_serve::supported_protocols(),
        }))
        .unwrap(),
    )
    .unwrap();
    fixture.assert_success(&["plugin", "install", "--local", package.to_str().unwrap()]);
}
async fn web(fixture: &CliFixture, port: bool) -> (String, String) {
    let mut command = fixture.tokio_command();
    if port {
        command.args(["web", "--no-open"]);
    } else {
        command.args(["run", "web", "--", "--no-open"]);
    }
    if port {
        command.args(["--port", "0"]);
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(210), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        fixture.owner_log()
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let (origin, ticket) = text
        .trim()
        .strip_prefix("rsi web: ")
        .unwrap()
        .split_once("#rsi-launch=")
        .unwrap();
    (origin.into(), ticket.into())
}
async fn bootstrap(origin: &str, header: &str) -> String {
    let address = origin.strip_prefix("http://").unwrap();
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("POST /api/v1/browser-bootstrap HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nX-Rsi-Csrf: 1\r\nContent-Length: 0\r\nConnection: close\r\n{header}\r\n\r\n").as_bytes()).await.unwrap();
    let mut reply = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_string(&mut reply),
    )
    .await
    .unwrap()
    .unwrap();
    reply
}
async fn serve(fixture: &CliFixture) -> (tokio::process::Child, String) {
    let mut child = fixture
        .tokio_command()
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--origin",
            "http://127.0.0.1",
            "--dev-http",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(210), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap_or_else(|| panic!("Serve failed: {}", fixture.owner_log()));
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    (child, value["bind"].as_str().unwrap().into())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One owned Host spans ticket admission, stop and relaunch.
async fn ordinary_product_runtime_smoke() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    install_web(&fixture);
    let (origin, first) = web(&fixture, true).await;
    let response = bootstrap(&origin, &format!("X-Rsi-Launch-Ticket: {first}")).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let cookie = response
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let owner_before: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .temporary
                .path()
                .join("state/rsi/session-host/owner.json"),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        fixture
            .temporary
            .path()
            .join("config/rsi/host-profiles/fixture/host.profile.toml"),
        "invalid Profile bytes",
    )
    .unwrap();
    let (again, second) = web(&fixture, false).await;
    assert_eq!(again, origin);
    assert_ne!(first, second);
    assert!(
        bootstrap(&origin, &format!("Cookie: {cookie}"))
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert!(
        bootstrap(&origin, &format!("X-Rsi-Launch-Ticket: {first}"))
            .await
            .starts_with("HTTP/1.1 401")
    );
    assert!(
        bootstrap(&origin, &format!("X-Rsi-Launch-Ticket: {second}"))
            .await
            .starts_with("HTTP/1.1 200")
    );
    let (mut one, first_address) = serve(&fixture).await;
    let (mut two, second_address) = serve(&fixture).await;
    one.kill().await.unwrap();
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(&first_address)
            .await
            .is_err()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        tokio::net::TcpStream::connect(&first_address)
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect(&second_address)
            .await
            .is_ok()
    );
    let launch_configuration =
        std::fs::read(fixture.temporary.path().join("config/rsi/rsi.toml")).unwrap();
    std::fs::write(
        fixture.temporary.path().join("config/rsi/rsi.toml"),
        "broken = [",
    )
    .unwrap();
    fixture.assert_success(&["web", "stop"]);
    assert!(
        tokio::net::TcpStream::connect(origin.strip_prefix("http://").unwrap())
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect(&second_address)
            .await
            .is_ok()
    );
    fixture.assert_success(&["host", "status"]);
    let owner_after: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .temporary
                .path()
                .join("state/rsi/session-host/owner.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(owner_before["pid"], owner_after["pid"]);
    assert_eq!(owner_before["host_epoch"], owner_after["host_epoch"]);
    std::fs::write(
        fixture.temporary.path().join("config/rsi/rsi.toml"),
        launch_configuration,
    )
    .unwrap();
    let status = fixture.assert_success(&["web", "status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("stopped"));
    let (restarted, ticket) = web(&fixture, true).await;
    assert!(
        bootstrap(&restarted, &format!("X-Rsi-Launch-Ticket: {ticket}"))
            .await
            .starts_with("HTTP/1.1 200")
    );
    fixture.assert_success(&["web", "stop"]);
    two.kill().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_renderer_refresh_preserves_listeners_and_bootstrap_changes_require_restart() {
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    let (origin, _) = web(&fixture, true).await;
    let owner: rsi_service_host::HostOwnerMetadata = serde_json::from_slice(
        &std::fs::read(
            fixture
                .temporary
                .path()
                .join("state/rsi/session-host/owner.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let (desktop, ready) = rsi_serve::request_listener(
        &owner,
        rsi_web_control::Command::Desktop {
            assets: rsi_web_control::Assets::default(),
        },
    )
    .await
    .unwrap();
    let rsi_web_control::Reply::Ready { address, .. } = ready else {
        panic!("Desktop listener");
    };
    let package = fixture.temporary.path().join("package");
    let assets = package.join("assets");
    let publish = || {
        let path = package.join("package.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for file in manifest["files"].as_array_mut().unwrap() {
            let bytes = std::fs::read(package.join(file["path"].as_str().unwrap())).unwrap();
            file["bytes"] = bytes.len().into();
            file["digest"] = format!("{:x}", Sha256::digest(&bytes)).into();
        }
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        fixture.assert_success(&["plugin", "install", "--local", package.to_str().unwrap()]);
    };
    let refreshed = async || {
        rsi_serve::request_listener(
            &owner,
            rsi_web_control::Command::Refresh {
                package: "web".into(),
            },
        )
        .await
        .unwrap()
        .1
    };
    std::fs::write(assets.join("standard.js"), "new renderer").unwrap();
    let path = assets.join("ui-renderers.json");
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    catalog["renderers"][0]["files"][0]["sha256"] =
        format!("{:x}", Sha256::digest("new renderer")).into();
    std::fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    publish();
    assert!(matches!(
        refreshed().await,
        rsi_web_control::Reply::Refreshed {
            restart_required: false
        }
    ));
    assert!(tokio::net::TcpStream::connect(address).await.is_ok());
    assert_eq!(web(&fixture, false).await.0, origin);
    std::fs::write(assets.join("index.html"), "new document bootstrap").unwrap();
    let path = assets.join("rsi-build.json");
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    receipt["bootstrap"]["index.html"] =
        format!("{:x}", Sha256::digest("new document bootstrap")).into();
    std::fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    publish();
    assert!(matches!(
        refreshed().await,
        rsi_web_control::Reply::Refreshed {
            restart_required: true
        }
    ));
    assert!(
        tokio::net::TcpStream::connect(address).await.is_ok(),
        "old good Desktop listener remains"
    );
    assert_eq!(web(&fixture, false).await.0, origin);
    drop(desktop);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_admits_independent_assets_and_connection_loss_retires_only_its_listener() {
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    let assets = fixture.temporary.path().join("package/assets");
    let reserve = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reserve.local_addr().unwrap();
    drop(reserve);
    let origin = format!("http://{address}");
    let arguments = [
        "serve",
        "--bind",
        &address.to_string(),
        "--origin",
        &origin,
        "--dev-http",
        "--assets",
        assets.to_str().unwrap(),
    ];
    let mut child = fixture
        .tokio_command()
        .args(arguments)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(210), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap_or_else(|| panic!("Serve asset admission failed: {}", fixture.owner_log()));
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(ready["event"], "serving");
    assert_eq!(ready["bind"], address.to_string());
    let owner = fixture
        .temporary
        .path()
        .join("state/rsi/session-host/owner.json");
    let identity = std::fs::read(&owner).unwrap();
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{origin}/index.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "index.html");
    let unauthenticated = client
        .post(format!("{origin}/api/v1/connection/describe/1"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);
    child.kill().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while tokio::net::TcpStream::connect(address).await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fixture.assert_success(&["host", "status"]);
    assert_eq!(std::fs::read(&owner).unwrap(), identity);
    std::fs::write(assets.join("worker.js"), b"corrupt bootstrap").unwrap();
    let failed = fixture.run(&arguments);
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stdout).contains("serving"));
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    assert_eq!(std::fs::read(owner).unwrap(), identity);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_first_browser_ticket_releases_listener_and_allows_retry() {
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    fixture.assert_success(&["host", "start"]);
    let owner_path = fixture
        .temporary
        .path()
        .join("state/rsi/session-host/owner.json");
    let owner: rsi_service_host::HostOwnerMetadata =
        serde_json::from_slice(&std::fs::read(owner_path).unwrap()).unwrap();
    let database =
        rusqlite::Connection::open(fixture.temporary.path().join("state/rsi/base.sqlite3"))
            .unwrap();
    database.execute_batch("CREATE TRIGGER fixture_reject_ticket BEFORE INSERT ON rsi_storage_records BEGIN SELECT RAISE(FAIL, 'fixture ticket failure'); END;").unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let command = rsi_web_control::Command::Launch {
        port: Some(address.port()),
        assets: None,
    };
    let Err(failure) = rsi_serve::request_listener(&owner, command.clone()).await else {
        panic!("ticket persistence failure must reject the launch");
    };
    assert!(
        failure.contains("API operation is unavailable"),
        "{failure}"
    );
    assert!(matches!(
        rsi_serve::request_listener(&owner, rsi_web_control::Command::Status {})
            .await
            .unwrap()
            .1,
        rsi_web_control::Reply::Stopped {}
    ));
    let released =
        std::net::TcpListener::bind(address).expect("failed ticket must retire the bound socket");
    drop(released);
    database
        .execute_batch("DROP TRIGGER fixture_reject_ticket;")
        .unwrap();
    let reply = rsi_serve::request_listener(&owner, command)
        .await
        .unwrap()
        .1;
    assert!(
        matches!(reply, rsi_web_control::Reply::Ready { .. }),
        "{reply:?}"
    );
    fixture.assert_success(&["host", "stop"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leased_listener_capacity_preserves_status_and_stop_commands() {
    use rsi_web_control::{Command as WebCommand, Reply};
    let fixture = CliFixture::new("http://127.0.0.1:9");
    fixture.assert_success(&["host", "start"]);
    let paths = rsi_host::HostPaths::new(
        fixture.temporary.path().join("config/rsi"),
        fixture.temporary.path().join("state/rsi"),
        fixture.temporary.path().join("cache/rsi"),
    )
    .unwrap();
    let owner = rsi_service_host::ServiceHostPaths::from_host_paths(&paths)
        .unwrap()
        .read_metadata()
        .unwrap()
        .unwrap();
    let config = rsi_api_http::HttpConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_origin: "http://127.0.0.1".into(),
        allow_loopback_http: true,
        tls: None,
    };
    let mut leases = Vec::new();
    for _ in 0..16 {
        let (stream, reply) = rsi_serve::request_listener(
            &owner,
            WebCommand::Serve {
                config: config.clone(),
                assets: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(reply, Reply::Ready { .. }));
        leases.push(stream);
    }
    let failure = rsi_serve::request_listener(
        &owner,
        WebCommand::Serve {
            config,
            assets: None,
        },
    )
    .await;
    assert!(matches!(failure, Err(message) if message.contains("attachment capacity")));
    for command in [WebCommand::Status {}, WebCommand::Stop {}] {
        let (_, reply) = rsi_serve::request_listener(&owner, command).await.unwrap();
        assert!(matches!(reply, Reply::Stopped {}));
    }
    drop(leases);
    let mut watchers = Vec::new();
    for _ in 0..16 {
        watchers.push(rsi::acquire_configuration_watch(&paths).await.unwrap());
    }
    assert!(
        rsi::acquire_configuration_watch(&paths)
            .await
            .unwrap_err()
            .to_string()
            .contains("lease capacity")
    );
    assert_eq!(
        serde_json::to_value(rsi::reload_service_host(&paths).await.unwrap()).unwrap()["status"],
        "reloaded"
    );
    drop(watchers);
    fixture.assert_success(&["host", "stop"]);
}
