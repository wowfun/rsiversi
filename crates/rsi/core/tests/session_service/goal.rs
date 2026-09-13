use super::*;
use rsi_agent_goal::{GoalAction, GoalPhase, GoalState};
use rsi_agent_session_protocol::{DomainRequestId, InputMessageSource};
use rsi_goal::{GoalControl, GoalDriverStage};
use rsi_session_protocol::SessionHandle;

async fn create(running: &RunningRsi, fixture: &Fixture, id: &str) -> Arc<dyn SessionHandle> {
    running
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
            session_id: SessionId::new(id).unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap()
}

async fn state(handle: &Arc<dyn SessionHandle>) -> GoalState {
    let snapshot = handle
        .observe_projections()
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    let entry = snapshot
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == rsi_agent_goal::GOAL_PROJECTION)
        .unwrap();
    serde_json::from_value(entry.view().unwrap().value().clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_abandons_unaccepted_draft_or_published_allocation_without_provider_spend() {
    use rsi_agent_session_protocol::{CommandArguments, ContributionId, SessionCommandInvocation};
    for published in [false, true] {
        let (endpoint, requests, provider) = capturing_provider().await;
        let fixture = fixture(&endpoint);
        let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
            .await
            .unwrap();
        let handle = create(&running, &fixture, "abandon-unaccepted").await;
        let stage = |goal: &str| GoalAction::Create {
            id: DomainRequestId::new(goal).unwrap(),
            objective: "Verify task".into(),
            constraints: String::new(),
            max_rounds: 1,
        };
        handle
            .execute_command(SessionCommandInvocation {
                command: ContributionId::new(rsi_agent_goal::GOAL_COMMAND).unwrap(),
                request_id: DomainRequestId::new("stage-original").unwrap(),
                expected_revision: handle.commands().await.unwrap().revision(),
                arguments: CommandArguments::new(serde_json::to_value(stage("test-goal")).unwrap())
                    .unwrap(),
            })
            .await
            .unwrap();
        if published {
            run_message_to_terminal(&handle, "human-first").await;
        }
        let before = requests.lock().unwrap().len();
        control(
            &handle,
            "cancel-unaccepted",
            GoalAction::Cancel {
                id: DomainRequestId::new("test-goal").unwrap(),
            },
        )
        .await;
        let goal = state(&handle).await.goal.unwrap();
        assert_eq!(goal.allocated_rounds, 1);
        assert_eq!(
            goal.reservation.unwrap().settlement,
            Some(rsi_agent_goal::RoundSettlement::Abandoned)
        );
        assert!(!handle.goal_status().await.unwrap().armed);
        handle
            .execute_command(SessionCommandInvocation {
                command: ContributionId::new(rsi_agent_goal::GOAL_COMMAND).unwrap(),
                request_id: DomainRequestId::new("stage-replacement").unwrap(),
                expected_revision: handle.commands().await.unwrap().revision(),
                arguments: CommandArguments::new(
                    serde_json::to_value(stage("replacement")).unwrap(),
                )
                .unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(
            state(&handle).await.goal.unwrap().id.as_str(),
            "replacement"
        );
        assert_eq!(requests.lock().unwrap().len(), before);
        drop(handle);
        assert!(running.shutdown().await.is_clean());
        provider.abort();
    }
}

async fn start(handle: &Arc<dyn SessionHandle>, rounds: u64) -> rsi_goal::GoalControlReceipt {
    handle
        .control_goal(GoalControl {
            request_id: DomainRequestId::new("create-goal").unwrap(),
            expected_revision: handle.commands().await.unwrap().revision(),
            action: GoalAction::Create {
                id: DomainRequestId::new("test-goal").unwrap(),
                objective: "Produce a verified task result".into(),
                constraints: "Use only the fixture workspace".into(),
                max_rounds: rounds,
            },
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_draft_publication_then_restart_resume_reuses_first_goal_allocation() {
    use rsi_agent_session_protocol::{CommandArguments, ContributionId, SessionCommandInvocation};
    let (endpoint, requests, provider) = capturing_provider().await;
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "unadmitted-baseline").await;
    // Stage through the public command seam: exactly the durable state before
    // the controller's first submit, without a timing race in this regression.
    handle
        .execute_command(SessionCommandInvocation {
            command: ContributionId::new(rsi_agent_goal::GOAL_COMMAND).unwrap(),
            request_id: DomainRequestId::new("stage-goal").unwrap(),
            expected_revision: handle.commands().await.unwrap().revision(),
            arguments: CommandArguments::new(
                serde_json::to_value(GoalAction::Create {
                    id: DomainRequestId::new("test-goal").unwrap(),
                    objective: "Verify the task".into(),
                    constraints: String::new(),
                    max_rounds: 1,
                })
                .unwrap(),
            )
            .unwrap(),
        })
        .await
        .unwrap();
    let original = state(&handle).await.goal.unwrap().reservation.unwrap();
    assert!(original.request_id.is_none());
    run_message_to_terminal(&handle, "human-first").await;
    assert_eq!(
        state(&handle).await.goal.unwrap().reservation.as_ref(),
        Some(&original)
    );
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("unadmitted-baseline").unwrap())
        .await
        .unwrap();
    assert!(!handle.goal_status().await.unwrap().armed);
    control(
        &handle,
        "resume-baseline",
        GoalAction::Resume {
            id: DomainRequestId::new("test-goal").unwrap(),
        },
    )
    .await;
    let live = stopped(&handle).await;
    assert_eq!(live.stage, GoalDriverStage::Disarmed, "{live:?}");
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.allocated_rounds, 1);
    assert_eq!(goal.phase, GoalPhase::Blocked);
    let settled = goal.reservation.unwrap();
    assert_eq!(settled.message_id, original.message_id);
    assert_eq!(settled.input(&goal.id), original.input(&goal.id));
    assert!(settled.request_id.is_some() && settled.settlement.is_some());
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

async fn stopped(handle: &Arc<dyn SessionHandle>) -> rsi_goal::GoalLiveState {
    let mut stream = handle.observe_goal().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let state = stream.next().await.unwrap().unwrap();
            if matches!(
                state.stage,
                GoalDriverStage::Disarmed | GoalDriverStage::Failed
            ) {
                return state;
            }
        }
    })
    .await
    .expect("Goal did not settle")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draft_goal_drives_exact_allocations_and_restart_reads_do_not_arm() {
    let (endpoint, requests, provider) = capturing_provider().await;
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "goal-rounds").await;
    assert_eq!(state(&handle).await, GoalState::default());
    assert!(!handle.goal_status().await.unwrap().armed);
    let receipt = start(&handle, 2).await;
    assert!(receipt.live.armed);
    let live = stopped(&handle).await;
    assert_eq!(live.stage, GoalDriverStage::Disarmed, "{live:?}");
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.allocated_rounds, 2);
    assert_eq!(goal.phase, GoalPhase::Blocked);
    assert!(goal.reservation.unwrap().settlement.is_some());
    assert_eq!(requests.lock().unwrap().len(), 2);
    let history = handle.history_before(None, 128).await.unwrap();
    assert!(!history.has_more);
    assert_eq!(
        history
            .facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::Continuation { .. },
                    ..
                }
            ))
            .count(),
        2
    );
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("goal-rounds").unwrap())
        .await
        .unwrap();
    assert!(!handle.goal_status().await.unwrap().armed);
    assert_eq!(state(&handle).await.goal.unwrap().allocated_rounds, 2);
    handle
        .command_status(&DomainRequestId::new("create-goal").unwrap())
        .await
        .unwrap();
    assert!(!handle.goal_status().await.unwrap().armed);
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

