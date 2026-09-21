use super::*;
use async_trait::async_trait;
use rsi_acp::{
    Peer, StreamTransport,
    server::{AgentBackend, Failure},
};
use rsi_acp_agent::{NativeAgent, SessionOwner};
use rsi_acp_protocol::{Message, schema};
use rsi_agent_turn_protocol::{TurnService, TurnServiceContract};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use rsi_session_protocol::SessionHandle;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

fn dto<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

#[derive(Debug)]
struct Capture(Arc<OnceLock<Arc<dyn TurnService>>>);
#[async_trait]
impl PluginFactory for Capture {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<TurnServiceContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.0.set(plan.local::<TurnServiceContract>()?).unwrap();
        Ok(())
    }
}

// This fixture exercises native Session translation. Private MCP/ownership
// preparation is intentionally supplied by a separate test owner.
#[derive(Debug)]
struct Owner {
    sessions: Arc<dyn SessionService>,
    workspaces: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    owned: Mutex<BTreeMap<String, Arc<dyn SessionHandle>>>,
    stopped: AtomicBool,
    next: std::sync::atomic::AtomicUsize,
    preparing: Mutex<Option<(Arc<tokio::sync::Semaphore>, Arc<tokio::sync::Semaphore>)>>,
}
#[async_trait]
impl SessionOwner for Owner {
    async fn create(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<Arc<dyn SessionHandle>, Failure> {
        assert!(request.mcp_servers.is_empty());
        let barriers = self.preparing.lock().unwrap().clone();
        if let Some((entered, release)) = barriers {
            entered.add_permits(1);
            release.acquire().await.unwrap().forget();
        }
        let workspace = self.workspaces.get_or_create(&request.cwd).await.unwrap();
        let next = self.next.fetch_add(1, Ordering::Relaxed);
        let id = SessionId::new(if next == 0 {
            "acp-native-fixture".to_owned()
        } else {
            format!("acp-native-{next}")
        })
        .unwrap();
        let session = self
            .sessions
            .create(CreateSession {
                workspace_id: workspace.id,
                session_id: id.clone(),
                agent_preset_id: None,
            })
            .await
            .unwrap();
        self.owned
            .lock()
            .unwrap()
            .insert(id.to_string(), session.clone());
        Ok(session)
    }
    async fn restore(
        &self,
        request: schema::ResumeSessionRequest,
    ) -> Result<Arc<dyn SessionHandle>, Failure> {
        assert!(request.mcp_servers.is_empty());
        let handle = self
            .owned
            .lock()
            .unwrap()
            .get(request.session_id.0.as_ref())
            .cloned()
            .ok_or(Failure::NotFound)?;
        if std::path::Path::new(handle.header().await.unwrap().canonical_cwd()) != request.cwd {
            return Err(Failure::Parameters);
        }
        Ok(handle)
    }
    async fn list(
        &self,
        _: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, Failure> {
        Ok(dto(json!({"sessions":[]})))
    }
    async fn close(&self, session: &SessionId) -> Result<(), Failure> {
        self.owned.lock().unwrap().remove(session.as_str());
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), Failure> {
        self.owned.lock().unwrap().clear();
        self.stopped.store(true, Ordering::Release);
        Ok(())
    }
}

async fn boot(fixture: &Fixture) -> (RunningRsi, Arc<NativeAgent>, Arc<Owner>) {
    let capture = Arc::new(OnceLock::new());
    let mut addon = rsi::StandardAddonBuilder::new("fixture.acp");
    addon
        .register_linked(
            "fixture.acp.capture",
            "1",
            rsi_meta::UpdateMode::RestartRequired,
            Arc::new(Capture(capture.clone())),
        )
        .unwrap();
    addon
        .register_fragment(rsi_host::ProfileFragment::new(
            "fixture.acp",
            [rsi_host::ProfileEntry::new(
                "fixture.acp.capture",
                "fixture.acp.capture",
                Value::Null,
            )],
        ))
        .unwrap();
    let running = RunningRsi::boot(
        composition(fixture.paths.clone())
            .with_addons(rsi::StandardAddonSet::new([addon.build().unwrap()]).unwrap()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let owner = Arc::new(Owner {
        sessions: running.session_service().unwrap(),
        workspaces: running.workspace_registry().unwrap(),
        owned: Mutex::new(BTreeMap::new()),
        stopped: AtomicBool::new(false),
        next: std::sync::atomic::AtomicUsize::new(0),
        preparing: Mutex::new(None),
    });
    let backend = Arc::new(NativeAgent::new(
        owner.clone(),
        capture.get().unwrap().clone(),
    ));
    (running, backend, owner)
}
fn peers() -> (Peer, Peer) {
    let (left, right) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(left);
    let client = Peer::start(StreamTransport::new(read, write));
    let (read, write) = tokio::io::split(right);
    (client, Peer::start(StreamTransport::new(read, write)))
}
async fn collect(client: &mut Peer, count: usize) -> Vec<Value> {
    let mut updates = Vec::new();
    for index in 0..count {
        let message = tokio::time::timeout(std::time::Duration::from_secs(20), client.next())
            .await
            .unwrap_or_else(|_| panic!("timed out after {index}/{count} ACP updates"))
            .unwrap();
        let Message::Notification { params, .. } = message.message else {
            panic!("expected update");
        };
        updates.push(params["update"].clone());
    }
    updates
}
async fn many_chunks(Json(_request): Json<Value>) -> Response {
    use std::fmt::Write as _;
    let mut body = String::new();
    for index in 0..1200 {
        let chunk = json!({"choices":[{"delta":{"role":"assistant","content":format!("{index},")},"finish_reason":null}]});
        write!(body, "data: {chunk}\n\n").unwrap();
    }
    body.push_str(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_acp_prompt_settles_and_load_replays_beyond_the_last_1024_facts() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(many_chunks)),
        )
        .await
        .unwrap();
    });
    let fixture = fixture(&endpoint);
    let (running, backend, owner) = boot(&fixture).await;
    let created = backend
        .new_session(dto(json!({"cwd":fixture.workspace,"mcpServers":[]})))
        .await
        .unwrap();
    let (mut client, server) = peers();
    let port = server.handle();
    let prompt = tokio::spawn({
        let backend = backend.clone();
        let port = port.clone();
        let id = created.session_id.clone();
        async move {
            let result = backend
                .prompt(
                    schema::PromptRequest::new(
                        id,
                        vec![schema::ContentBlock::Text(schema::TextContent::new(
                            "ancient ACP input",
                        ))],
                    ),
                    port,
                )
                .await;
            eprintln!("native ACP prompt result: {result:?}");
            result
        }
    });
    let updates = collect(&mut client, 1200).await;
    assert_eq!(updates.first().unwrap()["content"]["text"], "0,");
    assert_eq!(updates.last().unwrap()["content"]["text"], "1199,");
    assert_eq!(
        prompt.await.unwrap().unwrap().stop_reason,
        schema::StopReason::EndTurn
    );
    let handle = owner
        .owned
        .lock()
        .unwrap()
        .get(created.session_id.0.as_ref())
        .unwrap()
        .clone();
    let horizon = handle.history_before(None, 1).await.unwrap().durable_seq;
    eprintln!("native ACP horizon: {horizon}");
    assert!(horizon > 1024);
    let request = json!({"sessionId":created.session_id,"cwd":fixture.workspace,"mcpServers":[]});
    backend.resume(dto(request.clone())).await.unwrap();
    let load = tokio::spawn({
        let backend = backend.clone();
        async move {
            let result = backend.load(dto(request), port).await;
            eprintln!("native ACP load result: {result:?}");
            result
        }
    });
    let replay = collect(&mut client, 1201).await;
    load.await.unwrap().unwrap();
    assert_eq!(replay[0]["sessionUpdate"], "user_message_chunk");
    assert_eq!(replay[0]["content"]["text"], "ancient ACP input");
    assert_eq!(&replay[1..], updates.as_slice());
    backend
        .close(schema::CloseSessionRequest::new(created.session_id))
        .await
        .unwrap();
    backend.shutdown().await.unwrap();
    assert!(owner.stopped.load(Ordering::Acquire));
    drop(handle);
    client.close().await.unwrap();
    server.close().await.unwrap();
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

async fn pending_chat(State(started): State<Arc<tokio::sync::Notify>>) -> Response {
    started.notify_one();
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(futures_util::stream::pending::<
            Result<String, std::io::Error>,
        >()))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_acp_retains_prompt_after_handler_loss_and_cancel_settles_before_close() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let started = Arc::new(tokio::sync::Notify::new());
    let provider = tokio::spawn({
        let started = started.clone();
        async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(pending_chat))
                    .with_state(started),
            )
            .await
            .unwrap();
        }
    });
    let fixture = fixture(&endpoint);
    let (running, backend, owner) = boot(&fixture).await;
    let created = backend
        .new_session(dto(json!({"cwd":fixture.workspace,"mcpServers":[]})))
        .await
        .unwrap();
    let (client, server) = peers();
    let prompt = tokio::spawn({
        let backend = backend.clone();
        let port = server.handle();
        let id = created.session_id.clone();
        async move {
            backend
                .prompt(
                    schema::PromptRequest::new(
                        id,
                        vec![schema::ContentBlock::Text(schema::TextContent::new(
                            "wait for cancellation",
                        ))],
                    ),
                    port,
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    prompt.abort();
    assert!(prompt.await.unwrap_err().is_cancelled());
    assert!(matches!(
        backend
            .prompt(
                schema::PromptRequest::new(
                    created.session_id.clone(),
                    vec![schema::ContentBlock::Text(schema::TextContent::new(
                        "must not submit twice"
                    ))]
                ),
                server.handle()
            )
            .await,
        Err(Failure::Busy)
    ));
    backend
        .cancel(schema::CancelNotification::new(created.session_id.clone()))
        .await
        .unwrap();
    backend
        .close(schema::CloseSessionRequest::new(created.session_id))
        .await
        .unwrap();
    assert!(owner.owned.lock().unwrap().is_empty());
    backend.shutdown().await.unwrap();
    client.close().await.unwrap();
    server.close().await.unwrap();
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_acp_setups_and_close_do_not_share_a_mutex() {
    let fixture = fixture("http://127.0.0.1:1");
    let (running, backend, owner) = boot(&fixture).await;
    let request = json!({"cwd":fixture.workspace,"mcpServers":[]});
    let first = backend.new_session(dto(request.clone())).await.unwrap();
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    *owner.preparing.lock().unwrap() = Some((entered.clone(), release.clone()));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let backend = backend.clone();
        let request = request.clone();
        tasks.push(tokio::spawn(async move {
            backend.new_session(dto(request)).await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.acquire_many(2))
        .await
        .expect("both preparations must enter independently")
        .unwrap()
        .forget();
    backend
        .close(dto(json!({"sessionId":first.session_id})))
        .await
        .unwrap();
    release.add_permits(2);
    let a = tasks.remove(0).await.unwrap().unwrap();
    let b = tasks.remove(0).await.unwrap().unwrap();
    assert_ne!(a.session_id, b.session_id);
    backend.shutdown().await.unwrap();
    assert!(running.shutdown().await.is_clean());
}
