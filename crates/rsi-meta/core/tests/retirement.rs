use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, CancellationObserver, ConfigValue, Context, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Default)]
struct Capture {
    generations: Mutex<Vec<(Context, CancellationObserver)>>,
}
#[async_trait]
impl PluginFactory for Capture {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let observer = plan.context().retirement_observer()?;
        assert!(!observer.is_cancelled());
        self.generations
            .lock()
            .unwrap()
            .push((plan.context().clone(), observer.clone()));
        plan.defer(
            "join retirement observation",
            Box::new(move || {
                Box::pin(async move {
                    // The signal must already be observable before deferred cleanup runs.
                    assert!(observer.is_cancelled());
                    observer.cancelled().await;
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn retirement_observation_is_exact_to_generation_and_does_not_hold_admission() {
    let runtime = Runtime::default();
    let root = runtime.root().retirement_observer().unwrap();
    let capture = Arc::new(Capture::default());
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "retirement",
                "test",
                UpdateMode::Replayable,
                capture.clone(),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let (old, first) = capture.generations.lock().unwrap()[0].clone();
    assert!(!first.is_cancelled());
    tokio::time::timeout(
        Duration::from_secs(5),
        fiber.reconfigure(ConfigValue::Bool(true)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(first.is_cancelled());
    assert!(matches!(
        old.retirement_observer(),
        Err(MetaError::StaleContext { .. })
    ));
    let (_, second) = capture.generations.lock().unwrap()[1].clone();
    assert!(!second.is_cancelled());
    assert!(!root.is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_secs(5), fiber.dispose())
            .await
            .unwrap()
            .is_clean()
    );
    assert!(second.is_cancelled());
    // Retain both observers and stale Contexts while shutdown proves resource drain.
    assert!(
        tokio::time::timeout(Duration::from_secs(5), runtime.shutdown())
            .await
            .unwrap()
            .is_clean()
    );
    assert!(root.is_cancelled());
}