async fn reporting_chat(Json(request): Json<serde_json::Value>) -> Response {
    // ExclusiveFinal orders the Tool batch; the provider still closes the Turn.
    if request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool")
    {
        return chat().await;
    }
    let arguments = serde_json::json!({"goal_id":"test-goal","kind":"complete","evidence":"The fixed fixture result was checked"}).to_string();
    let call = serde_json::json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"goal-report","type":"function","function":{"name":"report_goal","arguments":arguments}}]},"finish_reason":null}]});
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
async fn real_report_tool_completes_only_after_its_source_turn() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(reporting_chat)),
        )
        .await
        .unwrap();
    });
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "goal-report").await;
    start(&handle, 3).await;
    let live = stopped(&handle).await;
    assert_eq!(live.stage, GoalDriverStage::Disarmed, "{live:?}");
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.phase, GoalPhase::Completed, "{goal:?}");
    assert_eq!(goal.allocated_rounds, 1);
    let report = goal.report.unwrap();
    let history = handle.history_before(None, 128).await.unwrap();
    assert!(history.facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { turn_id, outcome: rsi_agent_session_protocol::TurnOutcome::Completed } if turn_id == &report.source_turn)));
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[derive(Default)]
struct Gate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    requests: std::sync::atomic::AtomicUsize,
}

