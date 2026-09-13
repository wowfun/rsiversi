use super::*;
use crate::sources::{fixture, view};

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn prepared() -> (Runtime, Arc<Backend>, Arc<rsi_gui::GuiApplication>) {
    let (runtime, backend, app) = fixture().await;
    backend.task_panels.enabled.store(true, Ordering::SeqCst);
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    (runtime, backend, app)
}
fn open(app: &rsi_gui::GuiApplication, name: &str) -> String {
    let pane = view(app)["surfaces"]["main"].clone();
    let reference = pane["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|surface| surface["reference"]["name"] == name)
        .unwrap()["reference"]
        .clone();
    json!({"action":"ui_surface","pane":"main","generation":pane["generation"],"reference":reference}).to_string()
}
fn detail(app: &rsi_gui::GuiApplication) -> Value {
    view(app)["ui_detail"].clone()
}

#[tokio::test]
async fn late_goal_control_and_reconciliation_replies_preserve_observed_driver_state() {
    for reconcile in [false, true] {
        let (runtime, backend, app) = prepared().await;
        app.command(&open(&app, "goal")).await.unwrap();
        until(|| detail(&app).to_string().contains("Create and start Goal")).await;
        let mut action = crate::ui::button(&detail(&app), Some("Create and start Goal"));
        action["input"]["fields"] =
            json!({"objective":"Delayed control reply", "constraints":"", "rounds":"3"});
        let scenario = &backend.task_panels;
        if reconcile {
            scenario.unknown.store(true, Ordering::SeqCst);
            app.command(&action.to_string()).await.unwrap();
            until(|| detail(&app).to_string().contains("Check control result")).await;
            action = crate::ui::button(&detail(&app), Some("Check control result"));
        }
        scenario.block_reply.store(true, Ordering::SeqCst);
        let pending = app.command(&action.to_string());
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            scenario.reply_entered.notified(),
        )
        .await
        .unwrap();
        assert_eq!(
            scenario.live.lock().unwrap().stage,
            GoalDriverStage::Reserving
        );
        scenario.live.lock().unwrap().stage = GoalDriverStage::Waiting;
        scenario.live_changed.send_replace(());
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            scenario.waiting_observed.notified(),
        )
        .await
        .unwrap();
        scenario.reply_release.notify_one();
        pending.await.unwrap();
        let model = detail(&app).to_string();
        assert!(
            model.contains("Waiting") && !model.contains("Reserving"),
            "late receipt regressed live state (reconcile={reconcile}): {model}"
        );
        assert!(!model.contains("outcome is unresolved"));
        assert_eq!(scenario.controls.lock().unwrap().len(), 1);
        assert_eq!(
            scenario.receipt_queries.lock().unwrap().len(),
            usize::from(reconcile)
        );
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn goal_form_requires_a_cap_and_unknown_control_queries_without_replaying() {
    let (runtime, backend, app) = prepared().await;
    app.command(&open(&app, "goal")).await.unwrap();
    until(|| detail(&app).to_string().contains("Create and start Goal")).await;
    let mut create = crate::ui::button(&detail(&app), Some("Create and start Goal"));
    create["input"]["fields"] = json!({"objective":"Implement the fixed task", "constraints":"Preserve the oracle", "rounds":"0"});
    app.command(&create.to_string()).await.unwrap();
    assert!(backend.task_panels.controls.lock().unwrap().is_empty());
    until(|| {
        detail(&app)["model"]
            .to_string()
            .contains("positive integer")
    })
    .await;
    create = crate::ui::button(&detail(&app), Some("Create and start Goal"));
    create["input"]["fields"] = json!({"objective":"Implement the fixed task", "constraints":"Preserve the oracle", "rounds":"3"});
    backend.task_panels.unknown.store(true, Ordering::SeqCst);
    app.command(&create.to_string()).await.unwrap();
    until(|| detail(&app).to_string().contains("Check control result")).await;
    assert_eq!(backend.task_panels.controls.lock().unwrap().len(), 1);
    assert!(detail(&app).to_string().contains("outcome is unresolved"));
    backend
        .task_panels
        .receipt_unavailable
        .store(true, Ordering::SeqCst);
    app.command(&open(&app, "goal")).await.unwrap();
    app.command(&crate::ui::button(&detail(&app), Some("Check control result")).to_string())
        .await
        .unwrap();
    assert!(detail(&app).to_string().contains("outcome is unresolved"));
    assert_eq!(backend.task_panels.controls.lock().unwrap().len(), 1);
    backend
        .task_panels
        .receipt_unavailable
        .store(false, Ordering::SeqCst);
    app.command(&open(&app, "goal")).await.unwrap();
    app.command(&crate::ui::button(&detail(&app), Some("Check control result")).to_string())
        .await
        .unwrap();
    until(|| !detail(&app).to_string().contains("Check control result")).await;
    assert_eq!(
        backend.task_panels.controls.lock().unwrap().len(),
        1,
        "receipt reads never rearm or repeat creation"
    );
    let original = backend.task_panels.controls.lock().unwrap()[0]
        .request_id
        .clone();
    assert_eq!(
        *backend.task_panels.receipt_queries.lock().unwrap(),
        vec![original.clone(), original]
    );
    let model = detail(&app).to_string();
    assert!(model.contains("Current driving") && model.contains("Armed"));
    assert!(model.contains("Durable phase") && model.contains("Active"));
    assert!(model.contains("Allocated rounds") && model.contains("1 / 3"));
    backend.task_panels.unknown.store(false, Ordering::SeqCst);
    app.command(&open(&app, "goal")).await.unwrap();
    app.command(&crate::ui::button(&detail(&app), Some("Pause after current round")).to_string())
        .await
        .unwrap();
    until(|| detail(&app).to_string().contains("Paused")).await;
    assert!(detail(&app).to_string().contains("Disarmed"));
    assert!(detail(&app).to_string().contains("1 / 3"));
    assert!(backend.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(
        backend.task_panels.controls.lock().unwrap().len(),
        2,
        "presentation retirement is not a Goal control"
    );
    assert_eq!(backend.task_panels.projections.retained_bytes(), 0);
}

#[tokio::test]
async fn jobs_panel_pages_the_same_current_scope_and_cancels_closed_reads() {
    let (runtime, backend, app) = prepared().await;
    backend
        .task_panels
        .jobs_active
        .store(true, Ordering::SeqCst);
    app.command(&open(&app, "jobs")).await.unwrap();
    assert!(detail(&app).to_string().contains("job-a"));
    assert!(detail(&app).to_string().contains("Reported: false"));
    app.command(&crate::ui::button(&detail(&app), Some("Next Jobs page")).to_string())
        .await
        .unwrap();
    assert!(detail(&app).to_string().contains("job-b"));
    let reads = backend.task_panels.jobs_reads.lock().unwrap().clone();
    assert_eq!(reads.last().unwrap().after.as_deref(), Some("job-a"));
    assert_eq!(reads.last().unwrap().generation, Some(7));
    assert!(
        reads
            .iter()
            .all(|read| read.turn_id.as_str() == "jobs-turn")
    );
    backend
        .task_panels
        .jobs_active
        .store(false, Ordering::SeqCst);
    app.command(&crate::ui::button(&detail(&app), Some("Refresh current Turn")).to_string())
        .await
        .unwrap();
    assert!(detail(&app).to_string().contains("No active Turn"));
    assert_eq!(backend.task_panels.jobs.retained_bytes(), 0);
    backend
        .task_panels
        .jobs_active
        .store(true, Ordering::SeqCst);
    backend.task_panels.block_jobs.store(true, Ordering::SeqCst);
    let pending = app.command(&open(&app, "jobs"));
    until(|| backend.task_panels.reading_jobs.load(Ordering::SeqCst) == 1).await;
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    pending.await.unwrap();
    until(|| backend.task_panels.reading_jobs.load(Ordering::SeqCst) == 0).await;
    assert!(backend.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the gated live/projection/action chronology visible together.
async fn goal_rejection_survives_disarmed_then_delayed_settlement_projection() {
    let (runtime, backend, app) = prepared().await;
    let goal_id = DomainRequestId::new("delayed-settlement-goal").unwrap();
    let header = backend.header().await.unwrap();
    let budget = header.settings().turn_budget();
    {
        let mut state = backend.task_panels.state.lock().unwrap();
        state
            .apply(
                GoalAction::Create {
                    id: goal_id.clone(),
                    objective: "Finish the fixed task".into(),
                    constraints: String::new(),
                    max_rounds: 3,
                },
                budget,
                true,
            )
            .unwrap();
        state
            .apply(GoalAction::Pause { id: goal_id }, budget, true)
            .unwrap();
    }
    backend.task_panels.changed.send_replace(2);
    app.command(&open(&app, "goal")).await.unwrap();
    until(|| detail(&app).to_string().contains("Paused")).await;
    assert!(!detail(&app).to_string().contains("Create and start Goal"));
    let scenario = &backend.task_panels;
    scenario.block_projections.store(true, Ordering::SeqCst);
    {
        let mut state = scenario.state.lock().unwrap();
        let goal = state.goal.as_mut().unwrap();
        let message = goal.reservation.as_ref().unwrap().message_id.clone();
        goal.settle(
            &message,
            rsi_agent_goal::RoundSettlement::Turn {
                turn_id: TurnId::new("first-round").unwrap(),
                outcome: rsi_agent_goal::RoundOutcome::Completed,
            },
        )
        .unwrap();
    }
    scenario.changed.send_modify(|revision| *revision += 1);
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        scenario.projection_entered.notified(),
    )
    .await
    .unwrap();
    scenario.live.lock().unwrap().detail = Some("Disarmed before settlement projection".into());
    scenario.live_changed.send_replace(());
    until(|| {
        detail(&app)
            .to_string()
            .contains("Disarmed before settlement projection")
    })
    .await;
    app.command(&open(&app, "goal")).await.unwrap();
    let old_resume = crate::ui::button(&detail(&app), Some("Resume Goal"));
    app.command(&old_resume.to_string()).await.unwrap();
    until(|| {
        detail(&app)
            .to_string()
            .contains("command revision conflict")
    })
    .await;
    let rejected = scenario.controls.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        rejected.expected_revision,
        CommandRevision::Draft { revision: 2 }
    );
    scenario.projection_release.notify_one();
    until(|| detail(&app).to_string().contains("Create and start Goal")).await;
    assert!(
        detail(&app)
            .to_string()
            .contains("command revision conflict"),
        "projection refresh erased explicit rejection: {}",
        detail(&app)
    );
    assert!(
        detail(&app)
            .to_string()
            .contains(rejected.request_id.as_str())
    );
    scenario.live.lock().unwrap().detail = Some("Another live refresh".into());
    scenario.live_changed.send_replace(());
    until(|| detail(&app).to_string().contains("Another live refresh")).await;
    assert!(
        detail(&app)
            .to_string()
            .contains("command revision conflict")
    );
    app.command(&open(&app, "goal")).await.unwrap();
    let new_resume = crate::ui::button(&detail(&app), Some("Resume Goal"));
    app.command(&new_resume.to_string()).await.unwrap();
    until(|| {
        detail(&app).to_string().contains("Active")
            && !detail(&app)
                .to_string()
                .contains("command revision conflict")
    })
    .await;
    let controls = scenario.controls.lock().unwrap().clone();
    assert_eq!(controls.len(), 2);
    assert_eq!(
        controls.last().unwrap().expected_revision,
        CommandRevision::Draft { revision: 3 }
    );
    assert_ne!(controls.last().unwrap().request_id, rejected.request_id);
    assert!(runtime.shutdown().await.is_clean());
}
