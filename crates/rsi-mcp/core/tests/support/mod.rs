mod modern;
use async_trait::async_trait;
use rsi_credentials_protocol::{
    CredentialRef, CredentialSource, CredentialsResolve, ResolvedCredential, SecretValue,
};
use rsi_mcp::{McpConfig, McpService, ServerConfig, TransportConfig};
use rsi_process::{DuplexProcess, DuplexProcessSpec, ManagedDuplexProcess};
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessRequest, Sandbox, SandboxBackend, SandboxFileSystem,
    SandboxNetwork, SandboxScratch, WorkspaceReadRequest, WorkspaceReadScope,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, broadcast},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Default)]
pub struct Credentials {
    pub resolutions: AtomicUsize,
    pub missing: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl CredentialsResolve for Credentials {
    async fn resolve(
        &self,
        _: &CredentialRef,
    ) -> rsi_credentials_protocol::Result<ResolvedCredential> {
        self.resolutions.fetch_add(1, Ordering::AcqRel);
        if self.missing.load(Ordering::Acquire) {
            return Err(rsi_credentials_protocol::CredentialsError::NotConfigured(
                "fixture".into(),
            ));
        }
        Ok(ResolvedCredential {
            secret: SecretValue::new("fixture-secret").unwrap(),
            source: CredentialSource::File,
        })
    }
}
#[derive(Debug)]
pub struct NoProcess;
impl DuplexProcess for NoProcess {
    fn spawn(&self, _: DuplexProcessSpec) -> rsi_process::Result<ManagedDuplexProcess> {
        panic!("HTTP must not launch a process")
    }
}
#[derive(Debug)]
pub struct TestSandbox;
#[async_trait]
impl Sandbox for TestSandbox {
    async fn workspace_read(
        &self,
        _: WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<WorkspaceReadScope> {
        panic!("MCP stdio uses an explicit configured process plan")
    }
    async fn confine(&self, request: ProcessRequest) -> rsi_sandbox::Result<ConfinedProcess> {
        assert_eq!(request.mode, rsi_sandbox::SandboxMode::DangerFullAccess);
        assert_eq!(request.workspace, request.cwd);
        Ok(ConfinedProcess {
            program: request.program,
            arguments: request.arguments.into_iter().map(Into::into).collect(),
            cwd: request.cwd,
            stamp: EnforcementStamp {
                requested: request.mode,
                backend: SandboxBackend::Unconfined,
                workspace: request.workspace,
                filesystem: SandboxFileSystem::Unconfined,
                scratch: SandboxScratch::Host,
                network: SandboxNetwork::Host,
            },
        })
    }
}
#[derive(Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent transport fault switches are intentionally composable."
)]
pub struct Mode {
    pub modern: bool,
    pub modern_fault: Option<&'static str>,
    pub watch: bool,
    pub sse: bool,
    pub changed: bool,
    pub oversize: bool,
    pub wrong_id: bool,
    pub duplicate_cursor: bool,
    pub wait_call: bool,
    pub wait_list: bool,
    pub tool_count: Option<usize>,
    pub remote_error: bool,
    pub encoded_response: bool,
}
pub struct HttpFixture {
    pub url: String,
    pub mode: Arc<Mutex<Mode>>,
    pub calls: Arc<AtomicUsize>,
    pub started: Arc<Notify>,
    pub release: Arc<Notify>,
    pub credentials_seen: Arc<AtomicUsize>,
    pub events: broadcast::Sender<()>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}
