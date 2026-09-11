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
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
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
    let mut absent_model = source;
    absent_model["request"]["input"]["model"] = json!(null);
    let mut next_step = absent_model.clone();
    next_step["request"]["input"]["delivery"] = json!("next_step");
    for frozen in [sandbox, steer_model, absent_model, next_step] {
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
