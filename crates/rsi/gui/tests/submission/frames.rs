use super::*;
use serde_json::{Value, json};
fn frame(app: &rsi_gui::GuiApplication, base: Option<&str>) -> Value {
    serde_json::from_slice(app.next_frame(base).unwrap().as_bytes()).unwrap()
}
#[derive(Debug)]
struct GlobalCard;
impl rsi_ui::SurfaceRenderer for GlobalCard {
    fn render(&self, _: &Context) -> rsi_ui::Result<rsi_ui::UiView> {
        Ok(rsi_ui::UiView {
            title: "Application fixture".into(),
            elements: vec![rsi_ui::UiElement::Field {
                label: "Scope".into(),
                value: "Application".into(),
            }],
        })
    }
}
#[async_trait]
impl PluginFactory for GlobalCard {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()).requiring_local::<rsi_ui::UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<rsi_ui::UiContract>()?
            .register(
                &plan,
                rsi_ui::Contributions {
                    name: "global.fixture".into(),
                    surfaces: vec![rsi_ui::SurfaceContribution {
                        name: "status".into(),
                        title: "Application fixture".into(),
                        target: rsi_ui::TargetKind::Application,
                        renderer: Arc::new(GlobalCard),
                    }],
                    actions: vec![],
                    renderers: vec![],
                },
            )
            .unwrap();
        plan.defer(
            "close global fixture",
            Box::new(move || {
                Box::pin(async move {
                    assert!(lease.dispose().await.is_clean());
                    Ok(())
                })
            }),
        )
    }
}
#[tokio::test]
async fn application_contributions_have_no_session_binding_and_survive_session_surface_close() {
    let runtime = Runtime::default();
    let mut target = None;
    let backend = Arc::new(Backend::default());
    for (id, factory, config) in [
        (
            "providers",
            Arc::new(Providers(backend)) as Arc<dyn PluginFactory>,
            Value::Null,
        ),
        ("ui", Arc::new(rsi_ui::UiFactory), Value::Null),
        (
            "target",
            Arc::new(rsi_ui::UiTargetFactory),
            json!("application"),
        ),
        ("global", Arc::new(GlobalCard), Value::Null),
        ("gui", Arc::new(rsi_gui::GuiApplicationFactory), Value::Null),
    ] {
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(id, "fixture", UpdateMode::RestartRequired, factory),
                config,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
        if id == "target" {
            target = Some(fiber);
        }
    }
    let app = runtime
        .root()
        .lookup_local::<rsi_gui::GuiApplicationContract>()
        .unwrap();
    let view = sources::view(&app);
    let reference = view["application_surfaces"][0]["reference"].clone();
    app.command(&json!({"action":"application_ui_surface","reference":reference}).to_string())
        .await
        .unwrap();
    let detail = sources::view(&app)["ui_detail"].clone();
    assert!(detail["pane"].is_null() && detail["generation"].is_null());
    assert_eq!(
        detail["model"]["standard_view"]["title"],
        "Application fixture"
    );
    app.command(r#"{"action":"close_surface","pane":"main"}"#)
        .await
        .unwrap();
    assert_eq!(
        sources::view(&app)["ui_detail"]["binding"],
        detail["binding"]
    );
    assert!(target.unwrap().dispose().await.is_clean());
    assert!(
        app.command(&json!({"action":"application_ui_surface","reference":reference}).to_string())
            .await
            .is_err()
    );
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn surface_membership_is_bounded_and_reusing_a_key_cannot_reuse_attachment_authority() {
    let (runtime, _backend, app) = sources::fixture().await;
    let initial = frame(&app, None);
    let original = &initial["view"]["surfaces"]["main"];
    app.command(r#"{"action":"add_surface","pane":"compare"}"#)
        .await
        .unwrap();
    assert!(
        app.command(r#"{"action":"add_surface","pane":"third"}"#)
            .await
            .is_err()
    );
    assert!(
        app.command(r#"{"action":"add_surface","pane":"main"}"#)
            .await
            .is_err()
    );
    app.command(r#"{"action":"close_surface","pane":"main"}"#)
        .await
        .unwrap();
    let removed = frame(&app, Some("1"));
    assert_eq!(removed["kind"], "snapshot");
    assert!(removed["view"]["surfaces"].get("main").is_none());
    app.command(r#"{"action":"add_surface","pane":"main"}"#)
        .await
        .unwrap();
    app.command(&json!({"action":"open","pane":"main","session":original["session"]}).to_string())
        .await
        .unwrap();
    let current = frame(&app, Some("2"));
    assert_ne!(
        current["view"]["surfaces"]["main"]["generation"],
        original["generation"]
    );
    assert!(app.command(&json!({"action":"model","pane":"main","generation":original["generation"],"model":{"deployment":"stale","model":"stale"}}).to_string()).await.is_err());
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn application_frames_follow_models_details_and_generation_changes_without_editable_input() {
    let (runtime, _backend, app) = sources::fixture().await;
    let initial = frame(&app, None);
    assert_eq!(initial["kind"], "snapshot");
    let pane = &initial["view"]["surfaces"]["main"];
    assert!(pane.get("draft").is_none());
    assert!(pane.get("images").is_none());
    assert_eq!(pane["header"].as_str().unwrap().len(), 64);
    app.command(&json!({"action":"model","pane":"main","generation":pane["generation"],"model":{"deployment":"changed","model":"next"}}).to_string()).await.unwrap();
    let model = frame(&app, Some("1"));
    assert_eq!(
        model["surfaces"][0]["fields"]["model"],
        json!({"deployment":"changed","model":"next"})
    );
    assert!(model["surfaces"][0].get("transcript").is_none());
    let surface = &pane["ui_surfaces"][0]["reference"];
    app.command(&json!({"action":"ui_surface","pane":"main","generation":pane["generation"],"reference":surface}).to_string()).await.unwrap();
    let detail = frame(&app, Some("2"));
    assert!(detail["sections"]["ui_detail"].is_object());
    assert_eq!(detail["surfaces"], json!([]));
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    let closed = frame(&app, Some("3"));
    assert_eq!(closed["sections"]["ui_detail"], Value::Null);
    app.command(r#"{"action":"add_surface","pane":"compare"}"#)
        .await
        .unwrap();
    app.command(&json!({"action":"open","pane":"compare","session":pane["session"]}).to_string())
        .await
        .unwrap();
    let next = frame(&app, Some("4"));
    assert_eq!(next["kind"], "snapshot");
    assert!(next["view"]["surfaces"]["main"].get("draft").is_none());
    assert_eq!(
        next["view"]["surfaces"]["compare"]["session"],
        pane["session"]
    );
    assert!(runtime.shutdown().await.is_clean());
    assert!(matches!(
        app.next_frame(Some("5")),
        Err(rsi_api_protocol::ApiError::ShuttingDown)
    ));
}
