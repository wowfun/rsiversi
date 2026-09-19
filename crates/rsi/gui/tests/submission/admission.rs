use super::*;
use serde_json::json;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
pub(super) struct HeaderGate {
    pub(super) entered: CancellationToken,
    pub(super) release: CancellationToken,
}

async fn fixture() -> (Runtime, Arc<Backend>, Arc<rsi_gui::GuiApplication>, String) {
    let runtime = Runtime::default();
    let root = runtime.root();
    let backend = Arc::new(Backend::default());
    backend.unpublished.store(true, Ordering::SeqCst);
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
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#).await.unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let generation = view["surfaces"]["main"]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    (runtime, backend, app, generation)
}

#[tokio::test]
async fn rejected_fresh_submissions_do_not_exhaust_owned_message_capacity() {
    let (runtime, backend, app, generation) = fixture().await;
    backend.reject_submissions.store(true, Ordering::SeqCst);
    for _ in 0..1024 {
        let prepared = prepare(&app, &generation, "not sent", vec![], false).await;
        assert_eq!(
            dispatch(&app, &generation, &prepared, "dispatch").await["status"],
            "not_admitted"
        );
    }
    backend.reject_submissions.store(false, Ordering::SeqCst);
    backend.resolution.store(2, Ordering::SeqCst);
    let prepared = prepare(&app, &generation, "first accepted request", vec![], false).await;
    let result = dispatch(&app, &generation, &prepared, "dispatch").await;
    assert_eq!(result["status"], "complete", "{result}");
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn passive_model_refresh_preserves_efforts_during_catalog_outages() {
    let (runtime, backend, app, generation) = fixture().await;
    let before: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    assert!(!before["surfaces"]["main"]["effort_profile"].is_null());
    let describes = backend.model_describes.load(Ordering::SeqCst);
    backend.model_describe_fails.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        app.command(
            &json!({"action":"model_refresh","pane":"main","generation":generation}).to_string(),
        )
        .await
        .unwrap();
    }
    let after: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    assert_eq!(
        after["surfaces"]["main"]["effort_profile"],
        before["surfaces"]["main"]["effort_profile"]
    );
    assert_eq!(
        backend.model_describes.load(Ordering::SeqCst),
        describes + 3
    );
    assert!(app.command(&json!({"action":"model","pane":"main","generation":generation,"model":{"deployment":"other","model":"reasoner"},"reasoning_effort":"max"}).to_string()).await.is_err());
    assert!(backend.commands.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn model_effort_change_retains_unknown_invocation_across_pane_replacement() {
    let (runtime, backend, app, generation) = fixture().await;
    let selection = |generation: &str, effort| json!({"action":"model","pane":"main","generation":generation,"model":{"deployment":"other","model":"reasoner"},"reasoning_effort":effort});
    assert!(
        app.command(&selection(&generation, Some("xhigh")).to_string())
            .await
            .is_err()
    );
    assert!(backend.commands.lock().unwrap().is_empty());
    backend.model_outcome_unknown.store(true, Ordering::SeqCst);
    assert!(
        app.command(&selection(&generation, Some("max")).to_string())
            .await
            .is_err()
    );
    let invocation = backend.commands.lock().unwrap()[0].clone();
    assert_eq!(invocation.arguments.value()["reasoning_effort"], "max");
    assert!(
        app.command(&selection(&generation, Some("low")).to_string())
            .await
            .is_err()
    );
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let session = view["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    let generation = view["surfaces"]["main"]["generation"].as_str().unwrap();
    assert_eq!(
        view["surfaces"]["main"]["model_command"]["pending"]["request_id"],
        invocation.request_id.as_str()
    );
    assert!(
        app.command(
            &json!({"action":"model_refresh","pane":"main","generation":generation}).to_string()
        )
        .await
        .is_err()
    );
    *backend.command_receipt.lock().unwrap() =
        Some(SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap());
    app.command(
        &json!({"action":"model_refresh","pane":"main","generation":generation}).to_string(),
    )
    .await
    .unwrap();
    assert_eq!(
        backend.commands.lock().unwrap().len(),
        1,
        "refresh must only query the original command"
    );
    let view: serde_json::Value = serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
    assert_eq!(view["surfaces"]["main"]["model"]["model"], "reasoner");
    assert_eq!(view["surfaces"]["main"]["reasoning_effort"], "max");
    assert_eq!(view["surfaces"]["main"]["effort_profile"]["default"], "low");
    assert!(view["surfaces"]["main"]["model_command"]["pending"].is_null());
    backend.model_outcome_unknown.store(false, Ordering::SeqCst);
    app.command(&selection(generation, None::<&str>).to_string())
        .await
        .unwrap();
    assert!(backend.commands.lock().unwrap()[1].arguments.value()["reasoning_effort"].is_null());
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn replaced_pane_cannot_dispatch_through_its_retired_controller() {
    let (runtime, backend, app, generation) = fixture().await;
    let prepared = prepare(&app, &generation, "old pane", vec![], false).await;
    let gate = Arc::new(HeaderGate::default());
    *backend.header_gate.lock().unwrap() = Some(gate.clone());
    let pending = app.dispatch_submission(
        rsi_gui::SurfaceId::MAIN,
        &generation,
        prepared["opaque"].as_str().unwrap(),
        "dispatch",
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), gate.entered.cancelled())
        .await
        .unwrap();
    let session = backend
        .header
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .session_id()
        .clone();
    app.command(&json!({"action":"open", "pane":"main","session":session}).to_string())
        .await
        .unwrap();
    gate.release.cancel();
    let result: serde_json::Value = serde_json::from_str(&pending.await.unwrap()).unwrap();
    assert_eq!(result["status"], "not_admitted", "{result}");
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn global_admission_failure_has_a_structured_unsent_result() {
    let (runtime, backend, app, generation) = fixture().await;
    let prepared = prepare(&app, &generation, "busy", vec![], false).await;
    let mut waiting = Vec::new();
    for _ in 0..8 {
        waiting.push(app.command(
            &json!({"action":"cancel","pane":"main","generation":generation}).to_string(),
        ));
    }
    let result = dispatch(&app, &generation, &prepared, "dispatch").await;
    assert_eq!(result["status"], "not_admitted", "{result}");
    assert!(result["error"].as_str().unwrap().contains("busy"));
    assert!(backend.requests.lock().unwrap().is_empty());
    drop(waiting);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn cancel_inspects_a_fresh_attachment_after_an_unknown_submission() {
    let (runtime, backend, app, generation) = fixture().await;
    let prepared = prepare(&app, &generation, "accepted without a reply", vec![], false).await;
    assert_eq!(
        dispatch(&app, &generation, &prepared, "dispatch").await["status"],
        "unknown"
    );
    let id = backend.requests.lock().unwrap()[0].message_id.clone();
    // A rejected later attempt is not evidence about the earlier unknown attempt.
    backend.reject_submissions.store(true, Ordering::SeqCst);
    assert_eq!(
        dispatch(&app, &generation, &prepared, "dispatch").await["status"],
        "not_admitted"
    );
    backend
        .pending_messages
        .lock()
        .unwrap()
        .push(rsi_agent_store_protocol::StorePendingMessage {
            message_id: id.clone(),
            delivery: MessageDelivery::NextTurn,
            target: MessageTarget::NextTurn,
            permits_promotion: false,
            bound_turn_id: None,
            accepted_control_seq: 1,
        });
    backend.unpublished.store(false, Ordering::SeqCst);
    app.command(&json!({"action":"cancel","pane":"main","generation":generation}).to_string())
        .await
        .unwrap();
    assert_eq!(
        *backend.cancel.lock().unwrap(),
        vec![CancelTarget::Message(id)]
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn mismatched_command_receipts_never_become_authoritative() {
    let (runtime, backend, app, generation) = fixture().await;
    let prepared = prepare(&app, &generation, "/plan on", vec![], false).await;
    let frozen: serde_json::Value =
        serde_json::from_str(prepared["opaque"].as_str().unwrap()).unwrap();
    let invocation: SessionCommandInvocation =
        serde_json::from_value(frozen["request"]["invocation"].clone()).unwrap();
    for (index, mut wrong) in [invocation.clone(), invocation.clone()]
        .into_iter()
        .enumerate()
    {
        if index == 0 {
            wrong.request_id = DomainRequestId::new("foreign-request").unwrap();
        } else {
            wrong.arguments = CommandArguments::new("different arguments".into()).unwrap();
        }
        *backend.command_receipt.lock().unwrap() =
            Some(SessionCommandReceipt::draft_changed(&wrong, "a".repeat(64)).unwrap());
        let mode = if index == 0 { "dispatch" } else { "query" };
        assert_eq!(
            dispatch(&app, &generation, &prepared, mode).await["status"],
            "unknown"
        );
        let view: serde_json::Value =
            serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap();
        assert!(view["surfaces"]["main"]["command_receipt"].is_null());
    }
    *backend.command_receipt.lock().unwrap() =
        Some(SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap());
    assert_eq!(
        dispatch(&app, &generation, &prepared, "query").await["status"],
        "complete"
    );
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn completed_receipts_cross_the_document_boundary_as_exact_json_strings() {
    let (runtime, backend, app, generation) = fixture().await;
    backend.resolution.store(2, Ordering::SeqCst);
    backend.receipt_sequence.store(u64::MAX, Ordering::SeqCst);
    let prepared = prepare(&app, &generation, "exact receipt", vec![], false).await;
    let result = dispatch(&app, &generation, &prepared, "dispatch").await;
    assert_eq!(result["status"], "complete");
    let receipt: serde_json::Value =
        serde_json::from_str(result["receipt"].as_str().expect("opaque receipt")).unwrap();
    assert_eq!(receipt["accepted_control_seq"].as_u64(), Some(u64::MAX));
    assert_eq!(receipt["observed_fact_seq"].as_u64(), Some(u64::MAX - 1));
    assert_eq!(receipt["message_id"], prepared["id"]);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn crafted_frozen_messages_cannot_change_sandbox_or_delivery_policy() {
    let (runtime, backend, app, generation) = fixture().await;
    let prepared = prepare(&app, &generation, "bounded input", vec![], false).await;
    let source: serde_json::Value =
        serde_json::from_str(prepared["opaque"].as_str().unwrap()).unwrap();
    let mut sandbox = source.clone();
    sandbox["request"]["input"]["sandbox"] =
        serde_json::to_value(rsi_sandbox::SandboxMode::DangerFullAccess).unwrap();
    let mut steer_model = source.clone();
    steer_model["request"]["input"]["delivery"] = json!("steer");
    steer_model["request"]["input"]["model"] = json!({"deployment":"test","model":"model"});
    let mut override_model = source.clone();
    override_model["request"]["input"]["model"] = json!({"deployment":"test","model":"model"});
    let mut effort = source.clone();
    effort["request"]["input"]["reasoning_effort"] = json!("high");
    let mut next_step = source;
    next_step["request"]["input"]["delivery"] = json!("next_step");
    for frozen in [sandbox, steer_model, override_model, effort, next_step] {
        let mut crafted = prepared.clone();
        crafted["opaque"] = json!(frozen.to_string());
        for mode in ["dispatch", "retry_message", "query"] {
            let result = dispatch(&app, &generation, &crafted, mode).await;
            assert_eq!(
                result["status"],
                if mode == "dispatch" {
                    "not_admitted"
                } else {
                    "unknown"
                },
                "{result}"
            );
            assert!(backend.requests.lock().unwrap().is_empty());
        }
    }
    backend.resolution.store(2, Ordering::SeqCst);
    for steer in [false, true] {
        let valid = prepare(&app, &generation, "valid", vec![], steer).await;
        assert_eq!(
            dispatch(&app, &generation, &valid, "dispatch").await["status"],
            "complete"
        );
    }
    assert_eq!(backend.requests.lock().unwrap().len(), 2);
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
pub(super) struct TerminalGate {
    pub(super) entered: tokio::sync::Semaphore,
    pub(super) release: CancellationToken,
}

#[tokio::test]
async fn terminal_poll_budget_is_independent_bounded_and_drained_on_shutdown() {
    let (runtime, backend, app, generation) = fixture().await;
    let gate = Arc::new(TerminalGate {
        entered: tokio::sync::Semaphore::new(0),
        release: CancellationToken::new(),
    });
    *backend.terminal_gate.lock().unwrap() = Some(gate.clone());
    let request =
        |request| json!({"pane":"main","generation":generation,"request":request}).to_string();
    for i in 0..33 {
        app.terminal(&request(
            json!({"type":"attach","terminal":format!("pty-{i}")}),
        ))
        .await
        .unwrap();
    }
    let mut reads = Vec::new();
    for i in 0..32 {
        reads.push(app.terminal(&request(
            json!({"type":"read","attachment":format!("pty-{i}")}),
        )));
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        gate.entered.acquire_many(32),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    let rejected = app
        .terminal(&request(json!({"type":"read","attachment":"pty-32"})))
        .await
        .unwrap_err();
    assert!(rejected.contains("busy"), "{rejected}");
    app.command(
        &json!({"action":"model_refresh","pane":"main","generation":generation}).to_string(),
    )
    .await
    .unwrap();
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(backend.terminal_detaches.lock().unwrap().len(), 33);
    for read in reads {
        assert!(read.await.unwrap_err().contains("Application closed"));
    }
    assert!(
        app.terminal(&request(json!({"type":"read","attachment":"pty-32"})))
            .await
            .is_err()
    );
}
