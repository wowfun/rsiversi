use super::*;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct Counter(AtomicUsize);
impl SurfaceRenderer for Counter {
    fn model(&self, _: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(UiModel::standard(UiView::default())?)
        })
    }
}
#[derive(Debug, Default)]
struct Captured {
    targets: Vec<Arc<UiTarget>>,
    target_leases: Vec<Arc<ContributionLease>>,
    contributions: Vec<Arc<ContributionLease>>,
}
#[derive(Debug)]
struct RegistryFixture {
    sources: [Arc<Counter>; 2],
    captured: Arc<Mutex<Captured>>,
}
#[async_trait]
impl PluginFactory for RegistryFixture {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let ui = plan.local::<UiContract>()?;
        let mut captured = self.captured.lock().unwrap();
        for source in &self.sources {
            let (target, lease) = ui.register_target(&plan, TargetKind::Application).unwrap();
            captured.targets.push(target);
            captured.target_leases.push(Arc::new(lease));
            let lease = ui
                .register(
                    &plan,
                    Contributions {
                        name: format!("counter-{}", captured.contributions.len()),
                        surfaces: vec![SurfaceContribution {
                            name: "panel".into(),
                            title: "Panel".into(),
                            target: TargetKind::Application,
                            renderer: source.clone(),
                        }],
                        actions: vec![],
                        renderers: vec![],
                    },
                )
                .unwrap();
            captured.contributions.push(Arc::new(lease));
        }
        Ok(())
    }
}
#[tokio::test]
async fn data_invalidation_reaches_only_its_contribution_target_or_presentation() {
    let runtime = Runtime::default();
    apply(&runtime, "ui", UiFactory, ConfigValue::Null).await;
    let sources = [Arc::new(Counter::default()), Arc::new(Counter::default())];
    let captured = Arc::new(Mutex::new(Captured::default()));
    let _owner = apply(
        &runtime,
        "counters",
        RegistryFixture {
            sources: sources.clone(),
            captured: captured.clone(),
        },
        ConfigValue::Null,
    )
    .await;
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    let (targets, target_leases, contributions) = {
        let captured = captured.lock().unwrap();
        (
            captured.targets.clone(),
            captured.target_leases.clone(),
            captured.contributions.clone(),
        )
    };
    let mut presentations = Vec::new();
    for target in &targets {
        for surface in ui.surfaces(target).unwrap() {
            let presentation = ui.present(&surface.reference).unwrap();
            presentation.ready().await.unwrap();
            presentations.push(presentation);
        }
    }
    let counts = || {
        sources
            .each_ref()
            .map(|source| source.0.load(Ordering::SeqCst))
    };
    assert_eq!(counts(), [2, 2]);
    let membership = ui.membership_changes();
    target_leases[0].invalidate().unwrap();
    until(|| presentations[0].status().revision == 2 && presentations[1].status().revision == 2)
        .await;
    assert_eq!(counts(), [3, 3]);
    assert_eq!(presentations[2].status().revision, 1);
    contributions[0].invalidate().unwrap();
    until(|| presentations[0].status().revision == 3 && presentations[2].status().revision == 2)
        .await;
    assert_eq!(counts(), [5, 3]);
    presentations[0].invalidate().unwrap();
    until(|| presentations[0].status().revision == 4).await;
    assert_eq!(counts(), [6, 3]);
    assert!(
        !membership.has_changed().unwrap(),
        "domain data does not invalidate menus"
    );
    target_leases[0].retire();
    assert!(matches!(
        target_leases[0].invalidate(),
        Err(UiError::Retired)
    ));
    assert!(matches!(
        presentations[0].invalidate(),
        Err(UiError::Retired)
    ));
    assert!(runtime.shutdown().await.is_clean());
}
