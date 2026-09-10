use super::*;
use rsi_agent_session_protocol::{
    CommandArguments, DomainRequestId, SessionCommandInvocation, ToolRejection, TurnOutcome,
};

async fn denied_call() -> Response {
    let call = serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"plan-bash","type":"function","function":{"name":"bash","arguments":"{\"command\":\"printf forbidden > should-not-exist\"}"}}]},"finish_reason":null}]});
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
async fn standard_plan_policy_denies_bash_before_intent_start_or_filesystem_effect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(denied_call)),
        )
        .await
        .unwrap();
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
            session_id: SessionId::new("plan-denied-bash").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    let commands = handle.commands().await.unwrap();
    handle
        .execute_command(SessionCommandInvocation {
            command: commands
                .commands()
                .iter()
                .find(|entry| entry.name() == "plan")
                .unwrap()
                .id()
                .clone(),
            request_id: DomainRequestId::new("plan-enable").unwrap(),
            expected_revision: commands.revision(),
            arguments: CommandArguments::new("on".into()).unwrap(),
        })
        .await
        .unwrap();
    run_message_to_terminal(&handle, "attempt-denied-bash").await;
    let history = handle.history_before(None, 64).await.unwrap();
    assert!(!history.has_more);
    let rejected = history
        .facts
        .iter()
        .filter(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
        .collect::<Vec<_>>();
    assert_eq!(rejected.len(), 1);
    assert!(
        matches!(rejected[0].body(), SessionFactBody::ToolRejected { name, rejection: ToolRejection::PolicyDenied { contribution_id, .. }, .. } if name == "bash" && contribution_id.as_str() == "rsi.plan-policy.tools")
    );
    assert!(!history.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolResult { .. }
    )));
    assert!(
        matches!(history.facts.last().unwrap().body(), SessionFactBody::TurnTerminal { outcome: TurnOutcome::Failed { code, .. }, .. } if code == "policy.denied")
    );
    assert!(!fixture.workspace.join("should-not-exist").exists());
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
