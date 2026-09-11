use super::*;
use serde_json::json;

fn view(app: &rsi_gui::GuiApplication) -> serde_json::Value {
    serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap()
}
#[tokio::test]
async fn command_discovery_unknown_result_and_replaced_pane_preserve_original_invocation() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let backend = Arc::new(Backend::default());
    for (id, factory) in [
        (
            "providers",
            Arc::new(Providers(backend.clone())) as Arc<dyn PluginFactory>,
        ),
        ("ui", Arc::new(rsi_ui::UiFactory)),
        ("session-ui", Arc::new(rsi_session_ui::SessionUiFactory)),
        ("web", Arc::new(rsi_gui::GuiApplicationFactory)),
    ] {
        let fiber = root
            .apply(
                ResolvedFactory::linked(id, "fixture", UpdateMode::RestartRequired, factory),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
    }
    let app = root
        .lookup_local::<rsi_gui::GuiApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    let initial = view(&app);
    backend
        .unpublished
        .store(true, std::sync::atomic::Ordering::Release);
    let restored: serde_json::Value = serde_json::from_str(&app.restore_session(&json!({
        "action":"open", "pane":"main", "session":initial["surfaces"]["main"]["session"], "header":initial["surfaces"]["main"]["header"]
    }).to_string()).await.unwrap()).unwrap();
    assert_eq!(restored["status"], "opened");
    assert_eq!(restored["session"], initial["surfaces"]["main"]["session"]);
    let generation = view(&app)["surfaces"]["main"]["generation"].clone();
    backend
        .unpublished
        .store(false, std::sync::atomic::Ordering::Release);
    app.command(&json!({"action":"commands","pane":"main","generation":generation}).to_string())
        .await
        .unwrap();
    assert_eq!(
        view(&app)["surfaces"]["main"]["commands"]["commands"][0]["name"],
        "plan"
    );
    let generation = generation.as_str().unwrap();
    let prepared = prepare(&app, generation, "/plan on", vec![], false).await;
    assert_eq!(prepared["kind"], "command");
    assert!(backend.commands.lock().unwrap().is_empty());
    assert_eq!(
        dispatch(&app, generation, &prepared, "dispatch").await["status"],
        "unknown"
    );
    let invocation = backend.commands.lock().unwrap()[0].clone();
    assert_eq!(invocation.arguments.value(), "on");
    assert!(backend.requests.lock().unwrap().is_empty());
    app.command(
        &json!({"action":"open","pane":"main","session":initial["surfaces"]["main"]["session"]})
            .to_string(),
    )
    .await
    .unwrap();
    assert_eq!(
        dispatch(&app, generation, &prepared, "query").await["status"],
        "unknown",
        "stale generation is fenced"
    );
    let next = view(&app);
    let generation = next["surfaces"]["main"]["generation"].as_str().unwrap();
    assert_eq!(
        dispatch(&app, generation, &prepared, "query").await["status"],
        "unknown"
    );
    assert_eq!(
        dispatch(&app, generation, &prepared, "retry_message").await["status"],
        "unknown"
    );
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
    *backend.command_receipt.lock().unwrap() =
        Some(SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap());
    assert_eq!(
        dispatch(&app, generation, &prepared, "query").await["status"],
        "complete"
    );
    assert_eq!(
        view(&app)["surfaces"]["main"]["command_receipt"]["request_id"],
        invocation.request_id.as_str()
    );
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
