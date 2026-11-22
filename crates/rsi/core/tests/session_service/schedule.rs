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
            fork: None,
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
    projected_state(&snapshot)
}
fn projected_state(snapshot: &rsi_session_protocol::ProjectionSnapshot) -> ScheduleState {
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
async fn settled(handle: &Arc<dyn SessionHandle>, round: u64) -> ScheduleState {
    let mut changes = handle.observe_projections().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let state = projected_state(&changes.next().await.unwrap().unwrap());
            if state.allocated_rounds == round
                && state.reservation.as_ref().is_some_and(|r| r.settled)
            {
                return state;
            }
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
    let state = settled(&handle, 1).await;
    assert_eq!(state.allocated_rounds, 1);
    assert_eq!(state.reminders.len(), 1);
    assert!(state.reminders[0].consumed);
    let history = handle.history_before(None, 512).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(history.facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { outcome: rsi_agent_session_protocol::TurnOutcome::Failed { code, message }, .. }
        if code == "tool.execution" && message.contains("initial human-origin root Turn"))));
    assert!(history.facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::InputMessageClaimed { source: rsi_agent_session_protocol::InputMessageSource::Continuation { source, .. }, .. } if source.domain.id() == rsi_agent_schedule::SCHEDULE_DOMAIN)));
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
async fn restart_delivers_an_overdue_reminder_once_without_attaching() {
    overdue_delivery_after_restart(RestartTarget::Active).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_leaves_an_archived_reminder_due_until_its_session_is_restored() {
    overdue_delivery_after_restart(RestartTarget::Archived).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_without_navigation_delivers_existing_enabled_reminders() {
    overdue_delivery_after_restart(RestartTarget::NoArchiveRegistry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_navigation_waits_for_restart_then_delivers_an_archived_reminder_once() {
    overdue_delivery_after_restart(RestartTarget::PendingRestart).await;
}

enum RestartTarget {
    Active,
    Archived,
    NoArchiveRegistry,
    PendingRestart,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operator_run_keeps_the_rule_holds_until_idle_and_recovers_its_floor() {
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().route("/v1/chat/completions", post({
        let (calls, entered, release) = (calls.clone(), entered.clone(), release.clone());
        move |Json(body): Json<serde_json::Value>| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            let (entered, release) = (entered.clone(), release.clone());
            async move {
                if index == 0 {
                    return response(Some(("task","schedule_create",serde_json::json!({
                        "prompt":"Check this release", "rule":{"kind":"after","delay_ms":900000}
                    }))));
                }
                if index == 2 {
                    assert!(body["messages"].to_string().contains("[SCHEDULE REMINDER]"));
                    entered.notify_one();
                    release.notified().await;
                }
                response(None)
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let boot = || {
        RunningRsi::boot(
            composition_with_clock(&fixture, clock.clone()),
            &fixture.profile,
        )
    };
    let running = boot().await.unwrap();
    let session = SessionId::new("schedule-restart").unwrap();
    let handle = create(&running, &fixture, session.as_str()).await;
    run_message_to_terminal(&handle, "create-rule").await;
    let before = state(&handle).await.reminders.remove(0);
    let task = || rsi_session_protocol::ScheduledTaskId {
        session_id: session.clone(),
        id: before.id.clone(),
    };
    let service = running.session_service().unwrap();
    set_archived(&running, true).await;
    assert!(
        service
            .run_scheduled_task(task())
            .await
            .unwrap_err()
            .to_string()
            .contains("archived")
    );
    set_archived(&running, false).await;
    assert!(service.run_scheduled_task(task()).await.unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let receipt = state(&handle).await;
    assert_eq!(receipt.allocated_rounds, 0);
    assert_eq!(receipt.reminders[0].due_ms, before.due_ms);
    assert!(receipt.reminders[0].active);
    assert!(service.scheduled_tasks(None).await.unwrap().sessions[0].reminders[0].executing);
    assert!(service.run_scheduled_task(task()).await.is_err());
    clock.advance(60000);
    assert!(
        service
            .run_scheduled_task(task())
            .await
            .unwrap_err()
            .to_string()
            .contains("active")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    release.notify_one();
    handle.wait_idle().await.unwrap();
    let mut changes = service.observe_scheduled_tasks().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(update) = changes.next().await {
            if matches!(update.unwrap(), rsi_session_protocol::ScheduleCatalogUpdate::Page { sessions, .. }
                if !sessions[0].reminders[0].executing) { return; }
        }
        panic!("Schedule observation ended before execution settled");
    }).await.unwrap();
    drop(changes);
    assert!(service.run_scheduled_task(task()).await.unwrap());
    handle.wait_idle().await.unwrap();
    let before_restart = state(&handle).await;
    drop(handle);
    drop(service);
    assert!(running.shutdown().await.is_clean());
    let restarted = boot().await.unwrap();
    let service = restarted.session_service().unwrap();
    assert!(
        service
            .run_scheduled_task(task())
            .await
            .unwrap_err()
            .to_string()
            .contains("interval")
    );
    let handle = service.attach(&session).await.unwrap();
    assert_eq!(state(&handle).await, before_restart);
    clock.advance(840000);
    let after = settled(&handle, 1).await;
    assert!(after.reminders[0].consumed);
    assert_eq!(after.reservation.unwrap().occurrence_ms, before.due_ms);
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    drop(handle);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operator_run_calls_the_model_after_the_automatic_allowance_is_spent() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/v1/chat/completions", post({
        let calls = calls.clone();
        move |Json(body): Json<serde_json::Value>| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if index == 0 {
                    return response(Some(("task", "schedule_create", serde_json::json!({
                        "prompt":"Report the time", "rule":{"kind":"every","interval_ms":60000}
                    }))));
                }
                if index == 102 {
                    assert!(body["messages"].to_string().contains("[SCHEDULE REMINDER]"));
                }
                response(None)
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let running = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let session = SessionId::new("schedule-allowance").unwrap();
    let handle = create(&running, &fixture, session.as_str()).await;
    run_message_to_terminal(&handle, "create-recurring").await;
    for round in 1..=100 {
        clock.advance(60_000);
        settled(&handle, round).await;
    }
    let before = state(&handle).await;
    assert!(before.exhausted());
    let task = &before.reminders[0];
    clock.advance(60_000);
    let service = running.session_service().unwrap();
    assert!(
        service
            .run_scheduled_task(rsi_session_protocol::ScheduledTaskId {
                session_id: session,
                id: task.id.clone(),
            })
            .await
            .unwrap()
    );
    handle.wait_idle().await.unwrap();
    let after = state(&handle).await;
    assert_eq!(after.allocated_rounds, 100);
    assert_eq!(after.reminders[0].due_ms, task.due_ms);
    assert_eq!(after.next_due(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 103);
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

fn disable_navigation(profile: &std::path::Path) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(profile)
        .unwrap()
        .write_all(b"\n[[steps]]\nkind='patch'\ntarget='rsi.navigation'\nenabled=false\n")
        .unwrap();
}

async fn set_archived(running: &RunningRsi, archived: bool) {
    let receipt = super::navigation::wire(
        running,
        rsi_api_protocol::CallOrigin::Local,
        rsi_navigation_api::NavigationOperation::Replace,
        serde_json::json!({"session":"schedule-restart","expected_revision":if archived { "0" } else { "1" },
            "metadata":{"title":null,"pinned":false,"archived":archived}}),
    ).await.unwrap();
    assert_eq!(receipt["revision"], if archived { "1" } else { "2" });
}

async fn overdue_delivery_after_restart(target: RestartTarget) {
    let calls = Arc::new(AtomicUsize::new(0));
    let delivered = Arc::new(tokio::sync::Notify::new());
    let counter = calls.clone();
    let observed = delivered.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        let observed = observed.clone();
        async move { match index {
            0 => response(Some(("create", "schedule_create", serde_json::json!({"prompt":"Check retained intent", "rule":{"kind":"after","delay_ms":1000}})))),
            2 => {
                assert!(body["messages"].as_array().unwrap().iter().any(|message|
                    message["role"] == "user" && message["content"].to_string().contains("Check retained intent")));
                observed.notify_one();
                response(None)
            }
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
    let mut changes = running
        .session_service()
        .unwrap()
        .observe_scheduled_tasks()
        .await
        .unwrap();
    use rsi_session_protocol::ScheduleCatalogUpdate;
    assert!(matches!(
        changes.next().await.unwrap().unwrap(),
        ScheduleCatalogUpdate::Complete { pages: 0 }
    ));
    run_message_to_terminal(&handle, "create-reminder").await;
    let before = state(&handle).await;
    assert_eq!(before.allocated_rounds, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(handle);
    let catalog = running
        .session_service()
        .unwrap()
        .scheduled_tasks(None)
        .await
        .unwrap();
    assert_eq!(catalog.sessions.len(), 1);
    assert_eq!(
        catalog.sessions[0].position.session_id.as_str(),
        "schedule-restart"
    );
    assert_eq!(
        catalog.sessions[0].reminders[0].rule.id,
        before.reminders[0].id
    );
    let update = tokio::time::timeout(std::time::Duration::from_secs(3), changes.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ScheduleCatalogUpdate::Page { index: 0, sessions } = update else {
        panic!("expected new task page")
    };
    assert_eq!(sessions[0].reminders, catalog.sessions[0].reminders);
    assert!(matches!(
        changes.next().await.unwrap().unwrap(),
        ScheduleCatalogUpdate::Complete { pages: 1 }
    ));
    assert!(!catalog.sessions[0].archived);
    assert!(!catalog.sessions[0].exhausted);
    assert!(catalog.next.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    if !matches!(target, RestartTarget::Active) {
        set_archived(&running, true).await;
        let update = tokio::time::timeout(std::time::Duration::from_secs(3), changes.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(update, ScheduleCatalogUpdate::Page { index: 0, sessions }
            if sessions.len() == 1 && sessions[0].archived && sessions[0].reminders[0].rule.id == before.reminders[0].id)
        );
    }
    if matches!(target, RestartTarget::PendingRestart) {
        disable_navigation(&fixture.profile);
        assert!(matches!(
            running.reload().await.unwrap(),
            rsi_host::ReloadOutcome::RestartRequired(_)
        ));
        assert!(
            running
                .session_service()
                .unwrap()
                .scheduled_tasks(None)
                .await
                .unwrap()
                .sessions[0]
                .archived
        );
        clock.advance(5000);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), delivered.notified())
                .await
                .is_err()
        );
    }
    drop(changes);
    assert!(running.shutdown().await.is_clean());
    if matches!(target, RestartTarget::NoArchiveRegistry) {
        disable_navigation(&fixture.profile);
    }
    if !matches!(target, RestartTarget::PendingRestart) {
        clock.advance(5000);
    }
    let restarted = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    if matches!(target, RestartTarget::Archived) {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), delivered.notified())
                .await
                .is_err(),
            "archived Session received its due model request"
        );
        set_archived(&restarted, false).await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), delivered.notified())
        .await
        .expect("enabled reminder must reach the provider before a user attaches");
    let restored = restarted
        .session_service()
        .unwrap()
        .scheduled_tasks(None)
        .await
        .unwrap();
    assert!(!restored.sessions[0].archived);
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("schedule-restart").unwrap())
        .await
        .unwrap();
    let after = settled(&handle, 1).await;
    assert_eq!(after.reminders[0].id, before.reminders[0].id);
    assert_eq!(after.allocated_rounds, 1);
    assert!(after.reminders[0].consumed);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    drop(handle);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_coalesces_only_enabled_recurrences_at_the_original_anchor() {
    let calls = Arc::new(AtomicUsize::new(0));
    let recurring = Arc::new(Mutex::new(None::<String>));
    let app = Router::new().route("/v1/chat/completions", post({
        let calls = calls.clone();
        let recurring = recurring.clone();
        move || {
            let idx = calls.fetch_add(1, Ordering::SeqCst);
            let selected = recurring.lock().unwrap().clone();
            async move {
                match idx {
                    0 => response(Some(("repeat", "schedule_create", serde_json::json!({
                        "prompt":"Check daily report", "rule":{"kind":"every","interval_ms":60000}
                    })))),
                    2 => response(Some(("single", "schedule_create", serde_json::json!({
                        "prompt":"Leave this reminder disabled", "rule":{"kind":"after","delay_ms":1000}
                    })))),
                    4 => response(Some(("select", "schedule_resume", serde_json::json!({"ids":[selected.unwrap()]})))),
                    _ => response(None),
                }
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
    let handle = create(&running, &fixture, "schedule-recurrence").await;
    run_message_to_terminal(&handle, "create-recurring").await;
    let original = state(&handle).await.reminders.remove(0);
    *recurring.lock().unwrap() = Some(original.id.to_string());
    run_message_to_terminal(&handle, "create-one-shot").await;
    run_message_to_terminal(&handle, "enable-only-recurring").await;
    assert_eq!(
        state(&handle)
            .await
            .reminders
            .iter()
            .filter(|r| r.active)
            .count(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    clock.advance(5 * 60_000 + 37);
    let restarted = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&SessionId::new("schedule-recurrence").unwrap())
        .await
        .unwrap();
    let final_state = settled(&handle, 1).await;
    assert_eq!(final_state.allocated_rounds, 1);
    let receipt = final_state.reservation.unwrap();
    assert_eq!(receipt.reminder_id, original.id);
    assert_eq!(receipt.occurrence_ms, original.anchor_ms + 4 * 60_000);
    let repeat = final_state
        .reminders
        .iter()
        .find(|r| r.id == original.id)
        .unwrap();
    assert_eq!(repeat.due_ms, receipt.occurrence_ms + 60_000);
    let disabled = final_state
        .reminders
        .iter()
        .find(|r| r.id != original.id)
        .unwrap();
    assert!(!disabled.active && !disabled.consumed);
    assert_eq!(calls.load(Ordering::SeqCst), 7);
    clock.advance(60_000);
    let next = settled(&handle, 2).await;
    let second = next.reservation.unwrap();
    assert_eq!(second.reminder_id, original.id);
    assert_eq!(second.occurrence_ms, repeat.due_ms);
    assert_ne!(second.input.message_id, receipt.input.message_id);
    assert_eq!(calls.load(Ordering::SeqCst), 8);
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
    fn delivery_holds(&self) -> tokio::sync::watch::Receiver<rsi_agent_schedule::DeliveryHolds> {
        self.service.delivery_holds()
    }
    async fn check_operator_run(
        &self,
        session: &SessionId,
        task: &rsi_agent_session_protocol::DomainRequestId,
        previous: Option<&rsi_agent_session_protocol::MessageId>,
    ) -> Result<(), String> {
        self.service
            .check_operator_run(session, task, previous)
            .await
    }
    fn hold_delivery(
        &self,
        session: &SessionId,
        task: &rsi_agent_session_protocol::DomainRequestId,
    ) {
        self.service.hold_delivery(session, task);
    }
    async fn refresh_pending(&self, session: &SessionId) {
        self.service.refresh_pending(session).await
    }

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
        use rsi_agent_store_protocol::SessionStoreContract;
        use rsi_agent_turn_protocol::{
            SessionContinuationsContract, SessionProjectionsContract, TurnExecutionContract,
            TurnServiceContract,
        };
        assert!(desired.is_null());
        Ok(
            rsi_meta::PreparedActivation::new(rsi_meta::ConfigValue::Null)
                .requiring_local::<TurnServiceContract>()
                .requiring_local::<TurnExecutionContract>()
                .requiring_local::<SessionStoreContract>()
                .requiring_local::<SessionContinuationsContract>()
                .requiring_local::<SessionProjectionsContract>(),
        )
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        use rsi_agent_store_protocol::SessionStoreContract;
        use rsi_agent_turn_protocol::{
            SessionContinuationsContract, SessionProjectionsContract, TurnExecutionContract,
            TurnServiceContract,
        };
        let service = rsi_schedule::ScheduleService::new(
            plan.context().clone(),
            plan.local::<TurnServiceContract>()?,
            plan.local::<TurnExecutionContract>()?,
            plan.local::<SessionStoreContract>()?,
            plan.local::<SessionContinuationsContract>()?,
            plan.local::<SessionProjectionsContract>()?,
            self.clock.clone(),
        );
        let cleanup = service.clone();
        let supply = plan
            .context()
            .provide_local::<rsi_agent_schedule::ScheduleControllerContract>(Arc::new(
                GatedController {
                    service: Arc::new(service.clone()),
                    gate: self.gate.clone(),
                },
            ))?;
        plan.defer(
            "stop fixture Schedule",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    cleanup.stop().await
                })
            }),
        )?;
        service.start(None);
        Ok(())
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
async fn operator_delete_stops_future_runs_and_preserves_a_claimed_sibling() {
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().route("/v1/chat/completions", post({
        let (calls, entered, release) = (calls.clone(), entered.clone(), release.clone());
        move |axum::Json(body): axum::Json<serde_json::Value>| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            let (entered, release) = (entered.clone(), release.clone());
            async move {
                match index {
                    0 => response(Some(("first","schedule_create",serde_json::json!({"prompt":"Skip this one","rule":{"kind":"after","delay_ms":1}})))),
                    2 => response(Some(("second","schedule_create",serde_json::json!({"prompt":"Keep this delivery","rule":{"kind":"every","interval_ms":60000}})))),
                    4 => {
                        let messages = body["messages"].to_string();
                        assert!(messages.contains("[SCHEDULE CHANGE]"));
                        assert!(messages.contains("change: deleted") && messages.contains("Skip this one"));
                        entered.notify_one();
                        release.notified().await;
                        response(None)
                    }
                    5 => {
                        let messages = body["messages"].to_string();
                        assert!(messages.matches("[SCHEDULE CHANGE]").count() >= 2);
                        response(None)
                    }
                    _ => response(None),
                }
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let running = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let session_id = SessionId::new("schedule-delete").unwrap();
    let handle = create(&running, &fixture, session_id.as_str()).await;
    run_message_to_terminal(&handle, "create-first").await;
    run_message_to_terminal(&handle, "create-recurring").await;
    let service = running.session_service().unwrap();
    let reminders = state(&handle).await.reminders;
    let task = |prompt| rsi_session_protocol::ScheduledTaskId {
        session_id: session_id.clone(),
        id: reminders
            .iter()
            .find(|row| row.prompt == prompt)
            .unwrap()
            .id
            .clone(),
    };
    assert!(
        service
            .delete_scheduled_task(task("Skip this one"))
            .await
            .unwrap()
    );
    assert!(
        !service
            .delete_scheduled_task(task("Skip this one"))
            .await
            .unwrap()
    );
    clock.advance(60_000);
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let accepted = state(&handle).await.reservation.unwrap();
    assert_eq!(accepted.prompt, "Keep this delivery");
    assert!(
        service
            .delete_scheduled_task(task("Keep this delivery"))
            .await
            .unwrap()
    );
    let after = state(&handle).await;
    assert!(after.reminders.is_empty());
    assert_eq!(after.reservation.unwrap(), accepted);
    release.notify_one();
    assert_eq!(
        settled(&handle, 1).await.reservation.unwrap().input,
        accepted.input
    );
    let history = handle.history_before(None, 512).await.unwrap();
    assert_eq!(
        history
            .facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::MessageTurnAccepted { .. }))
            .count(),
        3
    );
    assert!(history.facts.iter().all(|fact| !matches!(fact.body(),
        SessionFactBody::TurnTerminal {outcome, ..} if !matches!(outcome,rsi_agent_session_protocol::TurnOutcome::Completed))));
    drop((handle, service));
    assert!(running.shutdown().await.is_clean());
    clock.advance(60_000);
    let restarted = RunningRsi::boot(composition_with_clock(&fixture, clock), &fixture.profile)
        .await
        .unwrap();
    let handle = restarted
        .session_service()
        .unwrap()
        .attach(&session_id)
        .await
        .unwrap();
    let after = state(&handle).await;
    assert!(after.reminders.is_empty());
    assert_eq!(after.allocated_rounds, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    drop(handle);
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_cold_reminder_or_one_with_a_full_inbox_does_not_enqueue_context() {
    use rsi_agent_session_protocol::{MAXIMUM_PENDING_AGENT_MESSAGES, MessageDelivery};
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            match index {
                0 | 2 => response(Some((if index == 0 { "first" } else { "second" }, "schedule_create", serde_json::json!({
                    "prompt":format!("Later {index}"),"rule":{"kind":"after","delay_ms":60000}
                })))),
                _ => response(None),
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let clock = ManualClock::new();
    select_clock_profile(&fixture);
    let run = RunningRsi::boot(
        composition_with_clock(&fixture, clock.clone()),
        &fixture.profile,
    )
    .await
    .unwrap();
    let session_id = SessionId::new("cold-delete").unwrap();
    let handle = create(&run, &fixture, session_id.as_str()).await;
    run_message_to_terminal(&handle, "create-first-reminder").await;
    run_message_to_terminal(&handle, "create-second-reminder").await;
    let reminders = state(&handle).await.reminders;
    assert_eq!(reminders.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    drop(handle);
    assert!(run.shutdown().await.is_clean());

    let restarted = RunningRsi::boot(composition_with_clock(&fixture, clock), &fixture.profile)
        .await
        .unwrap();
    let service = restarted.session_service().unwrap();
    let task = |index: usize| rsi_session_protocol::ScheduledTaskId {
        session_id: session_id.clone(),
        id: reminders[index].id.clone(),
    };
    assert!(service.delete_scheduled_task(task(0)).await.unwrap());
    let handle = service.attach(&session_id).await.unwrap();
    assert!(handle.inspect().await.unwrap().pending.is_empty());
    for index in 0..MAXIMUM_PENDING_AGENT_MESSAGES {
        handle
            .submit(SubmitInput {
                origin: rsi_session_protocol::SubmissionOrigin::User,
                attribution: None,
                message_id: MessageId::new(format!("held-{index}")).unwrap(),
                delivery: MessageDelivery::NextStep,
                content: vec![SessionInput::Text {
                    text: "Retain this context".into(),
                }]
                .try_into()
                .unwrap(),
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .unwrap();
    }
    assert!(service.delete_scheduled_task(task(1)).await.unwrap());
    assert!(state(&handle).await.reminders.is_empty());
    assert_eq!(
        handle.inspect().await.unwrap().pending.len(),
        MAXIMUM_PENDING_AGENT_MESSAGES
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    drop((handle, service));
    assert!(restarted.shutdown().await.is_clean());
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
            arguments: rsi_agent_session_protocol::CommandArguments::new("".into()).unwrap(),
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
