//! Actual UDS, `SQLite` and provider-boundary acceptance of input resources and frozen external Tools.
use super::*;
use rsi_agent_session_protocol::{AgentMessageContent, ReferenceReadRequest};
use rsi_session_protocol::SessionHandle;
use serde_json::{Value, json};
#[allow(dead_code)]
#[path = "../../../../rsi-mcp/core/tests/support/mod.rs"]
mod mcp_fixture;

#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<Value>>,
    next_tool: Mutex<Option<(String, Value)>>,
}
async fn response(State(provider): State<Arc<Provider>>, Json(request): Json<Value>) -> Response {
    let call_id = {
        let mut requests = provider.requests.lock().unwrap();
        requests.push(request);
        format!("milestone-{}", requests.len())
    };
    let Some((name, arguments)) = provider.next_tool.lock().unwrap().take() else {
        return chat().await;
    };
    let call = json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":call_id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":null}]});
    let finish = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":1}});
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}
async fn create(daemon: &DaemonFixture, fixture: &Fixture, id: &str) -> Arc<dyn SessionHandle> {
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
            session_id: SessionId::new(id).unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap()
}
fn message(id: &str, content: Vec<SessionInput>) -> SubmitInput {
    SubmitInput {
        message_id: MessageId::new(id).unwrap(),
        content,
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        model: None,
        reasoning_effort: None,
        sandbox: None,
    }
}
async fn submit(handle: &Arc<dyn SessionHandle>, input: SubmitInput) -> MessageReceipt {
    let receipt = handle.submit(input).await.unwrap();
    let (turn, entered) = observe_message_claim(handle, &receipt).await;
    let mut events = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: entered,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(event) = events.next().await {
            if matches!(event.unwrap(), SessionObservation::Fact {fact, ..} if matches!(fact.body(), SessionFactBody::TurnTerminal {turn_id, ..} if turn_id == &turn)) {return;}
        }
        panic!("observation ended before terminal");
    }).await.unwrap();
    receipt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One sequential public-seam scenario preserves causality and exact evidence"
)]
async fn uds_references_keep_exact_retry_cas_and_real_parent_interval_after_restart() {
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
    let daemon = DaemonFixture::new(&fixture).await;
    let source = create(&daemon, &fixture, "reference-source").await;
    let target = create(&daemon, &fixture, "reference-target").await;
    let foreign = create(&daemon, &fixture, "reference-foreign").await;
    submit(
        &source,
        message(
            "source-first",
            vec![SessionInput::Text {
                text: "ORIGINAL 中文材料".into(),
            }],
        ),
    )
    .await;
    let frozen = target
        .capture_reference(source.header().await.unwrap().session_id().clone())
        .await
        .unwrap();
    assert!(frozen.preview.contains("ORIGINAL 中文材料"));
    assert!(
        target
            .history_before(None, 8)
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    assert!(
        foreign
            .preview_reference(frozen.clone(), 0, 8192)
            .await
            .is_err()
    );
    for kind in 0..4 {
        let mut forged = frozen.clone();
        match kind {
            0 => forged.preview.push('x'),
            1 => {
                let rsi_agent_session_protocol::ReferenceSource::Native { binding } =
                    &mut forged.metadata.source
                else {
                    panic!("native")
                };
                binding.session_id = foreign.header().await.unwrap().session_id().clone();
            }
            2 => forged.snapshot.byte_len += 1,
            _ => forged.snapshot.sha256 = "a".repeat(64),
        }
        assert!(
            target
                .submit(message(
                    "bad",
                    vec![SessionInput::Reference { reference: forged }]
                ))
                .await
                .is_err()
        );
        assert!(
            target
                .history_before(None, 8)
                .await
                .unwrap()
                .facts
                .is_empty()
        );
    }
    submit(
        &source,
        message(
            "source-later",
            vec![SessionInput::Text {
                text: "LATER must stay outside the frozen cut".into(),
            }],
        ),
    )
    .await;
    let preview = target
        .preview_reference(frozen.clone(), 0, 65536)
        .await
        .unwrap();
    assert!(preview.text.contains("ORIGINAL 中文材料"));
    assert!(!preview.text.contains("LATER"));
    let input = message(
        "exact-reference",
        vec![SessionInput::Reference {
            reference: frozen.clone(),
        }],
    );
    let receipt = submit(&target, input.clone()).await;
    let before = provider.requests.lock().unwrap().len();
    let retry = target.submit(input.clone()).await.unwrap();
    assert_eq!(retry.accepted_control_seq, receipt.accepted_control_seq);
    assert_eq!(provider.requests.lock().unwrap().len(), before);
    let facts = target.history_before(None, 128).await.unwrap();
    let entered = facts.facts.iter().find(|fact| matches!(fact.body(), SessionFactBody::InputMessageEntered {content, ..} if matches!(content.first(),Some(AgentMessageContent::Reference {reference}) if reference == &frozen))).unwrap();
    let request = ReferenceReadRequest {
        recorded_session_id: target.header().await.unwrap().session_id().clone(),
        fact_seq: entered.seq(),
        content_index: 0,
        offset: 0,
        maximum: 65536,
    };
    let recorded = target
        .read_recorded_reference(request.clone())
        .await
        .unwrap();
    assert_eq!(recorded.text, preview.text);
    assert!(
        foreign
            .read_recorded_reference(request.clone())
            .await
            .is_err()
    );
    let mut wrong = request.clone();
    wrong.content_index = 1;
    assert!(target.read_recorded_reference(wrong).await.is_err());
    assert!(
        provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .to_string()
            .contains("ORIGINAL 中文材料")
    );
    // A real spawn Tool commits the immutable parent interval in SQLite.
    *provider.next_tool.lock().unwrap() = Some((
        "spawn_agent".into(),
        json!({"task_name":"reference-child","message":"Read-only child fixture","fork_turns":"all"}),
    ));
    run_message_to_terminal(&target, "spawn-child").await;
    let children = target.inspect().await.unwrap().tree.descendants;
    assert_eq!(children.len(), 1);
    let child_id = children[0].status.session_id.clone();
    let child = daemon
        .connection
        .session_service()
        .attach(&child_id)
        .await
        .unwrap();
    assert_eq!(
        child
            .read_recorded_reference(request.clone())
            .await
            .unwrap()
            .text,
        preview.text
    );
    let child_header = child.header().await.unwrap();
    assert!(child_header.fork_origin().unwrap().resolved_terminal_seq >= request.fact_seq);
    let later = target
        .capture_reference(source.header().await.unwrap().session_id().clone())
        .await
        .unwrap();
    submit(
        &target,
        message(
            "after-fork",
            vec![SessionInput::Reference { reference: later }],
        ),
    )
    .await;
    let after = target.history_before(None, 128).await.unwrap();
    let outside = after.facts.iter().rev().find(|fact| matches!(fact.body(),SessionFactBody::InputMessageEntered {content,..} if matches!(content.first(),Some(AgentMessageContent::Reference {..})))).unwrap();
    let outside = ReferenceReadRequest {
        fact_seq: outside.seq(),
        ..request.clone()
    };
    assert!(
        child
            .read_recorded_reference(outside.clone())
            .await
            .is_err()
    );
    drop((source, target, foreign, child));
    daemon.shutdown().await;
    let daemon = DaemonFixture::new(&fixture).await;
    let target = daemon
        .connection
        .session_service()
        .attach(&request.recorded_session_id)
        .await
        .unwrap();
    let child = daemon
        .connection
        .session_service()
        .attach(&child_id)
        .await
        .unwrap();
    let calls_before = provider.requests.lock().unwrap().len();
    assert_eq!(
        target
            .read_recorded_reference(request.clone())
            .await
            .unwrap()
            .text,
        preview.text
    );
    assert_eq!(
        child
            .read_recorded_reference(request.clone())
            .await
            .unwrap()
            .text,
        preview.text
    );
    assert!(child.read_recorded_reference(outside).await.is_err());
    assert_eq!(
        target.submit(input).await.unwrap().accepted_control_seq,
        receipt.accepted_control_seq
    );
    assert_eq!(provider.requests.lock().unwrap().len(), calls_before);
    drop((target, child));
    daemon.shutdown().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_durable_session_restores_mcp_and_retrieval_catalogs_offline() {
    let tool_name = rsi_mcp::public_tool_name("fixture", "echo");
    let mcp = mcp_fixture::HttpFixture::start(mcp_fixture::Mode::default()).await;
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
    let settings_file = fixture.paths.config().join("settings.json");
    let mut settings: Value =
        serde_json::from_slice(&std::fs::read(&settings_file).unwrap()).unwrap();
    settings["rsi.mcp"] = serde_json::to_value(mcp.config()).unwrap();
    settings["rsi.retrieval"] = json!({"web_fetch":true,"web_search":true});
    std::fs::write(&settings_file, serde_json::to_vec(&settings).unwrap()).unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let handle = create(&daemon, &fixture, "frozen-integrations").await;
    *provider.next_tool.lock().unwrap() =
        Some((tool_name.clone(), json!({"message":"saved catalog call"})));
    run_message_to_terminal(&handle, "original-catalog").await;
    let initial = handle.history_before(None, 128).await.unwrap();
    assert!(initial.facts.iter().any(|fact| matches!(fact.body(),SessionFactBody::ToolResult {result,..} if !result.is_error && result.value.to_string().contains("saved catalog call"))));
    let definitions = provider.requests.lock().unwrap()[0]["tools"].clone();
    assert!(definitions.to_string().contains("web_fetch"));
    assert!(definitions.to_string().contains(&tool_name));
    let source_seq = initial.facts.last().unwrap().seq();
    drop(handle);
    daemon.shutdown().await;
    mcp.shutdown().await;
    settings["rsi.mcp"] = json!({"servers":[]});
    settings["rsi.retrieval"] = json!({"web_fetch":false,"web_search":false});
    std::fs::write(&settings_file, serde_json::to_vec(&settings).unwrap()).unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let handle = daemon
        .connection
        .session_service()
        .attach(&SessionId::new("frozen-integrations").unwrap())
        .await
        .unwrap();
    let before = provider.requests.lock().unwrap().len();
    let restored = handle.history_before(None, 128).await.unwrap();
    assert_eq!(restored.facts.last().unwrap().seq(), source_seq);
    assert_eq!(provider.requests.lock().unwrap().len(), before);
    for (name, args) in [
        (tool_name.as_str(), json!({"message":"must not reconnect"})),
        ("web_fetch", json!({"url":"https://example.com/"})),
    ] {
        *provider.next_tool.lock().unwrap() = Some((name.to_owned(), args));
        run_message_to_terminal(&handle, name).await;
        assert_eq!(
            provider.requests.lock().unwrap().last().unwrap()["tools"],
            definitions
        );
        let facts = handle.history_before(None, 128).await.unwrap();
        let result = facts
            .facts
            .iter()
            .rev()
            .find_map(|fact| match fact.body() {
                SessionFactBody::ToolResult { result, .. } => Some(result),
                _ => None,
            })
            .unwrap();
        assert!(
            result.is_error,
            "disabled current integration must fail as a retained ToolResult"
        );
        if name == "web_fetch" {
            assert_eq!(result.value["error"], "disabled");
        }
    }
    let fresh = create(&daemon, &fixture, "current-integrations").await;
    run_message_to_terminal(&fresh, "new-catalog").await;
    let current = provider.requests.lock().unwrap().last().unwrap()["tools"].to_string();
    assert!(!current.contains(&tool_name));
    assert!(!current.contains("web_fetch"));
    drop((handle, fresh));
    daemon.shutdown().await;
    server.abort();
}
