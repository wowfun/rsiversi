use super::*;

#[derive(Debug)]
struct OrderedSource {
    value: Arc<AtomicUsize>,
    reads: AtomicUsize,
    captured: Semaphore,
    release_read: Semaphore,
    action_started: Arc<Semaphore>,
    release_action: Arc<Semaphore>,
}
fn model(value: usize) -> UiModel {
    UiModel {
        renderer: "fixture".into(),
        schema: ModelSchema {
            name: "fixture".into(),
            version: 1,
        },
        data: serde_json::json!(value),
        actions: if value == 0 {
            vec![ModelAction {
                name: "run".into(),
                title: "Run".into(),
            }]
        } else {
            vec![]
        },
        sources: vec![],
        standard_view: None,
    }
}
impl SurfaceRenderer for OrderedSource {
    fn model(&self, _: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            let value = self.value.load(Ordering::SeqCst);
            if self.reads.fetch_add(1, Ordering::SeqCst) == 1 {
                self.captured.add_permits(1);
                self.release_read.acquire().await.unwrap().forget();
            }
            Ok(model(value))
        })
    }
}
impl UiAction for OrderedSource {
    fn invoke(&self, _: ActionTarget, _: ActionInput) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async {
            Err(UiError::Invalid(
                "fixture requires a model presentation".into(),
            ))
        })
    }
    fn invoke_model(&self, _: ActionTarget, _: ActionInput) -> BoxFuture<'static, Result<UiModel>> {
        let (value, started, release) = (
            self.value.clone(),
            self.action_started.clone(),
            self.release_action.clone(),
        );
        Box::pin(async move {
            started.add_permits(1);
            release.acquire().await.unwrap().forget();
            value.store(1, Ordering::SeqCst);
            Ok(model(1))
        })
    }
}
#[derive(Debug)]
struct OrderedFixture(Arc<OrderedSource>);
#[async_trait]
impl PluginFactory for OrderedFixture {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "ordered".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "panel".into(),
                        title: "Panel".into(),
                        target: TargetKind::Application,
                        renderer: self.0.clone(),
                    }],
                    actions: vec![ActionContribution {
                        name: "run".into(),
                        target: TargetKind::Application,
                        handler: self.0.clone(),
                    }],
                    renderers: vec![],
                },
            )
            .unwrap();
        plan.defer(
            "release ordered fixture",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )?;
        Ok(())
    }
}
async fn reordered(refresh_first: bool) {
    let runtime = Runtime::default();
    apply(&runtime, "ui", UiFactory, ConfigValue::Null).await;
    apply(
        &runtime,
        "target",
        UiTargetFactory,
        serde_json::json!("application"),
    )
    .await;
    let source = Arc::new(OrderedSource {
        value: Arc::new(AtomicUsize::new(0)),
        reads: AtomicUsize::new(0),
        captured: Semaphore::new(0),
        release_read: Semaphore::new(0),
        action_started: Arc::new(Semaphore::new(0)),
        release_action: Arc::new(Semaphore::new(0)),
    });
    apply(
        &runtime,
        "ordered",
        OrderedFixture(source.clone()),
        ConfigValue::Null,
    )
    .await;
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
    let lease = ui
        .present(&ui.surfaces(&target).unwrap()[0].reference)
        .unwrap();
    let first = lease.ready().await.unwrap();
    lease.invalidate().unwrap();
    source.captured.acquire().await.unwrap().forget();
    let action = lease.invoke(&first.action("run").unwrap(), ActionInput::default());
    source.action_started.acquire().await.unwrap().forget();
    if refresh_first {
        source.release_read.add_permits(1);
        // A released candidate must be discarded before any newer invalidation is sent.
        until(|| ui.presentation_usage().1 == 2).await;
        assert_eq!(
            lease.status().revision,
            1,
            "an admitted action fences a pending refresh"
        );
    }
    source.release_action.add_permits(1);
    assert_eq!(
        action.await.unwrap().model().model.data,
        serde_json::json!(1)
    );
    if !refresh_first {
        source.release_read.add_permits(1);
    }
    until(|| ui.presentation_usage().1 == 2).await;
    let current = lease.snapshot().unwrap().unwrap();
    assert_eq!(
        current.model().model.data,
        serde_json::json!(1),
        "old source must not overwrite the action result"
    );
    assert!(
        current.action("run").is_none(),
        "old action membership must not return"
    );
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        2,
        "successful action does not replace its own model with an unsolicited read"
    );
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn delayed_refresh_cannot_overwrite_action_result() {
    reordered(false).await;
}
#[tokio::test]
async fn refresh_cannot_invalidate_an_already_admitted_action() {
    reordered(true).await;
}
