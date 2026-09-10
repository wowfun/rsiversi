use super::*;
use serde_json::{Value, json};
fn frame(app: &rsi_web::WebApplication, base: Option<&str>) -> Value {
    serde_json::from_slice(app.next_frame(base).unwrap().as_bytes()).unwrap()
}
#[tokio::test]
async fn application_frames_follow_drafts_models_details_and_generation_changes() {
    let (runtime, _backend, app) = sources::fixture().await;
    let initial = frame(&app, None);
    assert_eq!(initial["kind"], "snapshot");
    let pane = &initial["view"]["panes"][0];
    app.command(
        &json!({"action":"draft","pane":0,"generation":pane["generation"],"text":"edited draft"})
            .to_string(),
    )
    .await
    .unwrap();
    let draft = frame(&app, Some("1"));
    assert_eq!(draft["kind"], "patch");
    assert_eq!(
        draft["panes"],
        json!([{"index":0,"fields":{"draft":"edited draft"}}])
    );
    app.command(&json!({"action":"model","pane":0,"generation":pane["generation"],"model":{"deployment":"changed","model":"next"}}).to_string()).await.unwrap();
    let model = frame(&app, Some("2"));
    assert_eq!(
        model["panes"][0]["fields"]["model"],
        json!({"deployment":"changed","model":"next"})
    );
    assert!(model["panes"][0].get("transcript").is_none());
    let surface = &pane["ui_surfaces"][0]["reference"];
    app.command(&json!({"action":"ui_surface","pane":0,"generation":pane["generation"],"reference":surface}).to_string()).await.unwrap();
    let detail = frame(&app, Some("3"));
    assert!(detail["sections"]["ui_detail"].is_object());
    assert_eq!(detail["panes"], json!([]));
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    let closed = frame(&app, Some("4"));
    assert_eq!(closed["sections"]["ui_detail"], Value::Null);
    app.command(&json!({"action":"open","pane":1,"session":pane["session"]}).to_string())
        .await
        .unwrap();
    let next = frame(&app, Some("5"));
    assert_eq!(next["kind"], "snapshot");
    assert_eq!(next["view"]["panes"][0]["draft"], "edited draft");
    assert_eq!(next["view"]["panes"][1]["session"], pane["session"]);
    assert!(runtime.shutdown().await.is_clean());
    assert!(matches!(
        app.next_frame(Some("6")),
        Err(rsi_api_protocol::ApiError::ShuttingDown)
    ));
}
