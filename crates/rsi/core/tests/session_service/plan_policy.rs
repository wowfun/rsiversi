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

fn review_response(call: Option<(&str, &str, serde_json::Value)>) -> Response {
    let (delta, reason) = match call {
        Some((id, name, args)) => (
            serde_json::json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
            "tool_calls",
        ),
        None => (
            serde_json::json!({"role":"assistant","content":"review settled"}),
            "stop",
        ),
    };
    let delta = serde_json::json!({"choices":[{"delta":delta,"finish_reason":null}]});
    let finish = serde_json::json!({"choices":[{"delta":{},"finish_reason":reason}],"usage":{"prompt_tokens":8,"completion_tokens":2}});
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}
async fn plan_command(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    id: &str,
    value: &str,
) {
    let commands = handle.commands().await.unwrap();
    handle
        .execute_command(SessionCommandInvocation {
            command: commands
                .commands()
                .iter()
                .find(|c| c.name() == "plan")
                .unwrap()
                .id()
                .clone(),
            request_id: DomainRequestId::new(id).unwrap(),
            expected_revision: commands.revision(),
            arguments: CommandArguments::new(value.into()).unwrap(),
        })
        .await
        .unwrap();
}
async fn plan_view(handle: &Arc<dyn rsi_session_protocol::SessionHandle>) -> serde_json::Value {
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
        .find(|e| e.producer().as_str() == "rsi.plan-policy.view")
        .unwrap()
        .view()
        .unwrap()
        .value()
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One real Host scenario proves all typed decisions and stale approval rejection.
async fn saved_plan_review_commits_exact_decision_and_rejects_stale_mode_revision() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for choice in [
        "approve_execute",
        "request_changes",
        "decline",
        "stale",
        "cancelled",
    ] {
        let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let received = requests.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = calls.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
            let index = provider_calls.fetch_add(1, Ordering::SeqCst);
            received.lock().unwrap().push(body.clone());
            async move {
                match index {
                    0 => review_response(Some(("save", "plan_write", serde_json::json!({"title":"Exact reviewed plan","body":"Inspect the source. Then apply the agreed change."})))),
                    1 => {
                        let content = body["messages"].as_array().unwrap().iter().find(|m| m["tool_call_id"] == "save").unwrap()["content"].as_str().unwrap();
                        let saved: serde_json::Value = serde_json::from_str(content).unwrap();
                        review_response(Some(("review", "request_plan_execution", serde_json::json!({"plan_ref":saved["plan_ref"]}))))
                    },
                    _ => review_response(None),
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
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
                session_id: SessionId::new(format!("plan-review-{choice}")).unwrap(),
                agent_preset_id: None,
            })
            .await
            .unwrap();
        plan_command(&handle, "enable", "on").await;
        let worker_handle = handle.clone();
        let worker = tokio::spawn(async move {
            run_message_to_terminal(&worker_handle, "review-plan").await;
        });
        let request = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(request) = handle.pending_questions().await.unwrap().into_iter().next()
                {
                    break request;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("plan review must reach the human broker");
        assert_eq!(
            request.questions[0].prompt,
            "Exact reviewed plan\n\nInspect the source. Then apply the agreed change."
        );
        let before = plan_view(&handle).await;
        assert_eq!(before["enabled"], true);
        assert!(before["review"]["last_review"].is_null());
        assert!(
            handle
                .answer_question(
                    &request.id,
                    rsi_user_questions_protocol::QuestionAnswer {
                        review: None,
                        answers: vec!["approve_execute".into()]
                    }
                )
                .await
                .is_err()
        );
        if choice == "cancelled" {
            handle
                .cancel(
                    rsi_agent_turn_protocol::CancelTarget::Turn(
                        TurnId::new(request.turn_id.clone()).unwrap(),
                    ),
                    None,
                )
                .await
                .unwrap();
            worker.await.unwrap();
            let after = plan_view(&handle).await;
            assert_eq!(after["enabled"], true);
            assert!(after["review"]["last_review"].is_null());
            assert!(handle.pending_questions().await.unwrap().is_empty());
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(running.shutdown().await.is_clean());
            drop(handle);
            let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
                .await
                .unwrap();
            let attached = restarted
                .session_service()
                .unwrap()
                .attach(&SessionId::new(format!("plan-review-{choice}")).unwrap())
                .await
                .unwrap();
            assert_eq!(plan_view(&attached).await, after);
            assert!(attached.pending_questions().await.unwrap().is_empty());
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(restarted.shutdown().await.is_clean());
            server.abort();
            continue;
        }
        if choice == "stale" {
            plan_command(&handle, "off", "off").await;
            plan_command(&handle, "on-again", "on").await;
        }
        let reply = rsi_user_questions_protocol::QuestionAnswer {
            answers: vec![],
            review: Some(rsi_user_questions_protocol::ReviewAnswer {
                binding: request.review.as_ref().unwrap().binding.clone(),
                choice_id: if choice == "stale" {
                    "approve_execute"
                } else {
                    choice
                }
                .into(),
                feedback: Some("human feedback".into()),
            }),
        };
        assert!(handle.answer_question(&request.id, reply).await.unwrap());
        worker.await.unwrap();
        let after = plan_view(&handle).await;
        let history = handle.history_before(None, 128).await.unwrap();
        let review = history.facts.iter().find_map(|f| match f.body() {
            SessionFactBody::ToolResult {
                identity,
                result,
                conclusion,
                ..
            } if identity.call_id() == "review" => Some((result, conclusion)),
            _ => None,
        });
        if choice == "stale" {
            assert_eq!(after["enabled"], true);
            assert!(after["review"]["last_review"].is_null());
            assert!(review.is_none());
            assert!(history.facts.iter().any(|f| matches!(f.body(), SessionFactBody::TurnTerminal { outcome: TurnOutcome::Failed { code, .. }, .. } if code == "tool.settlement_failed")));
        } else {
            let review = review.unwrap_or_else(|| panic!("{choice}: review must retain a result"));
            assert!(!review.0.is_error);
            assert_eq!(after["enabled"], choice != "approve_execute");
            assert_eq!(after["review"]["last_review"]["decision"], choice);
            assert_eq!(
                after["review"]["last_review"]["plan_ref"],
                before["review"]["plan"]["plan_ref"]
            );
            assert_eq!(review.1.is_some(), choice == "decline");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if choice == "decline" { 2 } else { 3 }
            );
            if choice == "approve_execute" {
                assert!(
                    requests.lock().unwrap()[2]["messages"]
                        .to_string()
                        .contains("Plan mode is disabled")
                );
            }
        }
        assert!(running.shutdown().await.is_clean());
        server.abort();
    }
}
