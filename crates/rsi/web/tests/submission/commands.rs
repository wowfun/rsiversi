use super::*;
use serde_json::json;

fn view(app: &rsi_web::WebApplication) -> serde_json::Value {
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
        ("web", Arc::new(rsi_web::WebApplicationFactory)),
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
        .lookup_local::<rsi_web::WebApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    let initial = view(&app);
    let generation = initial["panes"][0]["generation"].clone();
    app.command(&json!({"action":"commands","pane":0,"generation":generation}).to_string())
        .await
        .unwrap();
    assert_eq!(
        view(&app)["panes"][0]["commands"]["commands"][0]["name"],
        "plan"
    );
    assert!(app.command(&json!({"action":"submit","pane":0,"generation":generation,"text":"/plan on","steer":false}).to_string()).await.is_err());
    let invocation = backend.commands.lock().unwrap()[0].clone();
    assert_eq!(invocation.arguments.value(), "on");
    assert!(backend.requests.lock().unwrap().is_empty());
    app.command(
        &json!({"action":"open","pane":0,"session":initial["panes"][0]["session"]}).to_string(),
    )
    .await
    .unwrap();
    let next = view(&app);
    assert_eq!(
        next["panes"][0]["command_submission"]["pending"],
        json!(invocation)
    );
    assert!(
        app.command(
            &json!({"action":"refresh_command_result","pane":0,"generation":generation})
                .to_string()
        )
        .await
        .is_err(),
        "stale rendered action is fenced"
    );
    let generation = next["panes"][0]["generation"].clone();
    assert!(app.command(&json!({"action":"submit","pane":0,"generation":generation,"text":"/plan off","steer":false}).to_string()).await.is_err());
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
    *backend.command_receipt.lock().unwrap() =
        Some(SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap());
    app.command(
        &json!({"action":"refresh_command_result","pane":0,"generation":generation}).to_string(),
    )
    .await
    .unwrap();
    let next = view(&app);
    assert!(next["panes"][0]["command_submission"]["pending"].is_null());
    assert_eq!(
        next["panes"][0]["command_submission"]["receipt"]["request_id"],
        invocation.request_id.as_str()
    );
    assert_eq!(next["panes"][0]["draft"], "/plan off");
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
