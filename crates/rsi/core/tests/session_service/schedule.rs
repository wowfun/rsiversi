use super::*;
use rsi_agent_schedule::ScheduleState;
use rsi_session_protocol::SessionHandle;
use std::sync::atomic::{AtomicUsize, Ordering};
fn response(call: Option<(&str, &str, serde_json::Value)>) -> Response {
    let (delta, reason) = match call {
        Some((id, name, args)) => (
            serde_json::json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
            "tool_calls",
        ),
        None => (
            serde_json::json!({"role":"assistant","content":"reminder checked"}),
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
        })
        .await
        .unwrap()
}
async fn state(handle: &Arc<dyn SessionHandle>) -> ScheduleState {
    let snapshot = handle
        .observe_projections()
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    let view = snapshot
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == "rsi.schedule.projection")
        .unwrap()
        .view()
        .unwrap();
    serde_json::from_value(view.value().clone()).unwrap()
}
async fn settled(handle: &Arc<dyn SessionHandle>) -> ScheduleState {
    let mut changes = handle.observe_projections().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let state = state(handle).await;
            if state.reservation.as_ref().is_some_and(|r| r.settled) {
                return state;
            }
            changes.next().await.unwrap().unwrap();
        }
    })
    .await
    .expect("Schedule round must settle")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn due_reminder_uses_normal_turn_and_cannot_create_a_successor() {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let counter = calls.clone();
    let captured = requests.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        captured.lock().unwrap().push(body);
        let index = counter.fetch_add(1, Ordering::SeqCst);
        async move { match index {
            0 => response(Some(("create", "schedule_create", serde_json::json!({"prompt":"Check the requested build", "rule":{"kind":"after","delay_ms":1}})))),
            2 => response(Some(("chain", "schedule_create", serde_json::json!({"prompt":"Unauthorized successor", "rule":{"kind":"after","delay_ms":1}})))),
            _ => response(None),
        } }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let running = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = create(&running, &fixture, "schedule-once").await;
    run_message_to_terminal(&handle, "create-reminder").await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(state(&handle).await.allocated_rounds, 0);
    clock.advance(1);
    let state = settled(&handle).await;
    assert_eq!(state.allocated_rounds, 1);
    assert_eq!(state.reminders.len(), 1);
    assert!(state.reminders[0].consumed);
    let history = handle.history_before(None, 512).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(history.facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { outcome: rsi_agent_session_protocol::TurnOutcome::Failed { code, message }, .. }
        if code == "tool.execution" && message.contains("initial human-origin root Turn"))));
    assert!(history.facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::InputMessageEntered { source: rsi_agent_session_protocol::InputMessageSource::Continuation { source, .. }, .. } if source.domain.id() == rsi_agent_schedule::SCHEDULE_DOMAIN)));
    let request = requests.lock().unwrap()[2].clone();
    assert!(
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "bash")
    );
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[derive(Debug)]
struct ManualClock {
    time: tokio::sync::watch::Sender<u64>,
}
impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            time: tokio::sync::watch::channel(1_000_000).0,
        })
    }
    fn advance(&self, delta: u64) {
        self.time.send_modify(|now| *now += delta);
    }
}
#[async_trait::async_trait]
impl rsi_agent_schedule::ScheduleClock for ManualClock {
    fn now_ms(&self) -> u64 {
        *self.time.borrow()
    }
    async fn wait_until(&self, at: u64, cancellation: CancellationToken) {
        let mut time = self.time.subscribe();
        loop {
            if *time.borrow_and_update() >= at {
                return;
            }
            tokio::select! { () = cancellation.cancelled() => return, changed = time.changed() => if changed.is_err() { return; } }
        }
    }
}
fn select_clock_profile(fixture: &Fixture) {
    let mut profile = std::fs::read_to_string(&fixture.profile).unwrap();
    profile.push_str("\n[[steps]]\nkind='patch'\ntarget='rsi-schedule-controller'\nenabled=false\n\n[[steps]]\nkind='plugin'\nid='fixture-schedule'\nplugin='fixture.schedule.controller'\n");
    std::fs::write(&fixture.profile, profile).unwrap();
}
fn composition_with_clock(fixture: &Fixture, clock: Arc<ManualClock>) -> StandardComposition {
    let mut addon = rsi::StandardAddonBuilder::new("fixture.schedule");
    addon
        .register_linked(
            "fixture.schedule.controller",
            "1",
            rsi_meta::UpdateMode::RestartRequired,
            Arc::new(rsi_schedule::ScheduleControllerFactory::with_clock(clock)),
        )
        .unwrap();
    composition(fixture.paths.clone())
        .with_addons(rsi::StandardAddonSet::new([addon.build().unwrap()]).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_keeps_overdue_intent_disarmed_until_selected_human_resume() {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let selected = Arc::new(Mutex::new(None::<String>));
    let counter = calls.clone();
    let captured = requests.clone();
    let choice = selected.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        captured.lock().unwrap().push(body);
        let index = counter.fetch_add(1, Ordering::SeqCst);
        let choice = choice.clone();
        async move { match index {
            0 => response(Some(("create", "schedule_create", serde_json::json!({"prompt":"Check retained intent", "rule":{"kind":"after","delay_ms":1000}})))),
            2 => response(Some(("inspect", "schedule_list", serde_json::json!({})))),
            3 => response(Some(("resume", "schedule_resume", serde_json::json!({"ids":[choice.lock().unwrap().clone().unwrap()]})))),
            _ => response(None),
        } }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let running = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = create(&running, &fixture, "schedule-restart").await;
    run_message_to_terminal(&handle, "create-reminder").await;
    let before = state(&handle).await;
    assert_eq!(before.allocated_rounds, 0);
    *selected.lock().unwrap() = Some(before.reminders[0].id.to_string());
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    clock.advance(5000);
    let restarted = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("schedule-restart").unwrap())
        .await
        .unwrap();
    assert_eq!(state(&handle).await, before);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    run_message_to_terminal(&handle, "explicitly-resume-selected-reminder").await;
    let after = settled(&handle).await;
    assert_eq!(after.allocated_rounds, 1);
    assert!(after.reminders[0].consumed);
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    {
        let requests = requests.lock().unwrap();
        let listed: serde_json::Value = serde_json::from_str(
            requests[3]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["tool_call_id"] == "inspect")
                .unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(listed["armed"], false);
    }
    drop(handle);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[derive(Debug, Default)]
struct ArmGate {
    block_disarm: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    turn: Mutex<Option<TurnId>>,
    armed: Mutex<Option<bool>>,
}
#[derive(Debug)]
struct GatedController {
    service: Arc<rsi_schedule::ScheduleService>,
    gate: Arc<ArmGate>,
}
#[async_trait::async_trait]
impl rsi_agent_schedule::ScheduleController for GatedController {
    fn epoch(&self) -> rsi_agent_schedule::ScheduleEpoch {
        self.service.epoch()
    }
    fn clock(&self) -> Arc<dyn rsi_agent_schedule::ScheduleClock> {
        self.service.clock()
    }
    fn armed(&self, session: &SessionId) -> bool {
        self.service.armed(session)
    }
    fn failure(&self, session: &SessionId) -> Option<String> {
        self.service.failure(session)
    }
    async fn disarm_for_mutation(
        &self,
        epoch: &rsi_agent_schedule::ScheduleEpoch,
        session: &SessionId,
    ) -> Result<bool, String> {
        if self.gate.block_disarm.load(Ordering::SeqCst) {
            self.gate.entered.notify_one();
            self.gate.release.notified().await;
        }
        self.service.disarm_for_mutation(epoch, session).await
    }
    async fn arm_after_commit(
        &self,
        epoch: &rsi_agent_schedule::ScheduleEpoch,
        caller: &rsi_agent_turn_protocol::AgentCallerAuthority,
        domain: &rsi_agent_session_protocol::DomainIdentity,
        receipt: &rsi_agent_turn_protocol::DomainMutationReceipt,
        cancellation: CancellationToken,
    ) -> Result<bool, String> {
        *self.gate.turn.lock().unwrap() = Some(caller.turn_id().clone());
        self.gate.entered.notify_one();
        self.gate.release.notified().await;
        let armed = self
            .service
            .arm_after_commit(epoch, caller, domain, receipt, cancellation)
            .await?;
        *self.gate.armed.lock().unwrap() = Some(armed);
        Ok(armed)
    }
}
#[derive(Debug)]
struct GatedFactory {
    clock: Arc<ManualClock>,
    gate: Arc<ArmGate>,
}
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for GatedFactory {
    fn prepare(
        &self,
        desired: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        use rsi_agent_turn_protocol::{
            SessionCommandsContract, SessionContinuationsContract, SessionProjectionsContract,
            TurnExecutionContract, TurnServiceContract,
        };
        assert!(desired.is_null());
        Ok(
            rsi_meta::PreparedActivation::new(rsi_meta::ConfigValue::Null)
                .requiring_local::<TurnServiceContract>()
                .requiring_local::<TurnExecutionContract>()
                .requiring_local::<SessionCommandsContract>()
                .requiring_local::<SessionContinuationsContract>()
                .requiring_local::<SessionProjectionsContract>(),
        )
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        use rsi_agent_turn_protocol::{
            SessionCommandsContract, SessionContinuationsContract, SessionProjectionsContract,
            TurnExecutionContract, TurnServiceContract,
        };
        let service = Arc::new(rsi_schedule::ScheduleService::new(
            plan.local::<TurnServiceContract>()?,
            plan.local::<TurnExecutionContract>()?,
            plan.local::<SessionCommandsContract>()?,
            plan.local::<SessionContinuationsContract>()?,
            plan.local::<SessionProjectionsContract>()?,
            self.clock.clone(),
        ));
        let supply = plan
            .context()
            .provide_local::<rsi_agent_schedule::ScheduleControllerContract>(Arc::new(
                GatedController {
                    service: service.clone(),
                    gate: self.gate.clone(),
                },
            ))?;
        plan.defer(
            "stop fixture Schedule",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    service.stop().await
                })
            }),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_after_durable_create_before_arm_retains_disarmed_intent() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        async move { if index == 0 { response(Some(("create", "schedule_create", serde_json::json!({"prompt":"Must remain disarmed", "rule":{"kind":"after","delay_ms":1}})))) } else { response(None) } }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let clock = ManualClock::new();
    let gate = Arc::new(ArmGate::default());
    select_clock_profile(&fixture);
    let mut addon = rsi::StandardAddonBuilder::new("fixture.schedule");
    addon
        .register_linked(
            "fixture.schedule.controller",
            "1",
            rsi_meta::UpdateMode::RestartRequired,
            Arc::new(GatedFactory {
                clock: clock.clone(),
                gate: gate.clone(),
            }),
        )
        .unwrap();
    let running = RunningRsi::boot(
        composition(fixture.paths.clone())
            .with_addons(rsi::StandardAddonSet::new([addon.build().unwrap()]).unwrap()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = create(&running, &fixture, "schedule-cancel-before-arm").await;
    let task = tokio::spawn({
        let handle = handle.clone();
        async move { run_message_to_terminal(&handle, "cancel-before-arm").await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    let before = state(&handle).await;
    assert_eq!(before.reminders.len(), 1);
    assert_eq!(before.allocated_rounds, 0);
    let turn = gate.turn.lock().unwrap().clone().unwrap();
    handle
        .cancel(rsi_agent_turn_protocol::CancelTarget::Turn(turn), None)
        .await
        .unwrap();
    clock.advance(10);
    gate.release.notify_one();
    task.await.unwrap();
    assert_eq!(*gate.armed.lock().unwrap(), Some(false));
    assert_eq!(state(&handle).await, before);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_settles_a_claimed_one_shot_without_rearming_it_on_restart() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let observed = entered.clone();
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        let entered = observed.clone();
        async move {
            match index {
                0 => response(Some(("create", "schedule_create", serde_json::json!({"prompt":"Check once", "rule":{"kind":"after","delay_ms":1}})))),
                2 => { entered.notify_one(); std::future::pending::<Response>().await },
                _ => response(None),
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let running = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = create(&running, &fixture, "schedule-shutdown-round").await;
    run_message_to_terminal(&handle, "create-reminder").await;
    clock.advance(1);
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    assert!(!state(&handle).await.reservation.unwrap().settled);
    drop(handle);
    let shutdown = tokio::time::timeout(std::time::Duration::from_secs(25), running.shutdown())
        .await
        .unwrap();
    assert!(shutdown.is_clean(), "{shutdown:?}");
    let restarted = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("schedule-shutdown-round").unwrap())
        .await
        .unwrap();
    let after = state(&handle).await;
    assert_eq!(after.allocated_rounds, 1);
    assert!(after.reminders[0].consumed && !after.reminders[0].active);
    assert!(after.reservation.unwrap().settled);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    drop(handle);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One gated product race verifies both the committed policy and rejected Schedule state."
)]
async fn plan_enablement_while_schedule_disarm_is_blocked_rejects_the_mutation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        async move { if index == 0 { response(Some(("create", "schedule_create", serde_json::json!({"prompt":"Must remain disarmed", "rule":{"kind":"after","delay_ms":1}})))) } else { response(None) } }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let clock = ManualClock::new();
    let gate = Arc::new(ArmGate::default());
    gate.block_disarm.store(true, Ordering::SeqCst);
    select_clock_profile(&fixture);
    let mut addon = rsi::StandardAddonBuilder::new("fixture.schedule");
    addon
        .register_linked(
            "fixture.schedule.controller",
            "1",
            rsi_meta::UpdateMode::RestartRequired,
            Arc::new(GatedFactory {
                clock: clock.clone(),
                gate: gate.clone(),
            }),
        )
        .unwrap();
    let running = RunningRsi::boot(
        composition(fixture.paths.clone())
            .with_addons(rsi::StandardAddonSet::new([addon.build().unwrap()]).unwrap()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = create(&running, &fixture, "schedule-plan-race").await;
    let task = tokio::spawn({
        let handle = handle.clone();
        async move { run_message_to_terminal(&handle, "enable-plan-while-blocked").await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    let before = state(&handle).await;
    assert!(before.reminders.is_empty());
    let commands = handle.commands().await.unwrap();
    handle
        .execute_command(rsi_agent_session_protocol::SessionCommandInvocation {
            command: commands
                .commands()
                .iter()
                .find(|entry| entry.name() == "plan")
                .unwrap()
                .id()
                .clone(),
            request_id: rsi_agent_session_protocol::DomainRequestId::new("enable-plan").unwrap(),
            expected_revision: commands.revision(),
            arguments: rsi_agent_session_protocol::CommandArguments::new("on".into()).unwrap(),
        })
        .await
        .unwrap();
    let projections = handle
        .observe_projections()
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap();
    let policy = projections
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == "rsi.plan-policy.view")
        .unwrap()
        .view()
        .unwrap();
    assert_eq!(
        policy.value()["enabled"],
        true,
        "plan on must commit before the blocked Schedule mutation resumes"
    );
    gate.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state(&handle).await, before);
    assert_eq!(
        *gate.armed.lock().unwrap(),
        None,
        "rejected mutation must never arm"
    );
    let history = handle.history_before(None, 512).await.unwrap();
    assert!(
        history
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal {
        outcome: rsi_agent_session_protocol::TurnOutcome::Failed { message, .. }, ..
    } if message.contains("plan mode")))
    );
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
