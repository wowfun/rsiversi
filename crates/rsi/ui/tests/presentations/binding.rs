use super::*;

#[derive(Debug)]
struct Pending {
    started: AtomicBool,
    cancelling: AtomicBool,
    rollback: Semaphore,
}
impl SurfaceRenderer for Pending {
    fn bind(
        &self,
        _: Context,
        _: PresentationIdentity,
        stop: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'_, Result<Option<PresentationBinding>>> {
        Box::pin(async move {
            self.started.store(true, Ordering::SeqCst);
            stop.cancelled().await;
            self.cancelling.store(true, Ordering::SeqCst);
            self.rollback.acquire().await.unwrap().forget();
            Err(UiError::Retired)
        })
    }
}
#[derive(Debug)]
struct SourceFactory(Arc<Pending>);
#[async_trait]
impl PluginFactory for SourceFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let contribution = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "pending".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "pending".into(),
                        title: "Pending".into(),
                        target: TargetKind::Application,
                        renderer: self.0.clone(),
                    }],
                    actions: vec![],
                    renderers: vec![],
                },
            )
            .unwrap();
        plan.defer(
            "pending source",
            Box::new(move || {
                Box::pin(async move {
                    drop(contribution);
                    Ok(())
                })
            }),
        )
    }
}
#[tokio::test]
async fn close_requests_binding_cancellation_but_holds_admission_until_owned_rollback_finishes() {
    let runtime = Runtime::default();
    apply(&runtime, "ui", UiFactory, ConfigValue::Null).await;
    apply(
        &runtime,
        "target",
        UiTargetFactory,
        serde_json::json!("application"),
    )
    .await;
    let source = Arc::new(Pending {
        started: AtomicBool::new(false),
        cancelling: AtomicBool::new(false),
        rollback: Semaphore::new(0),
    });
    apply(
        &runtime,
        "pending",
        SourceFactory(source.clone()),
        ConfigValue::Null,
    )
    .await;
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
    let surface = ui.surfaces(&target).unwrap().remove(0).reference;
    let lease = ui.present(&surface).unwrap();
    until(|| source.started.load(Ordering::SeqCst)).await;
    let closing = tokio::spawn(async move { lease.close().await });
    until(|| source.cancelling.load(Ordering::SeqCst)).await;
    assert!(!closing.is_finished());
    assert_eq!(ui.presentation_usage(), (1, 1, MAXIMUM_VIEW_BYTES));
    source.rollback.add_permits(1);
    closing.await.unwrap().unwrap();
    assert_eq!(ui.presentation_usage(), (0, 0, 0));
    assert!(runtime.shutdown().await.is_clean());
}
