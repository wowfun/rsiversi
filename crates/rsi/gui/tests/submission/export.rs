use super::*;
use serde_json::{Value, json};

#[tokio::test]
async fn export_cancel_interrupts_pending_read_releases_capacity_and_never_submits() {
    let (runtime, backend, app, generation) = admission::fixture().await;
    let gate = Arc::new(admission::HeaderGate::default());
    *backend.export_gate.lock().unwrap() = Some(gate.clone());
    let input = |operation: Value| {
        json!({"pane":"main","generation":generation,"operation":operation}).to_string()
    };
    let open = input(json!({"kind":"open","arguments":"'../../suggested.json' -f json -i h"}));
    let opened: Value = serde_json::from_str(&app.export_input(&open).await.unwrap()).unwrap();
    assert_eq!(opened["filename"], "suggested.json");
    assert_eq!(opened["event"]["options"]["include"], json!(["header"]));
    assert!(
        app.export_input(&open).await.is_err(),
        "one export per pane"
    );
    let pending = app.export_input(&input(json!({"kind":"next","token":opened["token"]})));
    let task = tokio::spawn(pending);
    gate.entered.cancelled().await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.export_input(&input(json!({"kind":"cancel","token":opened["token"]}))),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(task.await.unwrap().is_err());
    *backend.export_gate.lock().unwrap() = None;
    let next: Value = serde_json::from_str(&app.export_input(&open).await.unwrap()).unwrap();
    let pull = input(json!({"kind":"next","token":next["token"]}));
    let completed: Value = serde_json::from_str(&app.export_input(&pull).await.unwrap()).unwrap();
    assert_eq!(completed["type"], "complete");
    assert_eq!(app.export_input(&pull).await.unwrap(), "null");
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(backend.commands.lock().unwrap().is_empty());
    assert!(
        app.export_input(
            &json!({"pane":"main","generation":"stale","operation":{"kind":"open","arguments":""}})
                .to_string()
        )
        .await
        .is_err()
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn application_shutdown_interrupts_native_export_stream() {
    use futures_util::StreamExt;
    let (runtime, backend, app, generation) = admission::fixture().await;
    let gate = Arc::new(admission::HeaderGate::default());
    *backend.export_gate.lock().unwrap() = Some(gate.clone());
    let (mut source, _) = app
        .open_export(&json!({"pane":"main","generation":generation,"arguments":""}).to_string())
        .await
        .unwrap();
    source.next().await.unwrap().unwrap();
    let task = tokio::spawn(async move { source.next().await });
    gate.entered.cancelled().await;
    assert!(runtime.shutdown().await.is_clean());
    assert!(task.await.unwrap().unwrap().is_err());
}
