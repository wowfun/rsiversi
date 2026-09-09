use super::*;
use serde_json::{Value, json};
use sources::{fixture, view};
use std::sync::atomic::Ordering;

fn button(detail: &Value, label: Option<&str>) -> Value {
    let bound = &detail["view"];
    let element = bound["view"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| {
            element["kind"] == "button" && label.is_none_or(|label| element["label"] == label)
        })
        .unwrap();
    json!({"action":"ui_invoke","ticket":detail["ticket"],"reference":bound["actions"][element["action"].as_str().unwrap()],"input":{"value":element["value"],"fields":{}}})
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
    let session = view(&app)["panes"][0]["session"].clone();
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
    app.command(&json!({"action":"open","pane":0,"session":session}).to_string())
        .await
        .unwrap();
    let current = view(&app);
    let pane = &current["panes"][0];
    assert_eq!(pane["ui_surfaces"][0]["title"], "Session details");
    assert_eq!(pane["ui_cards"], true);
    let card = json!({"action":"ui_block","pane":0,"generation":pane["generation"],"key":pane["transcript"]["blocks"][0]["key"]}).to_string();
    app.command(&card).await.unwrap();
    let first = view(&app)["ui_detail"].clone();
    let read = button(&first, None);
    app.command(&read.to_string()).await.unwrap();
    let first_page = view(&app)["ui_detail"].clone();
    let shown = first_page["view"]["view"]["elements"][1]["text"]
        .as_str()
        .unwrap();
    assert!(shown.len() <= rsi_session_ui::SOURCE_PAGE_BYTES);
    assert_eq!(shown, &text[..shown.len()]);
    let next = button(&first_page, Some("Next page"));
    app.command(&next.to_string()).await.unwrap();
    let second_page = view(&app)["ui_detail"].clone();
    let second = second_page["view"]["view"]["elements"][1]["text"]
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
    app.command(&json!({"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}).to_string())
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
    app.command(&json!({"action":"create","pane":1,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}).to_string()).await.unwrap();
    let current = view(&app);
    let pane = &current["panes"][0];
    let menu = pane["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|surface| surface["title"] == "Independent addon")
        .unwrap();
    app.command(&json!({"action":"ui_surface","pane":1,"generation":current["panes"][1]["generation"],"reference":menu["reference"]}).to_string()).await.unwrap();
    assert!(view(&app)["ui_detail"]["view"].is_null());
    assert!(
        view(&app)["ui_detail"]["error"]
            .as_str()
            .unwrap()
            .contains("another")
    );
    let open = json!({"action":"ui_surface","pane":0,"generation":pane["generation"],"reference":menu["reference"]}).to_string();
    app.command(&open).await.unwrap();
    let mut invoke = button(&view(&app)["ui_detail"], Some("Apply"));
    invoke["input"]["fields"] = json!({"value":"<script>literal</script>界"});
    app.command(&invoke.to_string()).await.unwrap();
    let result = view(&app)["ui_detail"].clone();
    assert_eq!(result["view"]["view"]["title"], pane["session"]);
    assert_eq!(
        result["view"]["view"]["elements"][0]["text"],
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
        !view(&app)["panes"][0]["ui_surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|surface| surface["title"] == "Independent addon")
    );
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
