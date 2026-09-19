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

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "opt-in actual next_frame global-section measurement; no timing pass threshold"]
async fn measure_actual_frames_with_small_and_large_global_details() {
    global_frame_cases(&[32, 64 * 1024], 10, true).await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn measured_frames_reconstruct_changed_blocks_and_details() {
    global_frame_cases(&[32, 64 * 1024], 2, false).await;
}

#[cfg(feature = "test-support")]
async fn global_frame_cases(detail_sizes: &[usize], samples: u64, report: bool) {
    for &bytes in detail_sizes {
        let (runtime, backend, app) = sources::fixture().await;
        let initial = frame(&app, None);
        let generation = initial["view"]["surfaces"]["main"]["generation"].clone();
        backend.facts.lock().unwrap().push(
            SessionFact::new(
                9,
                1,
                SessionFactBody::TurnAccepted {
                    reasoning_effort: None,
                    turn_id: TurnId::new("turn").unwrap(),
                    text: "x".repeat(bytes),
                    model: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        app.command(&json!({"action":"inspect_source","pane":"main","generation":generation,"source":{"seq":"9","field":{"kind":"turn_input"}}}).to_string()).await.unwrap();
        app.command(&json!({"action":"history","pane":"main","generation":generation}).to_string())
            .await
            .unwrap();
        let mut snapshot = frame(&app, None);
        for scenario in ["unchanged", "single_block", "single_detail"] {
            for sample in 0..samples {
                if scenario == "single_block" {
                    backend.facts.lock().unwrap().push(
                        SessionFact::new(
                            10 + sample,
                            1,
                            SessionFactBody::ModelEvent {
                                purpose: ModelEventPurpose::Conversation,
                                turn_id: TurnId::new("turn").unwrap(),
                                effect_id: EffectId::new("stream").unwrap(),
                                event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                                    index: 0,
                                    delta: rsi_ai_protocol::ContentDelta::Text(" changed".into()),
                                },
                            },
                        )
                        .unwrap(),
                    );
                    for action in ["live", "history"] {
                        app.command(
                            &json!({"action":action,"pane":"main","generation":generation})
                                .to_string(),
                        )
                        .await
                        .unwrap();
                    }
                }
                if scenario == "single_detail" {
                    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
                    app.command(&json!({"action":"inspect_source","pane":"main","generation":generation,"source":{"seq":"9","field":{"kind":"turn_input"}}}).to_string()).await.unwrap();
                }
                let base = snapshot["frame_id"].as_str().unwrap();
                let (encoded, allocations) =
                    frame_allocations::measure(|| app.next_frame(Some(base)).unwrap());
                let measurement = rsi_gui::test_support::take_frame_measurement();
                let patch: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
                assert_eq!(patch["kind"], "patch");
                if scenario == "single_block" {
                    assert_eq!(patch["surfaces"].as_array().unwrap().len(), 1);
                    reconstruct_changed_block(
                        &mut snapshot["view"]["surfaces"]["main"],
                        &patch["surfaces"][0],
                    );
                } else {
                    assert_eq!(patch["surfaces"], json!([]));
                }
                if scenario == "unchanged" {
                    assert_eq!(patch["sections"], json!({}));
                }
                for (key, value) in patch["sections"].as_object().unwrap() {
                    snapshot["view"][key] = value.clone();
                }
                snapshot["frame_id"] = patch["frame_id"].clone();
                let output_bytes = encoded.as_bytes().len();
                drop(encoded);
                assert_eq!(snapshot["view"], sources::view(&app));
                assert_eq!(measurement.section_materializations, 1);
                if report {
                    eprintln!(
                        "global_frame {}",
                        json!({"detail_bytes":bytes,"scenario":scenario,"sample":sample,"allocation_calls":allocations.calls,"allocation_requested_bytes":allocations.requested,"output_bytes":output_bytes,"measurement":measurement})
                    );
                }
            }
        }
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[cfg(feature = "test-support")]
fn reconstruct_changed_block(pane: &mut Value, change: &Value) {
    for (key, value) in change["fields"].as_object().unwrap() {
        pane[key] = value.clone();
    }
    let transcript = &change["transcript"];
    assert_eq!(transcript["upsert"].as_array().unwrap().len(), 1);
    assert_eq!(transcript["remove"], json!([]));
    for (key, value) in transcript["fields"].as_object().unwrap() {
        pane["transcript"][key] = value.clone();
    }
    let blocks = pane["transcript"]["blocks"].as_array_mut().unwrap();
    for block in transcript["upsert"].as_array().unwrap() {
        if let Some(old) = blocks.iter_mut().find(|old| old["key"] == block["key"]) {
            *old = block.clone();
        } else {
            blocks.push(block.clone());
        }
    }
    if let Some(order) = transcript.get("order") {
        let sorted = order
            .as_array()
            .unwrap()
            .iter()
            .map(|key| {
                blocks
                    .iter()
                    .find(|block| block["key"] == *key)
                    .unwrap()
                    .clone()
            })
            .collect();
        *blocks = sorted;
    }
}
