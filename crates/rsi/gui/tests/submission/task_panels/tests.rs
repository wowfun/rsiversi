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
    app.command(&crate::ui::button(&detail(&app), Some("Check control result")).to_string())
        .await
        .unwrap();
    until(|| !detail(&app).to_string().contains("Check control result")).await;
    assert_eq!(
        backend.task_panels.controls.lock().unwrap().len(),
        1,
        "receipt reads never rearm or repeat creation"
    );
    let model = detail(&app).to_string();
    assert!(model.contains("Current driving") && model.contains("Armed"));
    assert!(model.contains("Durable phase") && model.contains("Active"));
    assert!(model.contains("Allocated rounds") && model.contains("1 / 3"));
    backend.task_panels.unknown.store(false, Ordering::SeqCst);
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
