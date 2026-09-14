use super::*;
use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, SessionCommandInvocation};
use std::sync::atomic::{AtomicUsize, Ordering};

fn lists() -> [serde_json::Value; 3] {
    [
        serde_json::json!([{"content":"Inspect source","status":"in_progress"},{"content":"Check fixtures","status":"in_progress"}]),
        serde_json::json!([{"content":"Verify final result","status":"pending"}]),
        serde_json::json!([]),
    ]
}
async fn projected(handle: &Arc<dyn rsi_session_protocol::SessionHandle>) -> serde_json::Value {
    let snapshot = handle
        .observe_projections()
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    snapshot
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == rsi_agent_todo::VIEW)
        .unwrap()
        .view()
        .unwrap()
        .value()
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn todo_replaces_across_turns_clears_and_is_allowed_in_plan_mode() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = requests.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let app=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {
        let index=count.fetch_add(1,Ordering::SeqCst); received.lock().unwrap().push(body);
        async move {
            let is_tool=index.is_multiple_of(2);
            let delta=if is_tool {serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":format!("todo-{index}"),"type":"function","function":{"name":"todo_write","arguments":serde_json::to_string(&serde_json::json!({"todos":lists()[index/2]})).unwrap()}}]},"finish_reason":null}]})}
                else {serde_json::json!({"choices":[{"delta":{"role":"assistant","content":"done"},"finish_reason":null}]})};
            let finish=serde_json::json!({"choices":[{"delta":{},"finish_reason":if is_tool {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}});
            Response::builder().status(200).header("content-type","text/event-stream").body(Body::from(format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"))).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let mut handle = running
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
            session_id: SessionId::new("todo-plan").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    assert_eq!(projected(&handle).await, serde_json::json!([]));
    let commands = handle.commands().await.unwrap();
    handle
        .execute_command(SessionCommandInvocation {
            command: commands
                .commands()
                .iter()
                .find(|command| command.name() == "plan")
                .unwrap()
                .id()
                .clone(),
            request_id: DomainRequestId::new("plan-on").unwrap(),
            expected_revision: commands.revision(),
            arguments: CommandArguments::new("on".into()).unwrap(),
        })
        .await
        .unwrap();
    for (index, expected) in lists().into_iter().enumerate() {
        run_message_to_terminal(&handle, &format!("todo-turn-{index}")).await;
        assert_eq!(projected(&handle).await, expected);
        if index == 0 {
            assert!(running.shutdown().await.is_clean());
            drop(handle);
            running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
                .await
                .unwrap();
            handle = running
                .session_service()
                .unwrap()
                .attach(&SessionId::new("todo-plan").unwrap())
                .await
                .unwrap();
            assert_eq!(
                projected(&handle).await,
                expected,
                "completed Todo settlement survives a cold Host restart"
            );
            assert_eq!(
                requests.lock().unwrap().len(),
                2,
                "recovery projection starts no model request"
            );
        }
    }
    let history = handle.history_before(None, 128).await.unwrap();
    assert_eq!(history.facts.iter().filter(|fact|matches!(fact.body(),SessionFactBody::ToolResult{result,..} if result.value.get("todos").is_some() && !result.is_error)).count(),3);
    assert!(
        !history
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
    );
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 6);
    assert!(
        serde_json::to_string(&requests[1])
            .unwrap()
            .contains("Current tasks:")
    );
    assert!(running.shutdown().await.is_clean());
    server.abort();
    let _ = server.await;
}
