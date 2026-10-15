use super::*;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

pub(super) fn view(app: &rsi_gui::GuiApplication) -> Value {
    serde_json::from_slice(app.view().unwrap().as_bytes()).unwrap()
}

pub(super) fn panel(app: &rsi_gui::GuiApplication) -> Value {
    view(app)["panels"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap_or(Value::Null)
}
pub(super) async fn close_panel(app: &Arc<rsi_gui::GuiApplication>) {
    let selected = panel(app);
    app.command(&json!({"action":"close_detail","view":selected["view"]}).to_string())
        .await
        .unwrap();
}

pub(super) async fn fixture() -> (Runtime, Arc<Backend>, Arc<rsi_gui::GuiApplication>) {
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
                Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
    }
    let app = root
        .lookup_local::<rsi_gui::GuiApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#).await.unwrap();
    (runtime, backend, app)
}

#[tokio::test]
async fn source_details_page_exact_bytes_and_cancel_across_view_and_pane_replacement() {
    let (runtime, backend, app) = fixture().await;
    let initial = view(&app);
    let generation = &initial["surfaces"]["main"]["generation"];
    let text = format!("<script>literal</script>{}", "界".repeat(30_000));
    backend.facts.lock().unwrap().push(
        SessionFact::new(
            9,
            1,
            SessionFactBody::TurnAccepted {
                reasoning_effort: None,
                turn_id: TurnId::new("turn").unwrap(),
                text: text.clone(),
                model: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    let inspect = |seq| {
        json!({"action":"inspect_source","pane":"main","generation":generation,"source":{"seq":seq,"field":{"kind":"turn_input"}}}).to_string()
    };
    app.command(&inspect("9")).await.unwrap();
    let first = super::sources::panel(&app)["source_detail"].clone();
    assert_eq!(first["source"]["seq"], "9");
    assert_eq!(first["window"]["start"], 0);
    let end = usize::try_from(first["window"]["end"].as_u64().unwrap()).unwrap();
    assert!(end <= 64 * 1024);
    assert_eq!(first["window"]["text"], text[..end]);
    assert_eq!(first["window"]["more"], true);
    let page = json!({"action":"source_page","ticket":first["ticket"],"forward":true}).to_string();
    app.command(&page).await.unwrap();
    let second = super::sources::panel(&app)["source_detail"].clone();
    assert_eq!(second["window"]["start"], end);
    assert_eq!(second["window"]["text"], text[end..]);
    assert_eq!(second["window"]["more"], false);
    let reads = backend.history_requests.lock().unwrap().len();
    assert!(app.command(&page).await.is_err());
    assert_eq!(
        backend.history_requests.lock().unwrap().len(),
        reads,
        "stale page ticket performs no I/O"
    );
    assert_eq!(super::sources::panel(&app)["source_detail"], second);
    app.command(&inspect("10")).await.unwrap();
    assert!(
        super::sources::panel(&app)["source_detail"]["error"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    assert_eq!(view(&app)["notice"], "Resource view has retired");
    assert!(app.command(&inspect("09")).await.is_err());

    for replacement in ["close", "pane"] {
        let notice = view(&app)["notice"].clone();
        backend.block_source.store(true, Ordering::SeqCst);
        let old = app.command(&inspect("9"));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while backend.active_source.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(super::sources::panel(&app)["source_detail"]["window"].is_null());
        let pending_view = panel(&app)["view"].clone();
        backend.block_source.store(false, Ordering::SeqCst);
        match replacement {
            "close" => super::sources::close_panel(&app).await,
            "pane" => app
                .command(
                    &json!({"action":"open","pane":"main","session":initial["surfaces"]["main"]["session"]})
                        .to_string(),
                )
                .await
                .unwrap(),
            _ => unreachable!(),
        }
        old.await.unwrap();
        assert_eq!(backend.active_source.load(Ordering::SeqCst), 0);
        let current = view(&app);
        assert_eq!(
            current["notice"], notice,
            "late cancelled read cannot change notice"
        );
        assert!(
            !current["panels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|panel| panel["view"] == pending_view)
        );
        assert!(backend.cancel.lock().unwrap().is_empty());
    }
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn settled_source_is_fragment_independent_stable_and_invalidated_with_its_pane() {
    let (runtime, backend, app) = fixture().await;
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    let events = std::iter::once(rsi_ai_protocol::LanguageEvent::ContentStarted {
        index: 0,
        content: rsi_ai_protocol::ContentStart::Text,
    })
    .chain(
        (1..=100).map(|seq| rsi_ai_protocol::LanguageEvent::ContentDelta {
            index: 0,
            delta: rsi_ai_protocol::ContentDelta::Text(format!("text {seq}\n")),
        }),
    )
    .chain([
        rsi_ai_protocol::LanguageEvent::ContentFinished { index: 0 },
        rsi_ai_protocol::LanguageEvent::Finished {
            reason: rsi_ai_protocol::FinishReason::Stop,
            replay: None,
        },
    ]);
    backend.facts.lock().unwrap().push(
        SessionFact::new(
            1,
            1,
            SessionFactBody::ModelAttemptSettled {
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                attempt: rsi_agent_session_protocol::ModelAttemptLog::from_events(
                    events.map(|event| (0, event)),
                    true,
                )
                .unwrap(),
            },
        )
        .unwrap(),
    );
    let open = json!({"action":"open","pane":"main","session":session}).to_string();
    app.command(&open).await.unwrap();
    let current = view(&app);
    let pane = &current["surfaces"]["main"];
    let block = pane["transcript"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["role"] == "assistant")
        .unwrap();
    assert_eq!(block["sources"], 1);
    app.command(&json!({"action":"inspect_block","pane":"main","generation":pane["generation"],"key":block["key"]}).to_string()).await.unwrap();
    let first = super::sources::panel(&app)["block_sources"].clone();
    assert_eq!(first["page"].as_array().unwrap().len(), 1);
    assert_eq!(first["page"][0]["seq"], "1");
    assert!(first.get("sources").is_none());
    let reads = backend.history_requests.lock().unwrap().len();
    backend.facts.lock().unwrap().clear();
    let next =
        json!({"action":"block_sources_page","ticket":first["ticket"],"forward":true}).to_string();
    app.command(&next).await.unwrap();
    let unchanged = super::sources::panel(&app)["block_sources"].clone();
    assert_eq!(
        unchanged["page"], first["page"],
        "one settlement cannot manufacture another source page"
    );
    assert_ne!(unchanged["ticket"], first["ticket"]);
    assert_eq!(backend.history_requests.lock().unwrap().len(), reads);
    let next = json!({"action":"block_sources_page","ticket":unchanged["ticket"],"forward":true})
        .to_string();
    app.command(&open).await.unwrap();
    assert!(super::sources::panel(&app)["block_sources"].is_null());
    assert!(app.command(&next).await.is_err());
    assert!(super::sources::panel(&app)["block_sources"].is_null());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn simultaneous_source_views_keep_independent_reads_across_float_settings_close_and_reopen() {
    let (runtime, backend, app) = fixture().await;
    let generation = view(&app)["surfaces"]["main"]["generation"].clone();
    let inspect = json!({"action":"inspect_source","pane":"main","generation":generation,"source":{"seq":"1","field":{"kind":"turn_input"}}}).to_string();
    backend.block_source.store(true, Ordering::SeqCst);
    let first = app.command(&inspect);
    let wait = async |count| {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while backend.active_source.load(Ordering::SeqCst) != count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    };
    wait(1).await;
    let first_view = panel(&app);
    let second = app.command(&inspect);
    wait(2).await;
    let second_view = panel(&app);
    assert_ne!(first_view["view"], second_view["view"]);
    app.command(
        &json!({"action":"float_panel","view":first_view["view"],"floating":true}).to_string(),
    )
    .await
    .unwrap();
    // Closing the independent settings owner cannot cancel either source read.
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    assert_eq!(backend.active_source.load(Ordering::SeqCst), 2);
    app.command(&json!({"action":"close_detail","view":first_view["view"]}).to_string())
        .await
        .unwrap();
    wait(1).await;
    first.await.unwrap();
    assert_eq!(view(&app)["panels"].as_array().unwrap().len(), 1);
    assert_eq!(panel(&app)["view"], second_view["view"]);
    let reads = backend.history_requests.lock().unwrap().len();
    assert!(app.command(&json!({"action":"source_page","ticket":first_view["source_detail"]["ticket"],"forward":true}).to_string()).await.is_err());
    assert_eq!(backend.history_requests.lock().unwrap().len(), reads);
    let reopened = app.command(&inspect);
    wait(2).await;
    let reopened_view = panel(&app);
    assert_ne!(first_view["view"], reopened_view["view"]);
    assert_ne!(
        first_view["source_detail"]["ticket"],
        reopened_view["source_detail"]["ticket"]
    );
    app.command(r#"{"action":"close_surface","pane":"main"}"#)
        .await
        .unwrap();
    wait(0).await;
    second.await.unwrap();
    reopened.await.unwrap();
    assert!(view(&app)["panels"].as_array().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
