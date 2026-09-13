use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberHandle, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use rsi_ui::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

#[path = "presentations/binding.rs"]
mod binding;
#[path = "presentations/inline.rs"]
mod inline;
#[path = "presentations/invalidation.rs"]
mod invalidation;
#[path = "presentations/ordering.rs"]
mod ordering;

#[derive(Debug)]
struct Source {
    reads: AtomicUsize,
    mutations: AtomicUsize,
    fail: AtomicBool,
    unknown_action: AtomicBool,
    fail_action: AtomicBool,
    panic_action: AtomicBool,
    gate: Semaphore,
    mutating: Semaphore,
}
impl Source {
    fn view(&self) -> UiView {
        UiView {
            title: self.reads.load(Ordering::SeqCst).to_string(),
            elements: vec![UiElement::Button {
                action: if self.unknown_action.load(Ordering::SeqCst) {
                    "unregistered"
                } else {
                    "run"
                }
                .into(),
                label: "Run".into(),
                value: ConfigValue::Null,
            }],
        }
    }
}
impl SurfaceRenderer for Source {
    fn model(&self, _: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.gate.acquire().await.unwrap().forget();
            if self.fail.load(Ordering::SeqCst) {
                return Err(UiError::Action("source unavailable".into()));
            }
            let mut model = UiModel::standard(self.view())?;
            model.sources.push(ModelSource {
                name: "raw".into(),
                title: "Raw source".into(),
                media_type: "text/plain".into(),
            });
            Ok(model)
        })
    }
    fn source(
        &self,
        _: ActionTarget,
        _: String,
        offset: u64,
        maximum: usize,
    ) -> BoxFuture<'static, Result<Vec<u8>>> {
        Box::pin(async move {
            Ok(b"source"
                .get(usize::try_from(offset).unwrap_or(usize::MAX)..)
                .unwrap_or_default()
                .iter()
                .take(maximum)
                .copied()
                .collect())
        })
    }
}
#[derive(Debug)]
struct Action(Arc<Source>);
impl UiAction for Action {
    fn invoke(&self, _: ActionTarget, _: ActionInput) -> BoxFuture<'static, Result<UiView>> {
        let source = self.0.clone();
        Box::pin(async move {
            source.mutations.fetch_add(1, Ordering::SeqCst);
            source.mutating.acquire().await.unwrap().forget();
            assert!(
                !source.panic_action.load(Ordering::SeqCst),
                "fixture action panic"
            );
            if source.fail_action.load(Ordering::SeqCst) {
                return Err(UiError::Action("fixture action failure".into()));
            }
            Ok(source.view())
        })
    }
}
#[derive(Debug)]
struct Fixture(Arc<Source>);
#[async_trait]
impl PluginFactory for Fixture {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let ui = plan.local::<UiContract>()?;
        let lease = ui
            .register(
                &plan,
                Contributions {
                    name: "fixture".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "panel".into(),
                        title: "Panel".into(),
                        target: TargetKind::Application,
                        renderer: self.0.clone(),
                    }],
                    actions: ["run", "hidden"]
                        .into_iter()
                        .map(|name| ActionContribution {
                            name: name.into(),
                            target: TargetKind::Application,
                            handler: Arc::new(Action(self.0.clone())),
                        })
                        .collect(),
                    renderers: vec![BlockRendererContribution {
                        name: "inline".into(),
                        target: TargetKind::Application,
                        renderer: Arc::new(inline::Inline(self.0.clone())),
                    }],
                },
            )
            .unwrap();
        plan.defer(
            "release fixture",
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
async fn apply(
    runtime: &Runtime,
    id: &str,
    factory: impl PluginFactory,
    config: ConfigValue,
) -> FiberHandle {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(id, "1", UpdateMode::Replayable, Arc::new(factory)),
            config,
        )
        .await
        .unwrap()
}
async fn setup() -> (
    Runtime,
    Arc<Ui>,
    Arc<Source>,
    FiberHandle,
    PresentationLease,
) {
    let runtime = Runtime::default();
    apply(&runtime, "ui", UiFactory, ConfigValue::Null).await;
    apply(
        &runtime,
        "target",
        UiTargetFactory,
        serde_json::json!("application"),
    )
    .await;
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
        mutations: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
        unknown_action: AtomicBool::new(false),
        fail_action: AtomicBool::new(false),
        panic_action: AtomicBool::new(false),
        gate: Semaphore::new(0),
        mutating: Semaphore::new(0),
    });
    let fiber = apply(
        &runtime,
        "fixture",
        Fixture(source.clone()),
        ConfigValue::Null,
    )
    .await;
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
    let surface = ui.surfaces(&target).unwrap().remove(0).reference;
    let lease = ui.present(&surface).unwrap();
    (runtime, ui, source, fiber, lease)
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn asynchronous_refresh_preserves_current_data_and_fences_displayed_membership() {
    let (runtime, ui, source, _fiber, lease) = setup().await;
    until(|| source.reads.load(Ordering::SeqCst) == 1).await;
    assert!(lease.snapshot().unwrap().is_none());
    assert_eq!(ui.presentation_usage(), (1, 1, MAXIMUM_VIEW_BYTES));
    source.gate.add_permits(1);
    let first = lease.ready().await.unwrap();
    for _ in 0..10 {
        lease.snapshot().unwrap();
    }
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        1,
        "synchronous draw never invokes a source"
    );
    assert_eq!(
        lease
            .source(first.revision(), "raw", 1, 3)
            .await
            .unwrap()
            .as_bytes(),
        b"our"
    );
    assert!(
        lease
            .source(first.revision(), "forged", 0, 3)
            .await
            .is_err()
    );
    let mut hidden = first.action("run").unwrap();
    hidden.action = "hidden".into();
    assert!(lease.invoke(&hidden, ActionInput::default()).await.is_err());
    assert_eq!(source.mutations.load(Ordering::SeqCst), 0);
    source.fail.store(true, Ordering::SeqCst);
    source.gate.add_permits(1);
    lease.invalidate().unwrap();
    until(|| lease.status().diagnostic.is_some()).await;
    assert_eq!(
        lease.snapshot().unwrap().unwrap().revision(),
        first.revision()
    );
    source.fail.store(false, Ordering::SeqCst);
    source.gate.add_permits(1);
    lease.invalidate().unwrap();
    until(|| lease.status().revision == 2).await;
    assert!(lease.status().diagnostic.is_none());
    assert!(
        lease
            .invoke(&first.action("run").unwrap(), ActionInput::default())
            .await
            .is_err()
    );
    lease.close().await.unwrap();
    assert!(lease.snapshot().is_err());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn escaped_transport_bytes_keep_snapshot_slots_and_retired_readers_keep_their_budget() {
    let (runtime, ui, source, _fiber, lease) = setup().await;
    source.gate.add_permits(1);
    let mut readers = vec![lease.ready().await.unwrap().bytes().clone().into_bytes()];
    for revision in 2..=MAXIMUM_SNAPSHOTS as u64 {
        source.gate.add_permits(1);
        lease.invalidate().unwrap();
        until(|| lease.status().revision == revision).await;
        readers.push(
            lease
                .snapshot()
                .unwrap()
                .unwrap()
                .bytes()
                .clone()
                .into_bytes(),
        );
    }
    assert_eq!(ui.presentation_usage().1, MAXIMUM_SNAPSHOTS);
    lease.invalidate().unwrap();
    until(|| lease.status().diagnostic.is_some()).await;
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        MAXIMUM_SNAPSHOTS,
        "candidate capacity precedes source invocation"
    );
    readers.remove(0);
    source.gate.add_permits(1);
    until(|| lease.status().revision == MAXIMUM_SNAPSHOTS as u64 + 1).await;
    lease.close().await.unwrap();
    assert_eq!(ui.presentation_usage().0, 0);
    assert_eq!(ui.presentation_usage().1, MAXIMUM_SNAPSHOTS - 1);
    assert!(ui.presentation_usage().2 > 0);
    assert!(runtime.shutdown().await.is_clean());
    drop(readers);
    assert_eq!(ui.presentation_usage(), (0, 0, 0));
}

