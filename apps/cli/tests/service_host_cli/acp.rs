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
    .unwrap_or_else(|error| panic!("ACP {method} failed: {error}"))
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
        .args(["run", "test-acp"])
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
    let initialized = tokio::time::timeout(
        Duration::from_secs(40),
        peer.handle().request(
            "initialize",
            &json!({"protocolVersion":1,"clientCapabilities":{}}),
            CancellationToken::new(),
        ),
    )
    .await;
    if !matches!(&initialized, Ok(Ok(response)) if response.result().is_ok()) {
        let status = child.try_wait().unwrap();
        let _ = child.kill().await;
        let mut stderr = String::new();
        if let Some(mut stream) = child.stderr.take() {
            use tokio::io::AsyncReadExt as _;
            let _ =
                tokio::time::timeout(Duration::from_secs(2), stream.read_to_string(&mut stderr))
                    .await;
        }
        panic!(
            "ACP initialize failed: {initialized:?}; child status: {status:?}; stderr: {}; owner log: {}",
            stderr.chars().take(4096).collect::<String>(),
            fixture.owner_log()
        );
    }
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

async fn effort_chat(
    State(requests): State<Arc<std::sync::Mutex<Vec<Value>>>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    requests.lock().unwrap().push(body);
    chat().await
}

#[cfg(windows)]
async fn native_tools_chat(
    State(requests): State<Arc<std::sync::Mutex<Vec<Value>>>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let index = {
        let mut captured = requests.lock().unwrap();
        let index = captured.len();
        captured.push(body.clone());
        index
    };
    if index >= 2 {
        return chat().await;
    }
    let name = if index == 0 { "bash" } else { "file_read" };
    assert!(
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == name)
    );
    assert!(
        !body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "output_read")
    );
    let arguments = if index == 0 {
        json!({"command":"cat > native.txt <<'RSI_EOF'\nnative-acp-tool-evidence\n中文 \\ escaped \"quotes\"\nRSI_EOF\n", "run_in_background":false})
    } else {
        json!({"path":"native.txt"})
    };
    let data = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":format!("native-{index}"),"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3}})
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(data))
        .unwrap()
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_bash_and_files_execute_through_owned_windows_acp() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(native_tools_chat))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    profile(&fixture);
    let (child, mut peer) = start(&fixture).await;
    let port = peer.handle();
    let session = request(
        &port,
        "session/new",
        json!({"cwd":fixture.workspace,"mcpServers":[]}),
    )
    .await;
    let pending = request(
        &port,
        "session/prompt",
        json!({"sessionId":session["sessionId"],"prompt":[{"type":"text","text":"exercise native Bash and Files"}]}),
    );
    tokio::pin!(pending);
    let mut permissions = 0;
    loop {
        tokio::select! {
            result = &mut pending => { assert_eq!(result["stopReason"], "end_turn"); break; }
            event = peer.next() => {
                let event = event.unwrap();
                if let Message::Request { id, method, .. } = event.message {
                    assert_eq!(method, "session/request_permission");
                    permissions += 1;
                    port.respond(&id, Ok(&json!({"outcome":{"outcome":"selected","optionId":"allow-once"}}))).await.unwrap();
                }
            }
        }
    }
    assert!(permissions >= 1);
    assert_eq!(
        std::fs::read(fixture.workspace.join("native.txt")).unwrap(),
        "native-acp-tool-evidence\n中文 \\ escaped \"quotes\"\n".as_bytes()
    );
    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 3);
        assert!(
            captured[2]["messages"]
                .to_string()
                .contains("native-acp-tool-evidence")
        );
    }
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

async fn truncated_chat(
    State(requests): State<Arc<std::sync::Mutex<Vec<Value>>>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    requests.lock().unwrap().push(body);
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"role":"assistant","content":"unfinished response"},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"length"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}),
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn output_limit_reports_max_tokens_without_continuation_over_stdio() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(truncated_chat))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    profile(&fixture);
    let settings_path = fixture.temporary.path().join("config/rsi/settings.json");
    let mut settings: Value =
        serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    settings["rsi.agent"]["output_budgets"] = json!([{
        "model":{"deployment":"fixture","model":"fixture-model"}, "max_output_tokens":8192
    }]);
    std::fs::write(settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
    let (child, mut peer) = start(&fixture).await;
    let port = peer.handle();
    let session = request(
        &port,
        "session/new",
        json!({"cwd":fixture.workspace,"mcpServers":[]}),
    )
    .await;
    let result = request(&port, "session/prompt", json!({"sessionId":session["sessionId"],"prompt":[{"type":"text","text":"produce a response"}]})).await;
    assert_eq!(result["stopReason"], "max_tokens");
    let chunk = update(&mut peer).await;
    assert_eq!(chunk["sessionUpdate"], "agent_message_chunk");
    assert_eq!(chunk["content"]["text"], "unfinished response");
    {
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "output limit must stop without dispatching another request"
        );
        assert_eq!(requests[0]["max_tokens"], 8192);
    }
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_during_private_mcp_prepare_reaps_unpublished_process() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    profile(&fixture);
    let marker = fixture.workspace.join("preparing.pid");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rsi/acp/mcp.py");
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

