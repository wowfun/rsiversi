use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Actual HTTP, approval and Unix Session API share one gated Bash invocation.
async fn remote_job_preview_follows_actual_tool_origin_and_leaves_the_durable_result_intact() {
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let command = format!(
        "exec 3<>/dev/tcp/127.0.0.1/{}; printf 'preview-before-exit'; printf ready >&3; IFS= read -r -u 3; printf -- '-completed'; printf warning >&2",
        gate.local_addr().unwrap().port()
    );
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    let app=Router::new().route("/v1/chat/completions",post(move || {
        let index=requests.fetch_add(1,Ordering::SeqCst);let command=command.clone();
        async move {
            let delta=if index==0 {serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"bash-preview","type":"function","function":{"name":"bash","arguments":serde_json::to_string(&serde_json::json!({"command":command,"timeout_ms":15000})).unwrap()}}]},"finish_reason":null}]})}
                else {serde_json::json!({"choices":[{"delta":{"role":"assistant","content":"done"},"finish_reason":null}]})};
            let finish=serde_json::json!({"choices":[{"delta":{},"finish_reason":if index==0 {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}});
            Response::builder().status(200).header("content-type","text/event-stream").body(Body::from(format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"))).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let daemon = DaemonFixture::new(&fixture).await;
    let settings = daemon.running.settings_access().unwrap();
    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["sandbox"] = serde_json::json!("danger-full-access");
    value["require_approval"] = serde_json::json!(true);
    settings
        .replace("rsi.agent", &current.version(), value)
        .await
        .unwrap();
    let handle = daemon
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
            session_id: SessionId::new("native-job-preview").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let mut interactions = handle.observe_interactions().await.unwrap();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_message_to_terminal(&task_handle, "run-preview").await;
    });
    let approval = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = interactions.next().await.unwrap().unwrap();
            if let Some(approval) = snapshot.approvals().first() {
                break approval.clone();
            }
        }
    })
    .await
    .unwrap();
    assert!(
        handle
            .answer_approval(
                &SessionId::new(approval.subject.session_id()).unwrap(),
                &approval.id,
                rsi_approval_protocol::ApprovalDecision::AllowOnce
            )
            .await
            .unwrap()
    );
    drop(interactions);
    let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    socket.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    let turn = handle.inspect().await.unwrap().active_turn_id.unwrap();
    let status = handle
        .read_jobs(rsi_agent_turn_protocol::TurnJobsRequest {
            turn_id: turn.clone(),
            generation: None,
            after: None,
            limit: 32,
        })
        .await
        .unwrap();
    let job = &status.page().jobs[0];
    assert!(!job.reported);
    let request = rsi_agent_turn_protocol::JobPreviewRequest {
        turn_id: turn,
        generation: status.page().generation,
        job_id: job.id.clone(),
        effect_id: rsi_agent_session_protocol::EffectId::new(job.origin.clone().unwrap()).unwrap(),
        stdout_bytes: 1024,
        stderr_bytes: 1024,
    };
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(260));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            ticks.tick().await;
            let page = handle.peek_job(request.clone()).await.unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < 64 * 1024);
            if page.preview.unwrap().stdout.bytes(1024).unwrap() == b"preview-before-exit" {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    assert!(matches!(
        handle.peek_job(request.clone()).await,
        Err(rsi_session_protocol::SessionError::Api(
            rsi_api_protocol::ApiError::Capacity
        ))
    ));
    socket.write_all(b"continue\n").await.unwrap();
    task.await.unwrap();
    let history = handle.history_before(None, 128).await.unwrap();
    let result = history
        .facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolResult {
                effect_id, result, ..
            } if effect_id == &request.effect_id => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        result.value["stdout"]["text"],
        "preview-before-exit-completed"
    );
    assert_eq!(result.value["stderr"]["text"], "warning");
    assert!(history.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::TurnTerminal {
            outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
            ..
        }
    )));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let local = daemon
        .running
        .session_service()
        .unwrap()
        .attach(handle.header().await.unwrap().session_id())
        .await
        .unwrap();
    assert!(matches!(
        local.peek_job(request).await,
        Err(rsi_session_protocol::SessionError::NotFound(_))
    ));
    daemon.shutdown().await;
    server.abort();
    let _ = server.await;
}
