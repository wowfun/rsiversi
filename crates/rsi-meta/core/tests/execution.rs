use rsi_meta::{Execution, Runtime, RuntimeLimits};
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn outside_tokio<T>(future: impl Future<Output = T>) -> T {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
            return result;
        }
        assert!(
            std::time::Instant::now() < until,
            "explicit execution stalled"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
}

#[test]
fn empty_runtime_shutdown_uses_explicit_execution_outside_tokio() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let runtime = Runtime::with_execution(
        RuntimeLimits::default(),
        Execution::native(executor.handle().clone()),
    )
    .unwrap();
    let result = outside_tokio(runtime.shutdown());
    assert!(result.is_clean());
}

#[derive(Debug)]
struct Empty;

#[async_trait::async_trait]
impl rsi_meta::PluginFactory for Empty {
    fn prepare(&self, _: &rsi_meta::ConfigValue) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(serde_json::Value::Null))
    }

    async fn activate(&self, _: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        Ok(())
    }
}

#[test]
fn apply_and_dispose_use_explicit_execution_outside_tokio() {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let runtime = Runtime::with_execution(
        RuntimeLimits::default(),
        Execution::native(executor.handle().clone()),
    )
    .unwrap();
    let factory = rsi_meta::ResolvedFactory::linked(
        "explicit-execution",
        "test",
        rsi_meta::UpdateMode::Replayable,
        Arc::new(Empty),
    );
    let fiber = outside_tokio(runtime.root().apply(factory, serde_json::Value::Null)).unwrap();
    assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
    outside_tokio(fiber.dispose());
    assert!(outside_tokio(runtime.shutdown()).is_clean());
}

#[derive(Debug, Default)]
struct AdvancingClock {
    ticks: Arc<std::sync::atomic::AtomicU64>,
    changed: Arc<tokio::sync::Notify>,
}
impl rsi_meta_execution::Backend for AdvancingClock {
    fn spawn(&self, future: futures_util::future::BoxFuture<'static, ()>) {
        drop(tokio::spawn(future));
    }
    fn prepare(&self, job: Box<dyn FnOnce() + Send>) {
        drop(tokio::task::spawn_blocking(job));
    }
    fn now(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.ticks.load(std::sync::atomic::Ordering::SeqCst))
    }
    fn sleep_until(&self, at: std::time::Duration) -> futures_util::future::BoxFuture<'static, ()> {
        let ticks = self.ticks.clone();
        let changed = self.changed.clone();
        Box::pin(async move {
            loop {
                let notified = changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if std::time::Duration::from_millis(ticks.load(std::sync::atomic::Ordering::SeqCst))
                    >= at
                {
                    return;
                }
                notified.await;
            }
        })
    }
}

#[derive(Debug)]
struct LateChild {
    clock: Arc<AdvancingClock>,
    alive: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for LateChild {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        self.alive.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let alive = self.alive.clone();
        plan.defer(
            "release child",
            Box::new(move || {
                Box::pin(async move {
                    alive.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        // One synchronous poll completes after its deadline in the explicit clock domain.
        self.clock
            .ticks
            .fetch_add(100, std::sync::atomic::Ordering::SeqCst);
        self.clock.changed.notify_waiters();
        Ok(())
    }
}
#[derive(Debug)]
struct ExistingParent {
    prepared: bool,
    child: Arc<LateChild>,
    rejected: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for ExistingParent {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        if plan.config().as_ref() == &serde_json::Value::Bool(true) {
            let factory = rsi_meta::ResolvedFactory::linked(
                "late-child",
                "test",
                rsi_meta::UpdateMode::Replayable,
                self.child.clone(),
            );
            let result = if self.prepared {
                let prepared = plan
                    .context()
                    .runtime()
                    .prepare(factory, serde_json::Value::Null)?;
                plan.context().apply_prepared(prepared).await
            } else {
                plan.context().apply(factory, serde_json::Value::Null).await
            };
            self.rejected.store(
                matches!(result, Err(rsi_meta::MetaError::Timeout(_))),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        Ok(())
    }
}

#[tokio::test]
async fn deadline_rejected_child_is_disposed_while_reconfigured_parent_survives() {
    verify_late_child(false).await;
}

#[tokio::test]
async fn deadline_rejected_prepared_child_is_disposed_while_parent_survives() {
    verify_late_child(true).await;
}

async fn verify_late_child(prepared: bool) {
    let clock = Arc::new(AdvancingClock::default());
    let alive = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let rejected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut limits = RuntimeLimits::default();
    limits.deadlines.transition = std::time::Duration::from_millis(20);
    let runtime = Runtime::with_execution(limits, Execution::new(clock.clone())).unwrap();
    let parent = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "existing-parent",
                "test",
                rsi_meta::UpdateMode::Replayable,
                Arc::new(ExistingParent {
                    prepared,
                    child: Arc::new(LateChild {
                        clock,
                        alive: alive.clone(),
                    }),
                    rejected: rejected.clone(),
                }),
            ),
            serde_json::Value::Bool(false),
        )
        .await
        .unwrap();
    let baseline = runtime.resource_snapshot();
    assert!(matches!(
        parent.reconfigure(serde_json::Value::Bool(true)).await,
        Err(rsi_meta::MetaError::Timeout(_))
    ));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(rejected.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(parent.snapshot().state, rsi_meta::FiberState::Active);
    assert_eq!(alive.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        runtime.resource_snapshot().fibers.current,
        baseline.fibers.current
    );
    assert!(runtime.shutdown().await.is_clean());
}
