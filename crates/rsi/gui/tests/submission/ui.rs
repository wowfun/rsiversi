use super::*;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use sources::{fixture, view};
use std::sync::atomic::Ordering;

pub(super) fn button(detail: &Value, label: Option<&str>) -> Value {
    let element = detail["model"]["standard_view"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| {
            element["kind"] == "button" && label.is_none_or(|label| element["label"] == label)
        })
        .unwrap();
    json!({"action":"ui_invoke","ticket":detail["ticket"],"name":element["action"],"input":{"value":element["value"],"fields":{}}})
}
pub(super) fn reference(detail: &Value, name: &Value) -> rsi_ui::UiReference {
    let mut reference = detail["binding"].clone();
    reference["name"] = name.clone();
    serde_json::from_value(reference).unwrap()
}
async fn idle_read(backend: &Backend, active: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while (backend.active_source.load(Ordering::SeqCst) > 0) != active {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn contributed_cards_read_exact_sources_and_close_reads_without_cancelling_session_work() {
    let (runtime, backend, app) = fixture().await;
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    let text = format!("<script>literal</script>{}", "界".repeat(20_000));
    backend.facts.lock().unwrap().push(
        SessionFact::new(
            9,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn").unwrap(),
                text: text.clone(),
                model: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    let current = view(&app);
    let pane = &current["surfaces"]["main"];
    assert_eq!(pane["ui_surfaces"][0]["title"], "Session details");
    assert_eq!(pane["ui_cards"], true);
    let card = json!({"action":"ui_block","pane":"main","generation":pane["generation"],"key":pane["transcript"]["blocks"][0]["key"]}).to_string();
    app.command(&card).await.unwrap();
    let first = view(&app)["ui_detail"].clone();
    let read = button(&first, None);
    app.command(&read.to_string()).await.unwrap();
    let first_page = view(&app)["ui_detail"].clone();
    let shown = first_page["model"]["standard_view"]["elements"][1]["text"]
        .as_str()
        .unwrap();
    assert!(shown.len() <= rsi_session_ui::SOURCE_PAGE_BYTES);
    assert_eq!(shown, &text[..shown.len()]);
    let next = button(&first_page, Some("Next page"));
    app.command(&next.to_string()).await.unwrap();
    let second_page = view(&app)["ui_detail"].clone();
    let second = second_page["model"]["standard_view"]["elements"][1]["text"]
        .as_str()
        .unwrap();
    assert_eq!(second, &text[shown.len()..shown.len() + second.len()]);
    let reads = backend.history_requests.lock().unwrap().len();
    app.command(&next.to_string()).await.unwrap();
    assert_eq!(backend.history_requests.lock().unwrap().len(), reads);
    assert_eq!(view(&app)["ui_detail"], second_page);
    backend.block_source.store(true, Ordering::SeqCst);
    let request = button(&second_page, Some("Next page"));
    let pending = app.command(&request.to_string());
    idle_read(&backend, true).await;
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    idle_read(&backend, false).await;
    pending.await.unwrap();
    assert!(view(&app)["ui_detail"].is_null());
    assert!(backend.cancel.lock().unwrap().is_empty());
    backend.block_source.store(false, Ordering::SeqCst);
    app.command(&card).await.unwrap();
    let stale = button(&view(&app)["ui_detail"], None);
    app.command(&json!({"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}).to_string())
        .await
        .unwrap();
    let reads = backend.history_requests.lock().unwrap().len();
    app.command(&stale.to_string()).await.unwrap();
    assert_eq!(backend.history_requests.lock().unwrap().len(), reads);
    assert!(view(&app)["ui_detail"].is_null());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct Addon {
    calls: std::sync::atomic::AtomicUsize,
    lease: Mutex<Option<Arc<rsi_ui::ContributionLease>>>,
}
impl rsi_ui::SurfaceRenderer for Addon {
    fn render(&self, _: &Context) -> rsi_ui::Result<rsi_ui::UiView> {
        Ok(rsi_ui::UiView {
            title: "Independent addon".into(),
            elements: vec![
                rsi_ui::UiElement::Input {
                    name: "value".into(),
                    label: "Value".into(),
                    value: "initial".into(),
                    multiline: true,
                },
                rsi_ui::UiElement::Button {
                    action: "echo".into(),
                    label: "Apply".into(),
                    value: Value::Null,
                },
            ],
        })
    }
}
#[derive(Debug)]
struct Echo(Arc<Addon>);
impl rsi_ui::UiAction for Echo {
    fn invoke(
        &self,
        target: rsi_ui::ActionTarget,
        input: rsi_ui::ActionInput,
    ) -> futures_util::future::BoxFuture<'static, rsi_ui::Result<rsi_ui::UiView>> {
        let addon = self.0.clone();
        Box::pin(async move {
            if !input.value.is_null()
                || input.fields.len() != 1
                || !input.fields.contains_key("value")
            {
                return Err(rsi_ui::UiError::Invalid("invalid echo payload".into()));
            }
            let controller = target
                .context()
                .lookup_local::<rsi_client::SessionControllerContract>()
                .ok_or(rsi_ui::UiError::Retired)?;
            addon.calls.fetch_add(1, Ordering::SeqCst);
            Ok(rsi_ui::UiView {
                title: controller.session_id().to_string(),
                elements: vec![rsi_ui::UiElement::Text {
                    text: input.fields["value"].clone(),
                }],
            })
        })
    }
}
#[derive(Debug)]
struct AddonFactory(Arc<Addon>);
#[async_trait]
impl PluginFactory for AddonFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null).requiring_local::<rsi_ui::UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<rsi_ui::UiContract>()?
            .register(
                &plan,
                rsi_ui::Contributions {
                    name: "test.addon".into(),
                    surfaces: vec![rsi_ui::SurfaceContribution {
                        name: "echo".into(),
                        title: "Independent addon".into(),
                        target: rsi_ui::TargetKind::Surface,
                        renderer: self.0.clone(),
                    }],
                    actions: vec![rsi_ui::ActionContribution {
                        name: "echo".into(),
                        target: rsi_ui::TargetKind::Surface,
                        handler: Arc::new(Echo(self.0.clone())),
                    }],
                    renderers: vec![],
                },
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        *self.0.lease.lock().unwrap() = Some(Arc::new(lease));
        Ok(())
    }
}
#[tokio::test]
async fn independent_addon_has_generic_fields_and_actions_with_cross_pane_and_withdrawal_fences() {
    let (runtime, _, app) = fixture().await;
    let addon = Arc::new(Addon::default());
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "test.addon",
                "test",
                UpdateMode::Replayable,
                Arc::new(AddonFactory(addon.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    app.command(r#"{"action":"add_surface","pane":"compare"}"#)
        .await
        .unwrap();
    app.command(&json!({"action":"create","pane":"compare","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}).to_string()).await.unwrap();
    let current = view(&app);
    let pane = &current["surfaces"]["main"];
    let menu = pane["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|surface| surface["title"] == "Independent addon")
        .unwrap();
    app.command(&json!({"action":"ui_surface","pane":"compare","generation":current["surfaces"]["compare"]["generation"],"reference":menu["reference"]}).to_string()).await.unwrap();
    assert!(view(&app)["ui_detail"]["model"].is_null());
    assert!(
        view(&app)["ui_detail"]["error"]
            .as_str()
            .unwrap()
            .contains("another")
    );
    let open = json!({"action":"ui_surface","pane":"main","generation":pane["generation"],"reference":menu["reference"]}).to_string();
    app.command(&open).await.unwrap();
    let mut invoke = button(&view(&app)["ui_detail"], Some("Apply"));
    invoke["input"]["fields"] = json!({"value":"<script>literal</script>界"});
    app.command(&invoke.to_string()).await.unwrap();
    let result = view(&app)["ui_detail"].clone();
    assert_eq!(result["model"]["standard_view"]["title"], pane["session"]);
    assert_eq!(
        result["model"]["standard_view"]["elements"][0]["text"],
        "<script>literal</script>界"
    );
    app.command(&invoke.to_string()).await.unwrap();
    assert_eq!(addon.calls.load(Ordering::SeqCst), 1);
    app.command(&open).await.unwrap();
    let lease = addon.lease.lock().unwrap().take().unwrap();
    assert!(lease.dispose().await.is_clean());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !view(&app)["ui_detail"].is_null() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !view(&app)["surfaces"]["main"]["ui_surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|surface| surface["title"] == "Independent addon")
    );
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct ModelOnly;
impl rsi_ui::SurfaceRenderer for ModelOnly {
    fn model(&self, _: rsi_meta::Context) -> BoxFuture<'_, rsi_ui::Result<rsi_ui::UiModel>> {
        Box::pin(async {
            Ok(rsi_ui::UiModel {
                renderer: "fixture.model".into(),
                schema: rsi_ui::ModelSchema {
                    name: "fixture.binary".into(),
                    version: 1,
                },
                data: json!({"label":"asynchronous model"}),
                actions: vec![],
                sources: vec![rsi_ui::ModelSource {
                    name: "raw".into(),
                    title: "Exact bytes".into(),
                    media_type: "application/octet-stream".into(),
                }],
                standard_view: None,
            })
        })
    }
    fn source(
        &self,
        _: rsi_ui::ActionTarget,
        _: String,
        offset: u64,
        maximum: usize,
    ) -> BoxFuture<'static, rsi_ui::Result<Vec<u8>>> {
        Box::pin(async move {
            Ok(b"\0\xffABC"
                .get(usize::try_from(offset).unwrap_or(usize::MAX)..)
                .unwrap_or_default()
                .iter()
                .copied()
                .take(maximum)
                .collect())
        })
    }
}
#[derive(Debug)]
struct ModelFactory;
#[async_trait]
impl PluginFactory for ModelFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null).requiring_local::<rsi_ui::UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<rsi_ui::UiContract>()?
            .register(
                &plan,
                rsi_ui::Contributions {
                    name: "fixture.model".into(),
                    surfaces: vec![rsi_ui::SurfaceContribution {
                        name: "binary".into(),
                        title: "Binary model".into(),
                        target: rsi_ui::TargetKind::Surface,
                        renderer: Arc::new(ModelOnly),
                    }],
                    ..rsi_ui::Contributions::default()
                },
            )
            .unwrap();
        plan.defer(
            "withdraw model fixture",
            Box::new(move || {
                Box::pin(async move {
                    lease.dispose().await;
                    Ok(())
                })
            }),
        )
    }
}
#[tokio::test]
async fn arbitrary_async_models_keep_exact_source_authority_and_close_every_snapshot_lease() {
    let (runtime, _, app) = fixture().await;
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture.model",
                "1",
                UpdateMode::RestartRequired,
                Arc::new(ModelFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    let pane = view(&app)["surfaces"]["main"].clone();
    let surface = pane["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["title"] == "Binary model")
        .unwrap();
    let open = json!({"action":"ui_surface","pane":"main","generation":pane["generation"],"reference":surface["reference"]}).to_string();
    let registry = runtime.root().lookup_local::<rsi_ui::UiContract>().unwrap();
    for _ in 0..24 {
        app.command(&open).await.unwrap();
        let detail = view(&app)["ui_detail"].clone();
        assert_eq!(detail["model"]["renderer"], "fixture.model");
        assert!(detail["model"]["standard_view"].is_null());
        let ticket = detail["ticket"].as_str().unwrap();
        assert_eq!(
            app.read_ui_source(ticket, "raw", 1, 3)
                .await
                .unwrap()
                .as_bytes(),
            b"\xffAB"
        );
        assert!(app.read_ui_source(ticket, "foreign", 0, 1).await.is_err());
        assert!(app.read_ui_source(ticket, "raw", 0, 65_537).await.is_err());
        assert!(app.read_ui_source("foreign", "raw", 0, 1).await.is_err());
        app.command(r#"{"action":"close_detail"}"#).await.unwrap();
        assert!(app.read_ui_source(ticket, "raw", 0, 1).await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while registry.presentation_usage() != (0, 0, 0) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    assert!(runtime.shutdown().await.is_clean());
}
