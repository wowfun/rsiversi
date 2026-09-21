use super::*;
use rsi_acp::{Peer, PeerHandle, StreamTransport};
use rsi_acp_protocol::Message;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn profile(fixture: &CliFixture) {
    let profile = fixture
        .temporary
        .path()
        .join("config/rsi/application-profiles/test-acp/application.profile.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(profile, "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"service\"\nplugin = \"rsi.application.acp-service\"\nconfig = { host_profile = \"fixture\" }\n[[steps]]\nkind = \"plugin\"\nid = \"application\"\nplugin = \"rsi.application.acp\"\n").unwrap();
}

async fn request(port: &PeerHandle, method: &str, params: Value) -> Value {
    tokio::time::timeout(
        Duration::from_secs(40),
        port.request(method, &params, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .result()
    .unwrap()
    .clone()
}

async fn update(peer: &mut Peer) -> Value {
    let event = tokio::time::timeout(Duration::from_secs(10), peer.next())
        .await
        .unwrap()
        .unwrap();
    let Message::Notification { method, params } = event.message else {
        panic!("expected update")
    };
    assert_eq!(method, "session/update");
    params["update"].clone()
}

async fn start(fixture: &CliFixture) -> (tokio::process::Child, Peer) {
    let mut child = fixture
        .tokio_command()
        .args(["--profile", "test-acp"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let peer = Peer::start(StreamTransport::new(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
    ));
    request(
        &peer.handle(),
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    )
    .await;
    (child, peer)
}

async fn stop(fixture: &CliFixture, peer: Peer, child: tokio::process::Child) {
    peer.close().await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(40), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        fixture.owner_log()
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-mcp-secret-fixture"));
}

async fn mcp_chat(
    State(requests): State<Arc<std::sync::Mutex<Vec<Value>>>>,
    axum::Json(request): axum::Json<Value>,
) -> Response {
    let has_tool_result = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool");
    let name = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|tool| {
            tool["function"]["name"]
                .as_str()
                .filter(|name| name.starts_with("mcp__"))
        })
        .unwrap()
        .to_owned();
    requests.lock().unwrap().push(request);
    if has_tool_result {
        return chat().await;
    }
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"acp-mcp-tool","type":"function","function":{"name":name,"arguments":"{\"message\":\"private scope executed\"}"}},{"index":1,"id":"acp-mcp-second","type":"function","function":{"name":name,"arguments":"{\"message\":\"second scope executed\"}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3}})
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

fn contains_secret(path: &std::path::Path) -> bool {
    std::fs::read_dir(path).unwrap().any(|entry| {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            contains_secret(&entry.path())
        } else {
            kind.is_file()
                && std::fs::read(entry.path())
                    .unwrap()
                    .windows(b"private-mcp-secret-fixture".len())
                    .any(|bytes| bytes == b"private-mcp-secret-fixture")
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_permission_preserves_peer_and_accepts_late_reply() {
    let (endpoint, state, provider) = gated_provider("bash").await;
    let fixture = CliFixture::new(&endpoint);
    profile(&fixture);
    fixture.require_approval();
    state.release.notify_one();
    let (child, mut peer) = start(&fixture).await;
    let port = peer.handle();
    let session = request(
        &port,
        "session/new",
        json!({"cwd":fixture.workspace,"mcpServers":[]}),
    )
    .await;
    let parameters = json!({"sessionId":session["sessionId"],"prompt":[{"type":"text","text":"run the command"}]});
    let prompt = port.request("session/prompt", &parameters, CancellationToken::new());
    tokio::pin!(prompt);
    let permission_id = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::select! {
                result = &mut prompt => panic!("prompt ended before permission: {result:?}"),
                event = peer.next() => {
                    if let Message::Request { id, method, .. } = event.unwrap().message {
                        assert_eq!(method, "session/request_permission");
                        break id;
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    port.notify("session/cancel", &json!({"sessionId":session["sessionId"]}))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(35), prompt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.result().unwrap()["stopReason"], "cancelled");
    port.respond(
        &permission_id,
        Ok(&json!({"outcome":{"outcome":"cancelled"}})),
    )
    .await
    .unwrap();
    request(&port, "session/list", json!({})).await;
    request(
        &port,
        "session/close",
        json!({"sessionId":session["sessionId"]}),
    )
    .await;
    stop(&fixture, peer, child).await;
    provider.abort();
    let _ = provider.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_during_private_mcp_prepare_reaps_unpublished_process() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    profile(&fixture);
    let marker = fixture.workspace.join("preparing.pid");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/rsi/acp/mcp.py");
    let servers = json!([{"name":"pending","command":"/usr/bin/python3","args":[script,marker,"stall"],"env":[{"name":"ACP_FIXTURE_SECRET","value":"private-mcp-secret-fixture"},{"name":"ACP_FIXTURE_EMPTY","value":""}]}]);
    let (child, peer) = start(&fixture).await;
    let port = peer.handle();
    let cwd = fixture.workspace.clone();
    let request = tokio::spawn(async move {
        port.request(
            "session/new",
            &json!({"cwd":cwd,"mcpServers":servers}),
            CancellationToken::new(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(marker).unwrap();
    peer.close().await.unwrap();
    assert!(request.await.unwrap().is_err());
    let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_mcp_executes_with_permission_and_cold_restore_requires_exact_manifest() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = CliFixture::new(&format!("http://{}", listener.local_addr().unwrap()));
    let router = Router::new()
        .route("/v1/chat/completions", post(mcp_chat))
        .with_state(requests.clone());
    let provider = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
    profile(&fixture);
    fixture.require_approval();
    let marker = fixture.workspace.join("mcp.pid");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/rsi/acp/mcp.py");
    let servers = json!([{"name":"private","command":"/usr/bin/python3","args":[script,marker],"env":[{"name":"ACP_FIXTURE_SECRET","value":"private-mcp-secret-fixture"},{"name":"ACP_FIXTURE_EMPTY","value":""}]}]);
    let (child, mut peer) = start(&fixture).await;
    let port = peer.handle();
    let created = request(
        &port,
        "session/new",
        json!({"cwd":fixture.workspace,"mcpServers":servers}),
    )
    .await;
    let id = created["sessionId"].clone();
    let pid = std::fs::read_to_string(&marker).unwrap();
    let parameters =
        json!({"sessionId":id,"prompt":[{"type":"text","text":"call the supplied echo tool"}]});
    let pending = request(&port, "session/prompt", parameters);
    tokio::pin!(pending);
    let mut permissions = 0;
    let mut updates = Vec::new();
    loop {
        tokio::select! {
            result = &mut pending => { assert_eq!(result["stopReason"], "end_turn"); break; }
            event = peer.next() => {
                let event = event.unwrap();
                match event.message {
                    Message::Request { id, method, params } => {
                        assert_eq!(method, "session/request_permission");
                        assert_eq!(params["options"].as_array().unwrap().len(), 2);
                        assert_eq!(params["options"][0]["kind"], "allow_once");
                        permissions += 1;
                        port.respond(&id, Ok(&json!({"outcome":{"outcome":"selected","optionId":"allow-once"}}))).await.unwrap();
                    }
                    Message::Notification { params, .. } => updates.push(params),
                    Message::Response { .. } => panic!("unexpected response"),
                }
            }
        }
    }
    assert_eq!(permissions, 2);
    assert!(
        updates
            .iter()
            .any(|update| update.to_string().contains("private scope executed")),
        "{updates:?}"
    );
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert!(
        !captured[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "ask_user")
    );
    assert!(
        captured[1]["messages"]
            .to_string()
            .contains("private scope executed")
    );
    request(&port, "session/close", json!({"sessionId":id})).await;
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    stop(&fixture, peer, child).await;
    let (child, peer) = start(&fixture).await;
    let port = peer.handle();
    let rejected = port
        .request(
            "session/resume",
            &json!({"sessionId":id,"cwd":fixture.workspace,"mcpServers":[]}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.result().unwrap_err()["code"], -32602);
    request(
        &port,
        "session/resume",
        json!({"sessionId":id,"cwd":fixture.workspace,"mcpServers":servers}),
    )
    .await;
    request(&port, "session/close", json!({"sessionId":id})).await;
    stop(&fixture, peer, child).await;
    assert!(!contains_secret(&fixture.temporary.path().join("state")));
    assert!(!contains_secret(&fixture.temporary.path().join("config")));
    provider.abort();
    let _ = provider.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdio_application_closes_drafts_and_replays_owned_native_sessions() {
    let (endpoint, provider) = provider().await;
    let fixture = CliFixture::new(&endpoint);
    profile(&fixture);
    let mut child = fixture
        .tokio_command()
        .args(["--profile", "test-acp"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut peer = Peer::start(StreamTransport::new(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
    ));
    let port = peer.handle();
    let initialized = request(
        &port,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    )
    .await;
    assert_eq!(initialized["protocolVersion"], 1);
    assert_eq!(initialized["agentCapabilities"]["loadSession"], true);
    let setup = json!({"cwd":fixture.workspace,"mcpServers":[]});
    let draft = request(&port, "session/new", setup.clone()).await;
    request(
        &port,
        "session/close",
        json!({"sessionId":draft["sessionId"]}),
    )
    .await;
    let created = request(&port, "session/new", setup.clone()).await;
    let id = created["sessionId"].clone();
    let result = request(
        &port,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"stdio round trip"}]}),
    )
    .await;
    assert_eq!(result["stopReason"], "end_turn");
    let message = update(&mut peer).await;
    assert_eq!(message["sessionUpdate"], "agent_message_chunk");
    assert_eq!(message["content"]["text"], "hello from daemon");
    let restore = json!({"cwd":fixture.workspace,"mcpServers":[],"sessionId":id});
    request(&port, "session/load", restore.clone()).await;
    let user = update(&mut peer).await;
    let assistant = update(&mut peer).await;
    assert_eq!(user["sessionUpdate"], "user_message_chunk");
    assert_eq!(user["content"]["text"], "stdio round trip");
    assert_eq!(assistant["content"]["text"], "hello from daemon");
    request(&port, "session/resume", restore).await;
    // The following response is a wire barrier after resume: no replay may precede it.
    let listed = request(&port, "session/list", json!({})).await;
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(listed["sessions"][0]["sessionId"], id);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), peer.next())
            .await
            .is_err()
    );
    request(&port, "session/close", json!({"sessionId":id})).await;
    peer.close().await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(40), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        fixture.owner_log()
    );
    provider.abort();
    let _ = provider.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_stdio_accepts_regular_file_eof_and_redirected_protocol_output() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    profile(&fixture);
    let input = fixture.temporary.path().join("empty-input");
    std::fs::write(&input, []).unwrap();
    let exited = tokio::time::timeout(
        Duration::from_secs(30),
        fixture
            .tokio_command()
            .args(["--profile", "test-acp"])
            .stdin(std::fs::File::open(input).unwrap())
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        exited.status.success(),
        "regular stdin EOF: {}",
        String::from_utf8_lossy(&exited.stderr)
    );
    assert!(exited.stdout.is_empty());
    let path = fixture.temporary.path().join("protocol-output");
    let mut child = fixture
        .tokio_command()
        .args(["--profile", "test-acp"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&path).unwrap())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1,\"clientCapabilities\":{}}}\n").await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let bytes = std::fs::read(&path).unwrap();
            if bytes.ends_with(b"\n") {
                break serde_json::from_slice::<Value>(&bytes).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], 1);
    drop(child.stdin.take());
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
