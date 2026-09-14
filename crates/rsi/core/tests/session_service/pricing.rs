use super::*;

async fn priced_chat() -> Response {
    Response::builder().status(200).header("content-type", "text/event-stream")
        .body(Body::from("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n")).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Old and new Sessions are compared across a real configuration update.
async fn configured_prices_freeze_at_creation_and_are_copied_into_actual_intents() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(priced_chat)),
        )
        .await
        .unwrap();
    });
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap()
        .id;
    let create = |id: &str| CreateSession {
        workspace_id: workspace.clone(),
        session_id: SessionId::new(id).unwrap(),
        agent_preset_id: None,
        workspace_trust: WorkspaceTrust::Trusted,
    };
    let service = running.session_service().unwrap();
    let original = service.create(create("unpriced")).await.unwrap();
    run_message_to_terminal(&original, "discover-route").await;
    let facts = original.history_before(None, 128).await.unwrap();
    let prepared = facts
        .facts
        .iter()
        .find_map(|fact| {
            if let SessionFactBody::ModelIntent {
                snapshot,
                price_quote,
                ..
            } = fact.body()
            {
                assert!(price_quote.is_none());
                Some(snapshot.clone())
            } else {
                None
            }
        })
        .unwrap();
    let settings = running.settings_access().unwrap();
    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["pricing"] = serde_json::json!([{"model":{"deployment":prepared.deployment_id,"model":prepared.model}, "endpoint_fingerprint":prepared.endpoint_fingerprint, "currency":"USD", "input_nanos":100, "output_nanos":200}]);
    settings
        .replace("rsi.agent", &current.version(), value)
        .await
        .unwrap();
    let priced = service.create(create("priced")).await.unwrap();
    run_message_to_terminal(&priced, "priced-turn").await;
    run_message_to_terminal(&original, "still-unpriced").await;
    let totals = priced.metrics().await.unwrap();
    assert!(totals.complete);
    assert!(totals.summary.configured_cost.is_complete());
    assert_eq!(totals.summary.configured_cost.totals[0].nanos, 900);
    for handle in [&original, &priced] {
        let expected = usize::from(Arc::ptr_eq(handle, &priced));
        let facts = handle.history_before(None, 128).await.unwrap();
        assert_eq!(
            facts
                .facts
                .iter()
                .filter(|fact| matches!(
                    fact.body(),
                    SessionFactBody::ModelIntent {
                        price_quote: Some(_),
                        ..
                    }
                ))
                .count(),
            expected
        );
    }
    let current = settings.read("rsi.agent").await.unwrap();
    let mut value = current.value.clone();
    value["pricing"][0]["input_nanos"] = serde_json::json!(1_000);
    settings
        .replace("rsi.agent", &current.version(), value)
        .await
        .unwrap();
    run_message_to_terminal(&priced, "frozen-again").await;
    assert_eq!(
        priced
            .metrics()
            .await
            .unwrap()
            .summary
            .configured_cost
            .totals[0]
            .nanos,
        1_800
    );
    let updated = service.create(create("new-prices")).await.unwrap();
    run_message_to_terminal(&updated, "new-price-turn").await;
    assert_eq!(
        updated
            .metrics()
            .await
            .unwrap()
            .summary
            .configured_cost
            .totals[0]
            .nanos,
        5_400
    );
    assert!(running.shutdown().await.is_clean());
    server.abort();
    let _ = server.await;
}