async fn gated_chat(State(gate): State<Arc<Gate>>) -> Response {
    gate.requests
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    gate.entered.notify_one();
    gate.release.notified().await;
    chat().await
}

async fn gated() -> (Fixture, Arc<Gate>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let gate = Arc::new(Gate::default());
    let router = Router::new()
        .route("/v1/chat/completions", post(gated_chat))
        .with_state(gate.clone());
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (fixture, gate, provider)
}

async fn entered(gate: &Gate) {
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
}

async fn control(handle: &Arc<dyn SessionHandle>, id: &str, action: GoalAction) {
    handle
        .control_goal(GoalControl {
            request_id: DomainRequestId::new(id).unwrap(),
            expected_revision: handle.commands().await.unwrap().revision(),
            action,
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_keeps_claimed_round_running_then_resume_preserves_allocation() {
    let (fixture, gate, provider) = gated().await;
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "goal-pause").await;
    start(&handle, 2).await;
    entered(&gate).await;
    control(
        &handle,
        "pause-goal",
        GoalAction::Pause {
            id: DomainRequestId::new("test-goal").unwrap(),
        },
    )
    .await;
    assert!(!handle.goal_status().await.unwrap().armed);
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.phase, GoalPhase::Paused);
    assert!(goal.reservation.unwrap().settlement.is_none());
    gate.release.notify_one();
    let live = stopped(&handle).await;
    assert_eq!(live.stage, GoalDriverStage::Disarmed, "{live:?}");
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.allocated_rounds, 1);
    assert!(matches!(
        goal.reservation.unwrap().settlement,
        Some(rsi_agent_goal::RoundSettlement::Turn {
            outcome: rsi_agent_goal::RoundOutcome::Completed,
            ..
        })
    ));
    control(
        &handle,
        "resume-goal",
        GoalAction::Resume {
            id: DomainRequestId::new("test-goal").unwrap(),
        },
    )
    .await;
    entered(&gate).await;
    gate.release.notify_one();
    assert_eq!(stopped(&handle).await.stage, GoalDriverStage::Disarmed);
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.allocated_rounds, 2);
    assert_eq!(goal.phase, GoalPhase::Blocked);
    assert_eq!(gate.requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_targets_claimed_goal_and_settles_without_another_round() {
    let (fixture, gate, provider) = gated().await;
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "goal-cancel").await;
    start(&handle, 3).await;
    entered(&gate).await;
    let message = state(&handle)
        .await
        .goal
        .unwrap()
        .reservation
        .unwrap()
        .message_id;
    let rsi_agent_turn_protocol::MessageState::Claimed { turn_id, .. } =
        handle.message_status(&message).await.unwrap().state
    else {
        panic!("provider gate requires a claimed Turn");
    };
    let jobs_request = rsi_agent_turn_protocol::TurnJobsRequest {
        turn_id,
        generation: None,
        after: None,
        limit: 32,
    };
    assert!(
        handle
            .read_jobs(jobs_request.clone())
            .await
            .unwrap()
            .page()
            .jobs
            .is_empty()
    );
    control(
        &handle,
        "cancel-goal",
        GoalAction::Cancel {
            id: DomainRequestId::new("test-goal").unwrap(),
        },
    )
    .await;
    let live = stopped(&handle).await;
    assert_eq!(live.stage, GoalDriverStage::Disarmed, "{live:?}");
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.phase, GoalPhase::Paused);
    assert_eq!(goal.allocated_rounds, 1);
    assert!(matches!(
        handle.read_jobs(jobs_request).await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        goal.reservation.unwrap().settlement,
        Some(rsi_agent_goal::RoundSettlement::Turn {
            outcome: rsi_agent_goal::RoundOutcome::Cancelled,
            ..
        })
    ));
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detach_leaves_host_driver_owned_and_host_shutdown_settles_it() {
    let (fixture, gate, provider) = gated().await;
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "goal-detach").await;
    start(&handle, 3).await;
    entered(&gate).await;
    drop(handle);
    let attached = running
        .session_service()
        .unwrap()
        .attach(&SessionId::new("goal-detach").unwrap())
        .await
        .unwrap();
    assert!(attached.goal_status().await.unwrap().armed);
    let report = running.shutdown().await;
    assert!(report.is_clean(), "{report:?}");
    // A process restart releases caller-held Store owners as well as Host supplies.
    drop(attached);
    let restarted = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let attached = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("goal-detach").unwrap())
        .await
        .unwrap();
    assert!(!attached.goal_status().await.unwrap().armed);
    let goal = state(&attached).await.goal.unwrap();
    assert_eq!(goal.allocated_rounds, 1);
    assert!(goal.reservation.unwrap().settlement.is_some());
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_goal_control_and_observation_use_the_same_host_driver() {
    let (endpoint, requests, provider) = capturing_provider().await;
    let fixture = fixture(&endpoint);
    let daemon = DaemonFixture::new(&fixture).await;
    let workspace_id = daemon
        .running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap()
        .id;
    let handle = daemon
        .connection
        .session_service()
        .create(CreateSession {
            workspace_id,
            session_id: SessionId::new("remote-goal").unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    start(&handle, 2).await;
    assert_eq!(stopped(&handle).await.stage, GoalDriverStage::Disarmed);
    assert_eq!(state(&handle).await.goal.unwrap().allocated_rounds, 2);
    assert_eq!(requests.lock().unwrap().len(), 2);
    daemon.shutdown().await;
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_stale_cancel_is_known_rejection_and_fresh_cancel_settles_exact_round() {
    let (fixture, gate, provider) = gated().await;
    let daemon = DaemonFixture::new(&fixture).await;
    let local = create(&daemon.running, &fixture, "remote-stale-cancel").await;
    let handle = daemon
        .connection
        .session_service()
        .attach(local.header().await.unwrap().session_id())
        .await
        .unwrap();
    start(&handle, 3).await;
    entered(&gate).await;
    let request_id = DomainRequestId::new("stale-cancel").unwrap();
    let rejected = handle
        .control_goal(GoalControl {
            request_id: request_id.clone(),
            expected_revision: rsi_agent_session_protocol::CommandRevision::Durable {
                control_seq: 1,
            },
            action: GoalAction::Cancel {
                id: DomainRequestId::new("test-goal").unwrap(),
            },
        })
        .await;
    assert!(
        matches!(&rejected, Err(SessionError::CommandRevisionConflict { expected: rsi_agent_session_protocol::CommandRevision::Durable { control_seq: 1 }, actual: rsi_agent_session_protocol::CommandRevision::Durable { control_seq } }) if *control_seq > 1),
        "{rejected:?}"
    );
    assert!(handle.command_status(&request_id).await.unwrap().is_none());
    assert!(!handle.goal_status().await.unwrap().armed);
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.phase, GoalPhase::Active);
    assert!(goal.reservation.unwrap().settlement.is_none());
    control(
        &handle,
        "fresh-cancel",
        GoalAction::Cancel {
            id: DomainRequestId::new("test-goal").unwrap(),
        },
    )
    .await;
    assert_eq!(stopped(&handle).await.stage, GoalDriverStage::Disarmed);
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.allocated_rounds, 1);
    assert!(matches!(
        goal.reservation.unwrap().settlement,
        Some(rsi_agent_goal::RoundSettlement::Turn {
            outcome: rsi_agent_goal::RoundOutcome::Cancelled,
            ..
        })
    ));
    assert_eq!(gate.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    daemon.shutdown().await;
    provider.abort();
}

async fn report_then_broken_stream(Json(request): Json<serde_json::Value>) -> Response {
    if request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool")
    {
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from("data: {invalid-json}\n\n"))
            .unwrap();
    }
    reporting_chat(Json(request)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_source_turn_overrides_model_completion_claim() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(report_then_broken_stream)),
        )
        .await
        .unwrap();
    });
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = create(&running, &fixture, "goal-failed-report").await;
    start(&handle, 3).await;
    assert_eq!(stopped(&handle).await.stage, GoalDriverStage::Disarmed);
    let goal = state(&handle).await.goal.unwrap();
    assert_eq!(goal.phase, GoalPhase::Blocked);
    assert_eq!(goal.allocated_rounds, 1);
    assert!(goal.report.is_some());
    assert!(matches!(
        goal.reservation.unwrap().settlement,
        Some(rsi_agent_goal::RoundSettlement::Turn {
            outcome: rsi_agent_goal::RoundOutcome::Failed,
            ..
        })
    ));
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
