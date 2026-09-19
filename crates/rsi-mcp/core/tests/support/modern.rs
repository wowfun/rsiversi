use super::*;

async fn reply(socket: &mut TcpStream, status: &str, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(&body).await;
}
fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}
fn assert_metadata(request: &Value, headers: &str, method: &str) {
    assert_eq!(header(headers, "mcp-protocol-version"), Some("2026-07-28"));
    assert_eq!(header(headers, "mcp-method"), Some(method));
    assert_eq!(header(headers, "mcp-session-id"), None);
    assert_eq!(
        request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
    assert_eq!(
        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({})
    );
    assert_eq!(
        request["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
        "rsiversi"
    );
}
#[expect(
    clippy::too_many_arguments,
    reason = "The HTTP fixture shares its existing controlled fault and observation owners"
)]
pub(super) async fn serve(
    mut socket: TcpStream,
    request: &Value,
    headers: &str,
    mode: &Mode,
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Arc<Notify>,
    events: broadcast::Receiver<()>,
) {
    let method = request["method"].as_str().unwrap();
    assert_metadata(request, headers, method);
    if method == "server/discover" {
        let code = match mode.modern_fault {
            Some("version" | "version-id") => Some(-32022),
            Some("header") => Some(-32020),
            Some("capability") => Some(-32021),
            _ => None,
        };
        if let Some(code) = code {
            let id = if mode.modern_fault == Some("version-id") {
                json!("foreign")
            } else {
                request["id"].clone()
            };
            reply(&mut socket, "400 Bad Request", &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":"modern error","data":{"supported":["2999-01-01"]}}})).await;
            return;
        }
    }
    if method == "subscriptions/listen" {
        listen(socket, request, mode, events).await;
        return;
    }
    let mut result = match method {
        "server/discover" => {
            json!({"supportedVersions":["2026-07-28"],"capabilities":{"tools":{"listChanged":mode.watch},"resources":{}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"modern-fixture","version":"1"}}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"echo","inputSchema":{"type":"object","properties":{"message":{"type":"string","x-mcp-header":"Message"},"nested":{"type":"object","properties":{"id":{"type":"integer","x-mcp-header":"Id"}}}}},"outputSchema":{"type":"array"}}]})
        }
        "resources/list" => json!({"resources":[{"uri":"fixture://中文","name":"Text"}]}),
        "resources/read" => {
            assert_eq!(
                header(headers, "mcp-name"),
                Some("=?base64?Zml4dHVyZTovL+S4reaWhw==?=")
            );
            json!({"contents":[{"uri":"fixture://中文","text":"resource text"}]})
        }
        "tools/call" => {
            assert_eq!(header(headers, "mcp-name"), Some("echo"));
            let args = &request["params"]["arguments"];
            let message = args["message"].as_str().unwrap();
            if message == " 中文\r\n" {
                assert_eq!(
                    header(headers, "mcp-param-message"),
                    Some("=?base64?IOS4reaWhw0K?=")
                );
            } else if message == "中文" {
                assert_eq!(
                    header(headers, "mcp-param-message"),
                    Some("=?base64?5Lit5paH?=")
                );
            } else {
                assert_eq!(header(headers, "mcp-param-message"), Some(message));
            }
            if args.get("nested").is_some() {
                assert_eq!(header(headers, "mcp-param-id"), Some("9007199254740991"));
            } else {
                assert_eq!(header(headers, "mcp-param-id"), None);
            }
            calls.fetch_add(1, Ordering::AcqRel);
            started.notify_one();
            if mode.wait_call {
                release.notified().await;
            }
            json!({"content":[{"type":"text","text":message}],"structuredContent":[true,null,"exact"]})
        }
        _ => panic!("unexpected modern method {method}"),
    };
    result["resultType"] = "complete".into();
    result["ttlMs"] = 1000.into();
    result["cacheScope"] = "private".into();
    if method == "tools/call" {
        match mode.modern_fault {
            Some("input-required") => {
                result = json!({"resultType":"input_required","requestState":"opaque-value"});
            }
            Some("client-input") => {
                result = json!({"resultType":"input_required","inputRequests":{"sample":{"method":"sampling/createMessage","params":{}}}});
            }
            Some("unknown-result") => result["resultType"] = "future-extension".into(),
            Some("missing-result") => {
                result.as_object_mut().unwrap().remove("resultType");
            }
            _ => {}
        }
    }
    if method == "tools/list" && mode.modern_fault == Some("cache") {
        result["cacheScope"] = "shared".into();
    }
    let value = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
    if mode.sse {
        sse_response(&mut socket, mode, &value).await;
    } else {
        reply(&mut socket, "200 OK", &value).await;
    }
}

async fn listen(
    mut socket: TcpStream,
    request: &Value,
    mode: &Mode,
    mut events: broadcast::Receiver<()>,
) {
    assert_eq!(
        request["params"]["notifications"],
        json!({"toolsListChanged":true,"resourcesListChanged":false})
    );
    if socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await
        .is_err()
    {
        return;
    }
    let id = if mode.modern_fault == Some("subscription-id") {
        json!("foreign")
    } else {
        request["id"].clone()
    };
    let ack = json!({"jsonrpc":"2.0","method":if mode.modern_fault == Some("before-ack") {"notifications/tools/list_changed"} else {"notifications/subscriptions/acknowledged"},"params":{"_meta":{"io.modelcontextprotocol/subscriptionId":id},"notifications":{"toolsListChanged":true}}});
    if write_events(&mut socket, mode, &event_bytes(mode, &[ack], true))
        .await
        .is_err()
    {
        return;
    }
    if events.recv().await.is_ok() {
        let changed = json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":request["id"]}}});
        let _ = write_events(&mut socket, mode, &event_bytes(mode, &[changed], false)).await;
    }
    std::future::pending::<()>().await;
}
