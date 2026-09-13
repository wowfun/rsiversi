use super::*;
use serde_json::{Value, json};
use sources::{fixture, view};
use std::sync::atomic::Ordering;

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn facts() -> Vec<SessionFact> {
    let turn_id = TurnId::new("turn").unwrap();
    let effect_id = EffectId::new("effect").unwrap();
    let identity =
        rsi_tools_protocol::ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64))
            .unwrap();
    vec![SessionFact::new(8, 1, SessionFactBody::ToolIntent {
        turn_id: turn_id.clone(), effect_id: effect_id.clone(), identity: identity.clone(), name: "apply_patch".into(), arguments: json!({"patch":"unused"}), approval: None, parallel_safe: false,
    }).unwrap(), SessionFact::new(9, 1, SessionFactBody::ToolResult {
        turn_id, effect_id, identity, result: rsi_tools_protocol::ToolResult::new(json!({"large":"unrelated".repeat(32_000),
            "evidence":{"version":1,"omitted":true,"diffs":[{"effect":0,"unified_diff":"--- a/a.rs\n+++ b/a.rs\n-old\n+recorded\n"}]}}), vec![], false).unwrap(),
    }).unwrap()]
}

#[derive(Debug, Default)]
struct FailingInline {
    reject: std::sync::atomic::AtomicBool,
}
impl rsi_ui::BlockRenderer for FailingInline {
    fn render(
        &self,
        _: &Context,
        _: &rsi_ui::BlockInput<'_>,
    ) -> rsi_ui::Result<Option<rsi_ui::UiView>> {
        Ok(None)
    }
    fn inline(
        &self,
        _: &Context,
        block: &rsi_ui::BlockInput<'_>,
    ) -> rsi_ui::Result<Option<Arc<dyn rsi_ui::SurfaceRenderer>>> {
        if self.reject.swap(false, Ordering::SeqCst) {
            return Err(rsi_ui::UiError::Action("fixture admission failed".into()));
        }
        Ok(Some(Arc::new(FailingCard(block.key.to_owned()))))
    }
}
#[derive(Debug)]
struct FailingCard(String);
#[async_trait]
impl rsi_ui::PresentationBindingOwner for FailingCard {
    fn retire(&self) {}
    async fn close(&self) -> rsi_ui::Result<()> {
        Err(rsi_ui::UiError::Action("fixture cleanup failed".into()))
    }
}
impl rsi_ui::SurfaceRenderer for FailingCard {
    fn bind(
        &self,
        context: Context,
        _: rsi_ui::PresentationIdentity,
        _: tokio_util::sync::CancellationToken,
    ) -> futures_util::future::BoxFuture<'_, rsi_ui::Result<Option<rsi_ui::PresentationBinding>>>
    {
        Box::pin(async move {
            Ok(Some(rsi_ui::PresentationBinding::new(
                context,
                Arc::new(Self(self.0.clone())),
            )))
        })
    }
    fn model(
        &self,
        _: Context,
    ) -> futures_util::future::BoxFuture<'_, rsi_ui::Result<rsi_ui::UiModel>> {
        Box::pin(async move {
            Ok(rsi_ui::UiModel::standard(rsi_ui::UiView {
                title: self.0.clone(),
                elements: vec![],
            })?)
        })
    }
}
#[derive(Debug)]
struct InlineFactory(Arc<FailingInline>);
#[async_trait]
impl PluginFactory for InlineFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null).requiring_local::<rsi_ui::UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<rsi_ui::UiContract>()?
            .register(
                &plan,
                rsi_ui::Contributions {
                    name: "fixture.inline".into(),
                    renderers: vec![rsi_ui::BlockRendererContribution {
                        name: "inline".into(),
                        target: rsi_ui::TargetKind::Surface,
                        renderer: self.0.clone(),
                    }],
                    ..Default::default()
                },
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "inline fixture",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn inline_close_failure_does_not_skip_replacement_and_failed_admission_can_retry() {
    let (runtime, backend, app) = fixture().await;
    let renderer = Arc::new(FailingInline::default());
    let plugin = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture.inline",
                "test",
                UpdateMode::Replayable,
                Arc::new(InlineFactory(renderer.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(plugin.snapshot().state, FiberState::Active);
    *backend.facts.lock().unwrap() = (1..=2)
        .map(|seq| {
            SessionFact::new(
                seq,
                seq,
                SessionFactBody::TurnAccepted {
                    turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
                    text: format!("input-{seq}"),
                    model: None,
                    sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap()
        })
        .collect();
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    let pane = view(&app)["surfaces"]["main"].clone();
    let first = pane["transcript"]["blocks"][0]["key"].as_str().unwrap();
    let second = pane["transcript"]["blocks"][1]["key"].as_str().unwrap();
    let visible = |sequence: &str, key: &str| {
        json!({"action":"ui_visible","pane":"main","generation":pane["generation"],"sequence":sequence,"keys":[key]}).to_string()
    };
    app.command(&visible("1", first)).await.unwrap();
    until(|| view(&app)["surfaces"]["main"]["inline"][first]["model"].is_object()).await;
    app.command(&visible("2", second)).await.unwrap();
    until(|| view(&app)["surfaces"]["main"]["inline"][second]["model"].is_object()).await;
    assert!(view(&app)["surfaces"]["main"]["inline"][first].is_null());
    assert!(
        view(&app)["notice"]
            .as_str()
            .unwrap()
            .contains("cleanup failed")
    );
    renderer.reject.store(true, Ordering::SeqCst);
    assert!(app.command(&visible("3", first)).await.is_err());
    app.command(&visible("3", first)).await.unwrap();
    until(|| view(&app)["surfaces"]["main"]["inline"][first]["model"].is_object()).await;
    let ticket = view(&app)["surfaces"]["main"]["inline"][first]["ticket"].clone();
    app.command(&visible("3", first)).await.unwrap();
    assert_eq!(
        view(&app)["surfaces"]["main"]["inline"][first]["ticket"],
        ticket
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn application_shutdown_drains_a_visible_inline_read() {
    let (runtime, backend, app) = fixture().await;
    *backend.facts.lock().unwrap() = facts();
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    let pane = view(&app)["surfaces"]["main"].clone();
    backend.block_source.store(true, Ordering::SeqCst);
    app.command(&json!({"action":"ui_visible","pane":"main","generation":pane["generation"],"sequence":"1","keys":[pane["transcript"]["blocks"][0]["key"]]}).to_string()).await.unwrap();
    until(|| backend.active_source.load(Ordering::SeqCst) == 1).await;
    let report = tokio::time::timeout(std::time::Duration::from_secs(3), runtime.shutdown())
        .await
        .expect("visible presentation must not block application shutdown");
    assert!(report.is_clean());
    assert_eq!(backend.active_source.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn inline_patch_uses_exact_evidence_and_retires_blocked_reads_and_old_tickets() {
    let (runtime, backend, app) = fixture().await;
    *backend.facts.lock().unwrap() = facts();
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
        .await
        .unwrap();
    let pane = view(&app)["surfaces"]["main"].clone();
    let key = pane["transcript"]["blocks"][0]["key"].as_str().unwrap();
    let visible = |sequence: u64, keys: Value| {
        json!({"action":"ui_visible","pane":"main","generation":pane["generation"],"sequence":sequence.to_string(),"keys":keys}).to_string()
    };
    app.command(&visible(1, json!([key]))).await.unwrap();
    until(|| view(&app)["surfaces"]["main"]["inline"][key]["model"].is_object()).await;
    let card = view(&app)["surfaces"]["main"]["inline"][key].clone();
    assert!(card.to_string().contains("+recorded"));
    assert!(card.to_string().contains("Some changes"));
    assert!(!card.to_string().contains("unrelated"));
    let complete = ui::button(&card, Some("Complete result"));
    app.command(&complete.to_string()).await.unwrap();
    assert!(
        view(&app)["surfaces"]["main"]["inline"][key]
            .to_string()
            .contains("unrelated")
    );
    let reads = backend.history_requests.lock().unwrap().len();
    assert!(app.command(&complete.to_string()).await.is_err());
    assert_eq!(backend.history_requests.lock().unwrap().len(), reads);
    app.command(&visible(2, json!([]))).await.unwrap();
    app.command(&visible(1, json!([key]))).await.unwrap();
    assert_eq!(view(&app)["surfaces"]["main"]["inline"], json!({}));
    assert!(
        app.command(&visible(3, json!([key, key, key, key, key])))
            .await
            .is_err()
    );
    backend.block_source.store(true, Ordering::SeqCst);
    app.command(&visible(4, json!([key]))).await.unwrap();
    until(|| backend.active_source.load(Ordering::SeqCst) == 1).await;
    app.command(&visible(5, json!([]))).await.unwrap();
    until(|| backend.active_source.load(Ordering::SeqCst) == 0).await;
    assert_eq!(view(&app)["surfaces"]["main"]["inline"], json!({}));
    app.command(&visible(6, json!([key]))).await.unwrap();
    until(|| backend.active_source.load(Ordering::SeqCst) == 1).await;
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    until(|| backend.active_source.load(Ordering::SeqCst) == 0).await;
    assert!(app.command(&complete.to_string()).await.is_err());
    assert!(backend.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
