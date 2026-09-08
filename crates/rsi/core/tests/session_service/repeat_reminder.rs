use super::*;
use rsi_agent_session_protocol::{InputMessageSource, TurnOutcome};

const ADVICE: &str = "Tool job_list has been called 3 consecutive times with identical arguments.";

async fn repeating_chat(
    State(requests): State<Arc<Mutex<Vec<serde_json::Value>>>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    let index = {
        let mut requests = requests.lock().unwrap();
        requests.push(request);
        requests.len()
    };
    if index > 3 {
        return chat().await;
    }
    let call = serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":format!("repeat-{index}"),"type":"function","function":{"name":"job_list","arguments":"{}"}}]},"finish_reason":null}]});
    let finish = serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":1}});
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_repeat_contribution_reaches_provider_after_unchanged_real_tool_results() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let router = Router::new()
        .route("/v1/chat/completions", post(repeating_chat))
        .with_state(requests.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            workspace_id: running
                .workspace_registry()
                .unwrap()
                .get_or_create(&fixture.workspace)
                .await
                .unwrap()
                .id,
            session_id: SessionId::new("repeat-provider").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    run_message_to_terminal(&handle, "repeat-three").await;
    let history = handle.history_before(None, 128).await.unwrap();
    assert!(!history.has_more);
    assert!(matches!(
        history.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
    let results = history
        .facts
        .iter()
        .filter(|f| matches!(f.body(), SessionFactBody::ToolResult { .. }))
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 3);
    let reminders = history.facts.iter().filter(|f| matches!(f.body(), SessionFactBody::InputMessageEntered { source: InputMessageSource::PluginContext { contribution_id }, .. } if contribution_id.as_str() == "rsi.repeat-tool-reminder")).collect::<Vec<_>>();
    assert_eq!(reminders.len(), 1);
    assert!(reminders[0].seq() > results[2].seq());
    assert!(
        serde_json::to_string(&reminders[0])
            .unwrap()
            .contains(ADVICE)
    );
    assert_provider_messages(&requests.lock().unwrap(), &results);
    run_message_to_terminal(&handle, "new-turn-no-replay").await;
    assert_eq!(requests.lock().unwrap().len(), 5);
    let history = handle.history_before(None, 128).await.unwrap();
    assert_eq!(history.facts.iter().filter(|f| matches!(f.body(), SessionFactBody::InputMessageEntered { source: InputMessageSource::PluginContext { contribution_id }, .. } if contribution_id.as_str() == "rsi.repeat-tool-reminder")).count(), 1);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

fn assert_provider_messages(
    requests: &[serde_json::Value],
    results: &[&rsi_agent_session_protocol::SessionFact],
) {
    assert_eq!(requests.len(), 4);
    for request in &requests[..3] {
        assert!(!request.to_string().contains(ADVICE));
    }
    let messages = requests[3]["messages"].as_array().unwrap();
    let advice_position = messages
        .iter()
        .position(|m| m.to_string().contains(ADVICE))
        .unwrap();
    for (index, fact) in results.iter().enumerate() {
        let SessionFactBody::ToolResult { result, .. } = fact.body() else {
            unreachable!()
        };
        let (position, message) = messages
            .iter()
            .enumerate()
            .find(|(_, m)| m["tool_call_id"] == format!("repeat-{}", index + 1))
            .unwrap();
        assert!(position < advice_position);
        assert_eq!(message["role"], "tool");
        assert!(!message.to_string().contains(ADVICE));
        let [rsi_tools_protocol::ToolContent::Text { text }] = result.content.as_slice() else {
            panic!("job_list returns one canonical text block")
        };
        assert_eq!(message["content"].as_str().unwrap(), text);
        assert_eq!(result.value, serde_json::json!({"jobs":[]}));
        assert!(!result.is_error);
    }
}