#[tokio::test]
async fn dropped_action_waiter_and_presentation_close_drain_admitted_mutation() {
    let (runtime, ui, source, fiber, lease) = setup().await;
    source.gate.add_permits(1);
    let snapshot = lease.ready().await.unwrap();
    drop(lease.invoke(&snapshot.action("run").unwrap(), ActionInput::default()));
    until(|| source.mutations.load(Ordering::SeqCst) == 1).await;
    let dispose = tokio::spawn(async move { fiber.dispose().await });
    until(|| lease.status().stopped).await;
    assert!(!dispose.is_finished());
    assert_eq!(
        ui.presentation_usage().0,
        1,
        "draining presentation keeps admission"
    );
    source.mutating.add_permits(1);
    assert!(dispose.await.unwrap().is_clean());
    lease.close().await.unwrap();
    assert_eq!(source.mutations.load(Ordering::SeqCst), 1);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn action_executed_before_retirement_has_an_unknown_outcome() {
    let (runtime, _ui, source, fiber, lease) = setup().await;
    source.gate.add_permits(1);
    let snapshot = lease.ready().await.unwrap();
    let result = lease.invoke(&snapshot.action("run").unwrap(), ActionInput::default());
    until(|| source.mutations.load(Ordering::SeqCst) == 1).await;
    let dispose = tokio::spawn(async move { fiber.dispose().await });
    until(|| lease.status().stopped).await;
    source.mutating.add_permits(1);
    assert!(matches!(result.await, Err(UiError::Action(_))));
    assert!(dispose.await.unwrap().is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn models_cannot_publish_actions_outside_the_contribution() {
    let (runtime, _ui, source, _fiber, lease) = setup().await;
    source.unknown_action.store(true, Ordering::SeqCst);
    source.gate.add_permits(1);
    assert!(matches!(lease.ready().await, Err(UiError::Invalid(_))));
    assert!(lease.snapshot().unwrap().is_none());
    assert!(
        lease
            .status()
            .diagnostic
            .unwrap()
            .contains("unavailable action")
    );
    source.unknown_action.store(false, Ordering::SeqCst);
    source.gate.add_permits(1);
    lease.invalidate().unwrap();
    until(|| lease.status().revision == 1).await;
    assert!(lease.snapshot().unwrap().unwrap().action("run").is_some());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn invalidation_during_action_coalesces_to_one_trailing_read() {
    let (runtime, _ui, source, _fiber, lease) = setup().await;
    source.gate.add_permits(1);
    let snapshot = lease.ready().await.unwrap();
    let action = lease.invoke(&snapshot.action("run").unwrap(), ActionInput::default());
    until(|| source.mutations.load(Ordering::SeqCst) == 1).await;
    for _ in 0..100 {
        lease.invalidate().unwrap();
    }
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            until(|| source.reads.load(Ordering::SeqCst) != 1),
        )
        .await
        .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    source.gate.add_permits(1);
    source.mutating.add_permits(1);
    action.await.unwrap();
    until(|| lease.status().revision == 3).await;
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn failed_and_panicking_actions_refresh_even_after_the_waiter_is_dropped() {
    for panic in [false, true] {
        let (runtime, _ui, source, _fiber, lease) = setup().await;
        source.gate.add_permits(1);
        let snapshot = lease.ready().await.unwrap();
        source.fail_action.store(!panic, Ordering::SeqCst);
        source.panic_action.store(panic, Ordering::SeqCst);
        drop(lease.invoke(&snapshot.action("run").unwrap(), ActionInput::default()));
        until(|| source.mutations.load(Ordering::SeqCst) == 1).await;
        source.gate.add_permits(1);
        source.mutating.add_permits(1);
        until(|| lease.status().revision == 2).await;
        assert_eq!(source.reads.load(Ordering::SeqCst), 2);
        assert_eq!(source.mutations.load(Ordering::SeqCst), 1);
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn concurrent_action_replies_keep_their_original_predecessor() {
    let (runtime, _ui, source, _fiber, lease) = setup().await;
    source.gate.add_permits(1);
    let snapshot = lease.ready().await.unwrap();
    let reference = snapshot.action("run").unwrap();
    let first = lease.invoke(&reference, ActionInput::default());
    let second = lease.invoke(&reference, ActionInput::default());
    until(|| source.mutations.load(Ordering::SeqCst) == 2).await;
    source.gate.add_permits(1);
    source.mutating.add_permits(2);
    let results = [first.await, second.await];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(UiError::Action(_))))
            .count(),
        1
    );
    until(|| lease.status().revision == 3).await;
    assert_eq!(source.mutations.load(Ordering::SeqCst), 2);
    assert!(runtime.shutdown().await.is_clean());
}
