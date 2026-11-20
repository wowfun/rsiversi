use super::*;
use serde_json::json;
use sources::view;

#[tokio::test]
async fn attached_workspace_truth_and_bounded_model_paging_reach_real_frames() {
    let (runtime, backend, app) = sources::fixture().await;
    let header = backend.header().await.unwrap();
    let coordinates = serde_json::to_value(header.coordinates()).unwrap();
    let key = rsi_workspace_protocol::WorkspaceKey::from_coordinates(header.coordinates());
    let initial = view(&app);
    assert_eq!(
        initial["surfaces"]["main"]["workspace"],
        json!({"key":key,"coordinates":coordinates,"registration":{"status":"unregistered"}})
    );
    let id = rsi_workspace_protocol::WorkspaceId::from_bytes([0xbb; 32]);
    *backend.current_registration.lock().unwrap() = Some(id.clone());
    *backend.registration_name.lock().unwrap() =
        Some(rsi_workspace_protocol::WorkspaceName::parse("Beta renamed").unwrap());
    *backend.model_catalog.lock().unwrap() = (0..65)
        .map(|i| rsi_ai_protocol::ModelRef::new("test", format!("model-{i:02}")).unwrap())
        .collect();
    app.command(r#"{"action":"refresh"}"#).await.unwrap();
    let registered = view(&app);
    assert!(
        registered["catalog"]["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        registered["surfaces"]["main"]["workspace"]["registration"],
        json!({"status":"registered","id":id,"name":"Beta renamed"})
    );
    assert_eq!(
        registered["catalog"]["models"].as_array().unwrap().len(),
        64
    );
    assert_eq!(registered["catalog"]["models_more"], true);
    assert_eq!(registered["catalog"]["models_at_start"], true);
    app.command(r#"{"action":"models_next"}"#).await.unwrap();
    let next = view(&app);
    assert_eq!(next["catalog"]["models"].as_array().unwrap().len(), 1);
    assert_eq!(next["catalog"]["models"][0]["model"], "model-64");
    assert_eq!(next["catalog"]["models_at_start"], false);
    assert_eq!(
        next["surfaces"]["main"]["model"],
        registered["surfaces"]["main"]["model"]
    );
    app.command(r#"{"action":"models_first"}"#).await.unwrap();
    assert_eq!(view(&app)["catalog"], registered["catalog"]);
    *backend.current_registration.lock().unwrap() = None;
    app.command(r#"{"action":"refresh"}"#).await.unwrap();
    let deleted = view(&app);
    assert_eq!(
        deleted["surfaces"]["main"]["workspace"]["registration"]["status"],
        "unregistered"
    );
    assert_eq!(
        deleted["surfaces"]["main"]["workspace"]["coordinates"],
        coordinates
    );
    backend
        .registration_unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(app.command(r#"{"action":"refresh"}"#).await.is_err());
    let unavailable = view(&app);
    assert_eq!(
        unavailable["surfaces"]["main"]["workspace"]["registration"]["status"],
        "unavailable"
    );
    // Optional evidence export belongs to this test caller, never to production.
    if let Some(path) = std::env::var_os("RSI_CATALOG_FRAME_EVIDENCE") {
        std::fs::write(path,serde_json::to_vec(&json!({"registered":registered,"next":next,"deleted":deleted,"unavailable":unavailable})).unwrap()).unwrap();
    }
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn directory_reply_does_not_publish_a_workbench_frame() {
    let (runtime, _backend, app) = super::sources::fixture().await;
    let mut changes = app.changes();
    changes.borrow_and_update();
    // This composition omits the picker. Its error is delivered only to the caller.
    assert!(
        app.directory_input(r#"{"action":"status","location":{"kind":"local"}}"#)
            .await
            .is_err()
    );
    assert!(!changes.has_changed().unwrap());
    assert!(runtime.shutdown().await.is_clean());
}
