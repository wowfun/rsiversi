use super::*;
use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, SessionCommandInvocation};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
struct ModelProbe {
    requests: tokio::sync::mpsc::Sender<(String, Option<String>)>,
    release: Arc<tokio::sync::Notify>,
    count: Arc<AtomicUsize>,
    retry: bool,
}
async fn response(
    State(probe): State<ModelProbe>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let index = probe.count.fetch_add(1, Ordering::SeqCst);
    probe
        .requests
        .send((
            body["model"].as_str().unwrap().into(),
            body["reasoning_effort"].as_str().map(str::to_owned),
        ))
        .await
        .unwrap();
    if index == 0 {
        probe.release.notified().await;
        if probe.retry {
            return Response::builder()
                .status(500)
                .body(Body::from("retry fixture"))
                .unwrap();
        }
    }
    let delta = if index == 0 {
        serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"selection-directory","type":"function","function":{"name":"directory_list","arguments":"{}"}}]},"finish_reason":null}]})
    } else {
        serde_json::json!({"choices":[{"delta":{"role":"assistant","content":"done"},"finish_reason":null}]})
    };
    let finish = serde_json::json!({"choices":[{"delta":{},"finish_reason":if index == 0 {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}});
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            "data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        )))
        .unwrap()
}

async fn select(handle: &Arc<dyn rsi_session_protocol::SessionHandle>, request: &str) {
    let commands = handle.commands().await.unwrap();
    let command = commands
        .commands()
        .iter()
        .find(|command| command.name() == "model-selection")
        .expect("standard selection command");
    handle.execute_command(SessionCommandInvocation {
        command:command.id().clone(),request_id:DomainRequestId::new(request).unwrap(),expected_revision:commands.revision(),
        arguments:CommandArguments::new(serde_json::json!({"model":{"deployment":"fixture","model":"second"},"reasoning_effort":"high"})).unwrap(),
    }).await.unwrap();
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    NextStep,
    DispatchedFailure,
    WholeTurn,
    Queued,
    Goal,
}
async fn wait_receipt(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    receipt: &rsi_agent_turn_protocol::MessageReceipt,
) {
    let (turn_id, entered) = observe_message_claim(handle, receipt).await;
    let mut observation = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: entered,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async { loop {
        if matches!(observation.next().await.unwrap().unwrap(), SessionObservation::Fact {fact,..} if matches!(fact.body(),SessionFactBody::TurnTerminal {turn_id: observed,..} if observed == &turn_id)) { break; }
    }}).await.unwrap();
}
fn input(id: &str) -> SubmitInput {
    SubmitInput {
        message_id: MessageId::new(id).unwrap(),
        content: vec![SessionInput::Text { text: id.into() }],
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        model: None,
        reasoning_effort: None,
        sandbox: None,
    }
}
#[allow(clippy::too_many_lines)] // Each scenario uses the same gated request lifecycle and cold restart oracle.
async fn exercise(scenario: Scenario) {
    let retry = scenario == Scenario::DispatchedFailure;
    let (send, mut received) = tokio::sync::mpsc::channel(8);
    let release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(response))
        .with_state(ModelProbe {
            requests: send,
            release: release.clone(),
            count: Arc::new(AtomicUsize::new(0)),
            retry,
        });
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let fixture = fixture(&endpoint);
    let mut profile = std::fs::read_to_string(&fixture.profile).unwrap();
    profile.push_str("\n[steps.config.language_models.second]\ncontext_window_tokens = 128000\ndefault_output_reserve_tokens = 4096\nmax_output_reserve_tokens = 8192\n[steps.config.reasoning_efforts.fixture-model]\nsupported = [\"low\", \"high\"]\ndefault = \"low\"\n[steps.config.reasoning_efforts.second]\nsupported = [\"low\", \"high\"]\ndefault = \"low\"\n");
    std::fs::write(&fixture.profile, profile).unwrap();
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
            session_id: SessionId::new(if retry {
                "selection-retry"
            } else {
                "selection-step"
            })
            .unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Trusted,
        })
        .await
        .unwrap();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        if scenario == Scenario::Goal {
            task_handle
                .control_goal(rsi_goal::GoalControl {
                    request_id: DomainRequestId::new("create-selection-goal").unwrap(),
                    expected_revision: task_handle.commands().await.unwrap().revision(),
                    action: rsi_agent_goal::GoalAction::Create {
                        id: DomainRequestId::new("selection-goal").unwrap(),
                        objective: "Verify model selection".into(),
                        constraints: String::new(),
                        max_rounds: 2,
                    },
                })
                .await
                .unwrap();
            let mut updates = task_handle.observe_goal().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                loop {
                    let state = updates.next().await.unwrap().unwrap();
                    if matches!(
                        state.stage,
                        rsi_goal::GoalDriverStage::Disarmed | rsi_goal::GoalDriverStage::Failed
                    ) {
                        assert_eq!(state.stage, rsi_goal::GoalDriverStage::Disarmed);
                        break;
                    }
                }
            })
            .await
            .unwrap();
        } else {
            let mut request = input("first-turn");
            if scenario == Scenario::WholeTurn {
                request.model =
                    Some(rsi_ai_protocol::ModelRef::new("fixture", "fixture-model").unwrap());
                request.reasoning_effort =
                    Some(rsi_ai_protocol::ReasoningEffortId::new("low").unwrap());
            }
            let receipt = task_handle.submit(request).await.unwrap();
            wait_receipt(&task_handle, &receipt).await;
        }
    });
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, ("fixture-model".into(), Some("low".into())));
    let queued = if scenario == Scenario::Queued {
        Some(
            handle
                .submit(input("queued-before-selection"))
                .await
                .unwrap(),
        )
    } else {
        None
    };
    select(&handle, "select-second").await;
    release.notify_one();
    task.await.unwrap();
    if retry {
        assert!(
            received.try_recv().is_err(),
            "dispatched failures must not be retried"
        );
    } else {
        let second = tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
            .await
            .unwrap()
            .unwrap();
        let expected = if scenario == Scenario::WholeTurn {
            ("fixture-model".into(), Some("low".into()))
        } else {
            ("second".into(), Some("high".into()))
        };
        assert_eq!(second, expected);
    }
    if let Some(receipt) = queued {
        wait_receipt(&handle, &receipt).await;
    } else if scenario != Scenario::Goal {
        run_message_to_terminal(&handle, "next-turn").await;
    }
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
            .await
            .unwrap()
            .unwrap(),
        ("second".into(), Some("high".into()))
    );
    let history = handle.history_before(None, 128).await.unwrap();
    let snapshots = history
        .facts
        .iter()
        .filter_map(|fact| {
            if let SessionFactBody::ModelIntent { snapshot, .. } = fact.body() {
                Some(snapshot)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(snapshots.len(), if retry { 2 } else { 3 });
    assert_eq!(
        snapshots[1].model,
        if scenario == Scenario::WholeTurn {
            "fixture-model"
        } else {
            "second"
        }
    );
    assert_eq!(
        snapshots[1]
            .language_settings
            .as_ref()
            .unwrap()
            .effective_reasoning_effort
            .as_ref()
            .unwrap()
            .as_str(),
        if scenario == Scenario::WholeTurn {
            "low"
        } else {
            "high"
        }
    );
    let metrics = handle.metrics().await.unwrap();
    metrics.validate().unwrap();
    assert!(metrics.complete);
    assert_eq!(metrics.summary.attempts, if retry { 2 } else { 3 });
    assert_eq!(metrics.summary.reported_attempts, if retry { 1 } else { 3 });
    assert_eq!(
        metrics.summary.tokens.input_tokens(),
        if retry { 5 } else { 15 }
    );
    assert_eq!(
        metrics
            .summary
            .last_context
            .as_ref()
            .unwrap()
            .description
            .model()
            .model(),
        "second"
    );
    let session_id = handle.header().await.unwrap().session_id().clone();
    assert!(running.shutdown().await.is_clean());
    drop(handle);
    let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let restored = restarted
        .session_service()
        .unwrap()
        .attach(&session_id)
        .await
        .unwrap();
    let restored_choice = restored.metrics().await.unwrap().current_model;
    assert_eq!(restored_choice.selection.model.model(), "second");
    assert_eq!(restored_choice.effective_effort().unwrap().as_str(), "high");
    assert!(
        received.try_recv().is_err(),
        "cold reads do not execute an idle Session or re-arm a Goal"
    );
    run_message_to_terminal(&restored, "after-restart").await;
    assert_eq!(
        received.recv().await.unwrap(),
        ("second".into(), Some("high".into()))
    );
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn next_step_and_next_turn_use_the_new_durable_selection() {
    exercise(Scenario::NextStep).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatched_failure_is_not_retried_and_next_turn_uses_new_selection() {
    exercise(Scenario::DispatchedFailure).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One request description is compared through configuration change and Host restart.
async fn default_effort_is_described_and_changed_capacity_invalidates_context_after_restart() {
    let (endpoint, server) = provider().await;
    let fixture = fixture(&endpoint);
    let mut profile = std::fs::read_to_string(&fixture.profile).unwrap();
    profile.push_str("\n[steps.config.reasoning_efforts.fixture-model]\nsupported = [\"low\", \"high\"]\ndefault = \"low\"\n");
    std::fs::write(&fixture.profile, &profile).unwrap();
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let session = SessionId::new("default-effort-context").unwrap();
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
            session_id: session.clone(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Trusted,
        })
        .await
        .unwrap();
    let draft = handle.metrics().await.unwrap();
    draft.validate().unwrap();
    assert_eq!(
        draft.current_model.effective_effort().unwrap().as_str(),
        "low"
    );
    assert_eq!(draft.summary.attempts, 0);
    assert!(draft.current_model.selection.reasoning_effort.is_none());
    run_message_to_terminal(&handle, "first-capacity").await;
    let first = handle.metrics().await.unwrap();
    let old_description = first
        .summary
        .last_context
        .as_ref()
        .unwrap()
        .description
        .clone();
    assert_eq!(
        first
            .summary
            .last_context
            .as_ref()
            .unwrap()
            .input_capacity(),
        128_000 - 4096
    );
    assert!(running.shutdown().await.is_clean());
    drop(handle);
    assert!(profile.contains("context_window_tokens = 128000"));
    let changed = profile
        .replace(
            "context_window_tokens = 128000",
            "context_window_tokens = 64000",
        )
        .replace("default = \"low\"", "default = \"high\"");
    std::fs::write(&fixture.profile, changed).unwrap();
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .attach(&session)
        .await
        .unwrap();
    let read = handle.metrics().await.unwrap();
    read.validate().unwrap();
    assert_eq!(
        read.current_model.effective_effort().unwrap().as_str(),
        "high"
    );
    let rsi_session_protocol::ModelAvailability::Available { description } =
        &read.current_model.availability
    else {
        panic!("route available")
    };
    // Runtime allocation order need not repeat across startup. The semantic
    // profile must still invalidate context if that numeric generation collides.
    let mut same_number = serde_json::to_value(description).unwrap();
    same_number["config_generation"] = old_description.config_generation().into();
    let same_number: rsi_ai_protocol::LanguageModelDescription =
        serde_json::from_value(same_number).unwrap();
    assert_ne!(same_number, old_description);
    assert!(read.summary.last_context.is_none());
    assert_eq!(read.summary.tokens, first.summary.tokens);
    run_message_to_terminal(&handle, "new-capacity").await;
    let read = handle.metrics().await.unwrap();
    assert_eq!(
        read.summary.last_context.as_ref().unwrap().input_capacity(),
        64000 - 4096
    );
    assert_eq!(
        read.summary
            .last_attempt
            .as_ref()
            .unwrap()
            .reasoning_effort
            .as_ref()
            .unwrap()
            .as_str(),
        "high"
    );
    assert!(running.shutdown().await.is_clean());
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_whole_turn_override_precedes_changed_domain_but_does_not_replace_it() {
    exercise(Scenario::WholeTurn).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_input_uses_execution_time_selection_and_cold_recovery_retains_it() {
    exercise(Scenario::Queued).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_continuation_uses_latest_selection_and_cold_reads_do_not_rearm() {
    exercise(Scenario::Goal).await;
}
