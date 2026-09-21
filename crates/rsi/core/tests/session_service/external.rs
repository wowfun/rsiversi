use super::*;
use rsi_acp_protocol::{
    observation::{ConversationId, Status},
    service::ExternalConversations,
};
use rsi_session_protocol::SessionHandle;
use serde_json::{Value, json};

#[derive(Default)]
struct Provider(Mutex<Option<(String, Value)>>);
async fn response(State(provider): State<Arc<Provider>>, Json(_request): Json<Value>) -> Response {
    let Some((id, arguments)) = provider.0.lock().unwrap().take() else {
        return chat().await;
    };
    let call = json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":"external_agent","arguments":arguments.to_string()}}]},"finish_reason":null}]});
    let finish = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":1}});
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}
async fn invoke(
    provider: &Provider,
    handle: &Arc<dyn SessionHandle>,
    id: &str,
    input: Value,
) -> rsi_tools_protocol::ToolResult {
    *provider.0.lock().unwrap() = Some((id.into(), input));
    run_message_to_terminal(handle, id).await;
    handle
        .history_before(None, 64)
        .await
        .unwrap()
        .facts
        .into_iter()
        .rev()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolResult {
                identity, result, ..
            } if identity.call_id() == id => Some(result.clone()),
            _ => None,
        })
        .expect("actual native Tool result")
}
async fn create(daemon: &DaemonFixture, fixture: &Fixture, name: &str) -> Arc<dyn SessionHandle> {
    daemon
        .connection
        .session_service()
        .create(CreateSession {
            workspace_id: daemon
                .connection
                .workspace_registry()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new(name).unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One causal product lifecycle checks delegated authority and direct API interoperability"
)]
async fn native_tool_and_direct_uds_share_external_owner_without_cross_caller_authority() {
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/v1/chat/completions", post(response))
        .with_state(provider.clone());
    let server = tokio::spawn(async {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = fixture(&endpoint);
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/rsi/acp/agent.py")
        .canonicalize()
        .unwrap();
    let secret = json!({"kind":"literal","value":"private-fixture-secret"});
    let config = json!({"directory":fixture.paths.state().join("acp"),"endpoints":[{"id":"configured","enabled":true,"cwd":fixture.workspace,"sandbox":"danger-full-access","launch":{"program":"/usr/bin/python3","arguments":["-u",script,fixture.workspace,"normal"],"environment":{"FIXTURE_SECRET":secret}},"mcp_servers":[{"name":"private","launch":{"program":"/usr/bin/python3","environment":{"MCP_SECRET":secret}}}]}]});
    let source = std::fs::read_to_string(&fixture.profile).unwrap();
    let mut document: toml::Value = toml::from_str(&source).unwrap();
    document["steps"].as_array_mut().unwrap().push(
        toml::Value::try_from(json!({"kind":"patch","target":"rsi-acp","config":config})).unwrap(),
    );
    std::fs::write(&fixture.profile, toml::to_string(&document).unwrap()).unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let direct = rsi_acp_api::Client::new(daemon.connection.api_client()).unwrap();
    assert_eq!(direct.endpoints().await.unwrap()[0].id, "configured");
    let one = create(&daemon, &fixture, "delegating-one").await;
    let two = create(&daemon, &fixture, "delegating-two").await;
    let started = invoke(
        &provider,
        &one,
        "start-call",
        json!({"operation":"start","endpoint":"configured"}),
    )
    .await;
    assert!(!started.is_error, "{started:?}");
    let id = ConversationId::new(started.value["conversation"].as_str().unwrap()).unwrap();
    let snapshot = direct.view(&id).await.unwrap().snapshot;
    assert_eq!(snapshot.status, Status::Ready);
    assert!(
        direct
            .page(&id, snapshot.epoch, 0)
            .await
            .unwrap()
            .records
            .is_empty(),
        "start never sends a prompt"
    );
    let forbidden = invoke(
        &provider,
        &two,
        "foreign-call",
        json!({"operation":"follow_up","conversation":id,"text":"work"}),
    )
    .await;
    assert!(forbidden.is_error);
    assert!(
        direct
            .page(&id, snapshot.epoch, 0)
            .await
            .unwrap()
            .records
            .is_empty()
    );
    let submitted = invoke(
        &provider,
        &one,
        "follow-call",
        json!({"operation":"follow_up","conversation":id,"text":"work"}),
    )
    .await;
    assert!(!submitted.is_error, "{submitted:?}");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while direct.view(&id).await.unwrap().snapshot.status != Status::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let read = invoke(
        &provider,
        &one,
        "read-call",
        json!({"operation":"read","conversation":id}),
    )
    .await;
    assert!(!read.is_error, "{read:?}");
    assert!(read.value.to_string().contains("fixture-output"));
    assert_eq!(direct.residents().await.unwrap().len(), 1);
    verify_attention(&daemon, &direct, &id).await;
    let closed = invoke(
        &provider,
        &one,
        "close-call",
        json!({"operation":"close","conversation":id}),
    )
    .await;
    assert!(!closed.is_error, "{closed:?}");
    assert_eq!(
        direct.view(&id).await.unwrap().snapshot.status,
        Status::Closed
    );
    assert!(direct.residents().await.unwrap().is_empty());
    for pid in std::fs::read_to_string(fixture.workspace.join("peer-pids"))
        .unwrap()
        .lines()
    {
        assert!(!std::path::Path::new("/proc").join(pid).exists());
    }
    daemon.shutdown().await;
    server.abort();
    let _ = server.await;
}

async fn verify_attention(
    daemon: &DaemonFixture,
    direct: &rsi_acp_api::Client,
    id: &ConversationId,
) {
    use rsi_navigation_api::attention::{Client, Status as AttentionStatus, Target};
    let attention = Client::new(daemon.connection.api_client()).unwrap();
    direct.submit(id, "permission").await.unwrap();
    let permission = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let view = direct.view(id).await.unwrap();
            if let Some(permission) = view.permissions.into_iter().next() {
                break permission;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let waiting = attention.read().await.unwrap();
    assert_eq!(
        waiting.entries[0].status,
        AttentionStatus::Waiting,
        "External requests outrank completed native conversations"
    );
    assert_eq!(
        waiting.entries[0].targets,
        vec![Target::External {
            generation: permission.generation.clone(),
            request: permission.id.clone()
        }]
    );
    direct
        .answer(
            id,
            permission.generation.parse().unwrap(),
            &permission.id,
            &permission.options[0].id,
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while direct.view(id).await.unwrap().snapshot.status != Status::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let identity = rsi_navigation_api::attention::ConversationIdentity::External(id.clone());
    let page = attention.read().await.unwrap();
    let position = page
        .entries
        .iter()
        .find(|row| row.position.conversation == identity)
        .unwrap()
        .position
        .clone();
    attention.mark_read(position.clone()).await.unwrap();
    assert!(
        attention
            .read()
            .await
            .unwrap()
            .entries
            .iter()
            .all(|row| row.position.conversation != identity)
    );
    // A complete new prompt between attention reads must still be unread in the same epoch.
    direct.submit(id, "work").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while direct.view(id).await.unwrap().snapshot.status != Status::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let page = attention.read().await.unwrap();
    let row = page
        .entries
        .iter()
        .find(|row| row.position.conversation == identity)
        .unwrap();
    assert_eq!(row.status, AttentionStatus::Unread);
    assert_eq!(row.position.epoch, position.epoch);
    assert!(
        row.position.sequence.parse::<u64>().unwrap() > position.sequence.parse::<u64>().unwrap()
    );
}