impl HttpFixture {
    pub async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let mode = Arc::new(Mutex::new(mode));
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let credentials_seen = Arc::new(AtomicUsize::new(0));
        let (events, _) = broadcast::channel(4);
        let stop = CancellationToken::new();
        let settings = mode.clone();
        let count = calls.clone();
        let began = started.clone();
        let resume = release.clone();
        let auth = credentials_seen.clone();
        let event = events.clone();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    () = stopping.cancelled() => break,
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); },
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap(); let settings = settings.clone(); let count = count.clone(); let began = began.clone(); let resume = resume.clone(); let auth = auth.clone(); let events = event.subscribe();
                        tasks.spawn(async move { serve(socket, settings, count, began, resume, auth, events).await; });
                    },
                }
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Self {
            url,
            mode,
            calls,
            started,
            release,
            credentials_seen,
            events,
            stop,
            task,
        }
    }
    pub fn config(&self) -> McpConfig {
        McpConfig {
            servers: vec![ServerConfig {
                id: "fixture".into(),
                enabled: true,
                tools: vec!["echo".into()],
                transport: TransportConfig::StreamableHttp {
                    url: self.url.clone(),
                    credential: None,
                },
            }],
        }
    }
    #[expect(
        clippy::unused_self,
        reason = "The fixture exposes one uniform service construction interface."
    )]
    pub fn service(&self, credentials: Arc<Credentials>) -> McpService {
        McpService::new(credentials, Arc::new(NoProcess), Arc::new(TestSandbox))
    }
    pub async fn shutdown(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
async fn response(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nMcp-Session-Id: fixture-session\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await?;
    socket.write_all(bytes).await
}
#[expect(
    clippy::too_many_lines,
    reason = "One finite mock connection exercises the complete MCP exchange sequence."
)]
async fn serve(
    mut socket: TcpStream,
    mode: Arc<Mutex<Mode>>,
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Arc<Notify>,
    auth: Arc<AtomicUsize>,
    mut events: broadcast::Receiver<()>,
) {
    let mut bytes = Vec::new();
    let headers_end;
    loop {
        let mut chunk = [0u8; 4096];
        let Ok(count) = socket.read(&mut chunk).await else {
            return;
        };
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            headers_end = index + 4;
            break;
        }
        assert!(bytes.len() <= 65536);
    }
    let headers = String::from_utf8(bytes[..headers_end].to_vec()).unwrap();
    let lower = headers.to_ascii_lowercase();
    if lower.contains("authorization: bearer fixture-secret") {
        auth.fetch_add(1, Ordering::AcqRel);
    }
    let selected = mode.lock().unwrap().clone();
    if headers.starts_with("GET ") {
        assert!(!selected.modern, "modern MCP must not open a GET stream");
        if selected.watch {
            if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: connected\n\n").await.is_err() { return; }
            if events.recv().await.is_ok() {
                let _ = socket.write_all(b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n").await;
            }
            std::future::pending::<()>().await;
        } else {
            let _ = response(&mut socket, "405 Method Not Allowed", "text/plain", b"").await;
        }
        return;
    }
    let length: usize = lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
        .unwrap()
        .parse()
        .unwrap();
    assert!(length <= rsi_mcp::MAXIMUM_FRAME_BYTES);
    while bytes.len() < headers_end + length {
        let mut chunk = [0u8; 4096];
        let Ok(count) = socket.read(&mut chunk).await else {
            return;
        };
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let request: Value = serde_json::from_slice(&bytes[headers_end..headers_end + length]).unwrap();
    let method = request["method"].as_str().unwrap();
    if selected.modern {
        modern::serve(
            socket, &request, &headers, &selected, calls, started, release, events,
        )
        .await;
        return;
    }
    if method == "server/discover" {
        let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32602,"message":"initialize first"}})).unwrap();
        let _ = response(&mut socket, "400 Bad Request", "application/json", &body).await;
        return;
    }
    if method == "notifications/initialized" {
        let _ = response(&mut socket, "202 Accepted", "text/plain", b"").await;
        return;
    }
    if method != "initialize" {
        assert!(lower.contains("mcp-session-id: fixture-session"));
        assert!(lower.contains("mcp-protocol-version: 2025-11-25"));
    }
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true},"resources":{}},"serverInfo":{"name":"fixture","version":"1"},"instructions":"External fixture instructions"})
        }
        "tools/list" if selected.oversize => {
            let _ = response(
                &mut socket,
                "200 OK",
                "application/json",
                &vec![b' '; rsi_mcp::MAXIMUM_FRAME_BYTES + 1],
            )
            .await;
            return;
        }
        "tools/list" => {
            if selected.wait_list {
                started.notify_one();
                release.notified().await;
            }
            let mut result = json!({"tools":[{"name":"echo","description":if selected.changed {"Changed schema"} else {"Echo fixture"},"inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false},"annotations":{"readOnlyHint":true},"_meta":{"exact":18_446_744_073_709_551_615_u64}}]});
            if let Some(count) = selected.tool_count {
                let first = result["tools"][0].clone();
                result["tools"] = Value::Array(
                    (0..count)
                        .map(|index| {
                            let mut tool = first.clone();
                            if index > 0 {
                                tool["name"] = format!("extra{index}").into();
                            }
                            tool
                        })
                        .collect(),
                );
            }
            if selected.duplicate_cursor {
                result["nextCursor"] = "same".into();
            }
            result
        }
        "resources/list" => {
            json!({"resources":[{"uri":"fixture://text","name":"Fixture text","mimeType":"text/plain"}]})
        }
        "resources/read" => {
            json!({"contents":[{"uri":"fixture://text","text":"Frozen-catalog resource 中文"}]})
        }
        "tools/call" => {
            calls.fetch_add(1, Ordering::AcqRel);
            started.notify_one();
            if selected.wait_call {
                release.notified().await;
            }
            json!({"content":[{"type":"text","text":request["params"]["arguments"]["message"]}],"structuredContent":{"exact":18_446_744_073_709_551_615_u64}})
        }
        _ => panic!("unexpected fixture method {method}"),
    };
    let id = if selected.wrong_id {
        json!("wrong")
    } else {
        request["id"].clone()
    };
    let value = if selected.remote_error && method == "tools/call" {
        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"server-private-diagnostic"}})
    } else {
        json!({"jsonrpc":"2.0","id":id,"result":result})
    };
    let body = serde_json::to_vec(&value).unwrap();
    if selected.encoded_response {
        assert!(lower.contains("accept-encoding: identity"));
        let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope").await;
        return;
    }
    if selected.sse {
        let prefix = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: fixture-session\r\nConnection: close\r\n\r\ndata: {}\n\n",
            String::from_utf8(body).unwrap()
        );
        for chunk in prefix.as_bytes().chunks(7) {
            if socket.write_all(chunk).await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
    } else {
        let _ = response(&mut socket, "200 OK", "application/json", &body).await;
    }
}
