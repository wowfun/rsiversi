use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_application::{
    ApplicationError, ApplicationLifetime, ApplicationRun, ApplicationRunContract,
};
use rsi_host::{HostBuilder, Profile, ProfileEntry, RunningHost};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, UpdateMode};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct Entry {
    entered: AtomicBool,
    complete: CancellationToken,
    draining: CancellationToken,
    drained: CancellationToken,
    fail_cleanup: bool,
}
impl ApplicationRun for Entry {
    fn run(self: Arc<Self>) -> BoxFuture<'static, rsi_application::Result<u8>> {
        Box::pin(async move {
            self.entered.store(true, Ordering::SeqCst);
            self.complete.cancelled().await;
            Ok(7)
        })
    }
}
#[derive(Debug)]
struct Factory(Arc<Entry>);
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let entry = self.0.clone();
        let supply = plan
            .context()
            .provide_local::<ApplicationRunContract>(entry.clone())?;
        plan.defer(
            "await retained application work",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    entry.draining.cancel();
                    entry.drained.cancelled().await;
                    if entry.fail_cleanup {
                        Err("fixture cleanup failure".into())
                    } else {
                        Ok(())
                    }
                })
            }),
        )
    }
}
async fn fixture(entry: Arc<Entry>) -> RunningHost {
    let mut builder = HostBuilder::without_paths("lifetime fixture");
    builder
        .register_local_contract::<ApplicationRunContract>()
        .unwrap();
    builder
        .register_linked(
            "entry",
            "1",
            UpdateMode::RestartRequired,
            Arc::new(Factory(entry)),
        )
        .unwrap();
    builder
        .build()
        .unwrap()
        .start(Profile::new([ProfileEntry::new(
            "entry",
            "entry",
            ConfigValue::Null,
        )]))
        .await
        .unwrap()
}

#[tokio::test]
async fn stop_before_entry_is_idempotent_and_waits_for_actual_cleanup() {
    let entry = Arc::new(Entry::default());
    let running = fixture(entry.clone()).await;
    let lifetime = ApplicationLifetime::default();
    lifetime.request_stop();
    lifetime.request_stop();
    let mut run = Box::pin(lifetime.run(&running));
    assert!(futures_util::poll!(&mut run).is_pending());
    entry.draining.cancelled().await;
    assert!(!entry.entered.load(Ordering::SeqCst));
    assert!(futures_util::poll!(Box::pin(lifetime.stopped())).is_pending());
    entry.drained.cancel();
    assert_eq!(run.await.unwrap(), 0);
    lifetime.stopped().await;
    assert!(matches!(
        lifetime.run(&running).await,
        Err(ApplicationError::AlreadyStarted)
    ));
}

#[tokio::test]
async fn normal_completion_and_cleanup_failure_use_the_same_fence() {
    for fail_cleanup in [false, true] {
        let entry = Arc::new(Entry {
            fail_cleanup,
            ..Default::default()
        });
        let running = fixture(entry.clone()).await;
        let lifetime = ApplicationLifetime::default();
        entry.complete.cancel();
        let mut run = Box::pin(lifetime.run(&running));
        assert!(futures_util::poll!(&mut run).is_pending());
        entry.draining.cancelled().await;
        assert!(entry.entered.load(Ordering::SeqCst));
        assert!(futures_util::poll!(Box::pin(lifetime.stopped())).is_pending());
        entry.drained.cancel();
        let result = run.await;
        if fail_cleanup {
            assert!(matches!(result, Err(ApplicationError::CleanupFailed)));
        } else {
            assert_eq!(result.unwrap(), 7);
        }
        lifetime.stopped().await;
    }
}