#[cfg(target_os = "linux")]
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
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rsi/acp/mcp.py");
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
#[expect(
    clippy::too_many_lines,
    reason = "One fixture proves draft disposal, selection changes and cold durable replay across two native processes."
)]
async fn stdio_application_closes_drafts_and_replays_owned_native_sessions() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(effort_chat))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let fixture = CliFixture::new(&endpoint);
    let settings_path = fixture.temporary.path().join("config/rsi/settings.json");
    let mut settings: Value =
        serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    settings["rsi.agent"]["default_reasoning_effort"] = json!("high");
    std::fs::write(settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
    let host = fixture
        .temporary
        .path()
        .join("config/rsi/host-profiles/fixture/host.profile.toml");
    let mut contents: toml::Value =
        toml::from_str(&std::fs::read_to_string(&host).unwrap()).unwrap();
    let config = &mut contents["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|step| step["id"].as_str() == Some("fixture-provider"))
        .unwrap()["config"];
    let second_profile = config["language_models"]["fixture-model"].clone();
    config["language_models"]
        .as_table_mut()
        .unwrap()
        .insert("second".into(), second_profile);
    config.as_table_mut().unwrap().insert("reasoning_efforts".into(), toml::from_str("[fixture-model]\nsupported=[\"low\",\"high\"]\ndefault=\"low\"\n[second]\nsupported=[\"low\",\"high\"]\ndefault=\"low\"\n").unwrap());
    std::fs::write(host, toml::to_string(&contents).unwrap()).unwrap();
    profile(&fixture);
    let (child, mut peer) = start(&fixture).await;
    let port = peer.handle();
    let setup = json!({"cwd":fixture.workspace,"mcpServers":[]});
    let draft = request(&port, "session/new", setup.clone()).await;
    let model = "{\"deployment\":\"fixture\",\"model\":\"fixture-model\"}";
    assert_eq!(draft["configOptions"][0]["currentValue"], model);
    assert_eq!(draft["configOptions"][1]["id"], "thought_level");
    assert_eq!(draft["configOptions"][1]["currentValue"], "high");
    assert_eq!(draft["configOptions"][1]["options"][0]["value"], "@default");
    assert_eq!(
        draft["configOptions"][0]["options"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let second = "{\"deployment\":\"fixture\",\"model\":\"second\"}";
    let selected = request(
        &port,
        "session/set_config_option",
        json!({"sessionId":draft["sessionId"],"configId":"model","value":second}),
    )
    .await;
    assert_eq!(selected["configOptions"][0]["currentValue"], second);
    assert_eq!(selected["configOptions"][1]["currentValue"], "@default");
    assert_eq!(
        update(&mut peer).await["configOptions"][0]["currentValue"],
        second
    );
    let configured = request(
        &port,
        "session/set_config_option",
        json!({"sessionId":draft["sessionId"],"configId":"model","value":model}),
    )
    .await;
    assert_eq!(configured["configOptions"][0]["currentValue"], model);
    let configured_update = update(&mut peer).await;
    assert_eq!(configured_update["sessionUpdate"], "config_option_update");
    for (config_id, value) in [
        ("invalid", second),
        ("model", "fixture/fixture-model"),
        ("thought_level", "max"),
        ("thought_level", "invalid effort"),
    ] {
        let invalid = port
            .request(
                "session/set_config_option",
                &json!({"sessionId":draft["sessionId"],"configId":config_id,"value":value}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.result().unwrap_err()["code"], -32602);
    }
    let unchanged = request(
        &port,
        "session/resume",
        json!({"sessionId":draft["sessionId"],"cwd":fixture.workspace,"mcpServers":[]}),
    )
    .await;
    assert_eq!(unchanged["configOptions"][0]["currentValue"], model);
    assert_eq!(unchanged["configOptions"][1]["currentValue"], "@default");
    request(
        &port,
        "session/close",
        json!({"sessionId":draft["sessionId"]}),
    )
    .await;
    let created = request(&port, "session/new", setup.clone()).await;
    assert_eq!(created["configOptions"][0]["currentValue"], model);
    let id = created["sessionId"].clone();
    let initial = request(
        &port,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"explicit initial effort"}]}),
    )
    .await;
    assert_eq!(initial["stopReason"], "end_turn");
    assert_eq!(
        update(&mut peer).await["content"]["text"],
        "hello from daemon"
    );
    request(
        &port,
        "session/set_config_option",
        json!({"sessionId":id,"configId":"model","value":second}),
    )
    .await;
    assert_eq!(
        update(&mut peer).await["sessionUpdate"],
        "config_option_update"
    );
    let response = tokio::time::timeout(
        Duration::from_secs(40),
        port.request(
            "session/prompt",
            &json!({"sessionId":id,"prompt":[{"type":"text","text":"stdio round trip"}]}),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let result = match response.result() {
        Ok(result) => result.clone(),
        Err(error) => {
            // Join the owned application before reporting its native diagnostics.
            peer.close().await.unwrap();
            let output = tokio::time::timeout(Duration::from_secs(40), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            panic!(
                "ACP session/prompt failed: {error}; stderr: {}; owner log: {}",
                String::from_utf8_lossy(&output.stderr),
                fixture.owner_log()
            );
        }
    };
    assert_eq!(result["stopReason"], "end_turn");
    let message = update(&mut peer).await;
    assert_eq!(message["sessionUpdate"], "agent_message_chunk");
    assert_eq!(message["content"]["text"], "hello from daemon");
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["model"], "fixture-model");
        assert_eq!(requests[0]["reasoning_effort"], "high");
        assert_eq!(requests[1]["model"], "second");
        assert_eq!(requests[1]["reasoning_effort"], "low");
    }
    for effort in ["high", "@default", "high"] {
        let configured = request(
            &port,
            "session/set_config_option",
            json!({"sessionId":id,"configId":"thought_level","value":effort}),
        )
        .await;
        assert_eq!(configured["configOptions"][0]["currentValue"], second);
        assert_eq!(configured["configOptions"][1]["currentValue"], effort);
        assert_eq!(
            update(&mut peer).await["configOptions"],
            configured["configOptions"]
        );
    }
    let response = request(
        &port,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"explicit ACP reasoning effort"}]}),
    )
    .await;
    assert_eq!(response["stopReason"], "end_turn");
    assert_eq!(
        update(&mut peer).await["content"]["text"],
        "hello from daemon"
    );
    assert_eq!(requests.lock().unwrap()[2]["reasoning_effort"], "high");
    // Cold startup must recover the durable choice, not an ACP connection cache.
    stop(&fixture, peer, child).await;
    let (child, mut peer) = start(&fixture).await;
    let port = peer.handle();
    let restore = json!({"cwd":fixture.workspace,"mcpServers":[],"sessionId":id});
    let loaded = request(&port, "session/load", restore.clone()).await;
    assert_eq!(loaded["configOptions"][0]["currentValue"], second);
    assert_eq!(loaded["configOptions"][1]["currentValue"], "high");
    assert_eq!(
        update(&mut peer).await["content"]["text"],
        "explicit initial effort"
    );
    assert_eq!(
        update(&mut peer).await["content"]["text"],
        "hello from daemon"
    );
    let user = update(&mut peer).await;
    let assistant = update(&mut peer).await;
    assert_eq!(user["sessionUpdate"], "user_message_chunk");
    assert_eq!(user["content"]["text"], "stdio round trip");
    assert_eq!(assistant["content"]["text"], "hello from daemon");
    assert_eq!(
        update(&mut peer).await["content"]["text"],
        "explicit ACP reasoning effort"
    );
    assert_eq!(
        update(&mut peer).await["content"]["text"],
        "hello from daemon"
    );
    let resumed = request(&port, "session/resume", restore).await;
    assert_eq!(resumed["configOptions"][0]["currentValue"], second);
    assert_eq!(resumed["configOptions"][1]["currentValue"], "high");
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
            .args(["run", "test-acp"])
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
        .args(["run", "test-acp"])
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_acp_clients_share_one_host_and_disconnect_independently() {
    let fixture = CliFixture::new("http://127.0.0.1:1");
    profile(&fixture);
    let (first, first_peer) = start(&fixture).await;
    let owner_path = fixture
        .temporary
        .path()
        .join("state/rsi/session-host/owner.json");
    let before: Value = serde_json::from_slice(&std::fs::read(&owner_path).unwrap()).unwrap();
    let (second, second_peer) = start(&fixture).await;
    let setup = json!({"cwd":fixture.workspace,"mcpServers":[]});
    let first_session = request(&first_peer.handle(), "session/new", setup.clone()).await;
    let second_session = request(&second_peer.handle(), "session/new", setup.clone()).await;
    assert_ne!(first_session["sessionId"], second_session["sessionId"]);
    stop(&fixture, first_peer, first).await;
    let resumed = request(
        &second_peer.handle(),
        "session/resume",
        json!({"sessionId":second_session["sessionId"],"cwd":fixture.workspace,"mcpServers":[]}),
    )
    .await;
    assert!(resumed["configOptions"].is_array());
    let devices = fixture
        .tokio_command()
        .args(["devices", "list"])
        .output()
        .await
        .unwrap();
    assert!(
        devices.status.success(),
        "{}",
        String::from_utf8_lossy(&devices.stderr)
    );
    stop(&fixture, second_peer, second).await;
    let after: Value = serde_json::from_slice(&std::fs::read(&owner_path).unwrap()).unwrap();
    assert_eq!(before["pid"], after["pid"]);
    assert_eq!(before["host_epoch"], after["host_epoch"]);
    let status = fixture
        .tokio_command()
        .args(["host", "status"])
        .output()
        .await
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).starts_with("running"));
}
