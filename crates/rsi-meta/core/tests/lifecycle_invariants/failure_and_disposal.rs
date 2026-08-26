use super::*;

#[derive(Debug)]
struct PanickingActivationFactory(FactorySpec);

#[async_trait]
impl PluginFactory for PanickingActivationFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        panic!("activation panic evidence")
    }
}

#[derive(Debug)]
struct HangingActivationFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    cleanups: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct BlockingReconfigurationFactory {
    spec: FactorySpec,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    activated: Arc<Notify>,
    configurations: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl PluginFactory for BlockingReconfigurationFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if *config == 1 {
            self.entered.wait();
            self.release.wait();
        }
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let config = Arc::clone(plan.config());
        self.configurations
            .lock()
            .expect("configuration log poisoned")
            .push((*config).clone());
        self.activated.notify_one();
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for HangingActivationFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "timed-out activation rollback",
            Box::new(move || {
                let cleanups = Arc::clone(&cleanups);
                async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn activation_waiter_timeout_keeps_runtime_owned_rollback_exactly_once() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            service_call: std::time::Duration::from_millis(10),
            event_dispatch: std::time::Duration::from_millis(10),
            shutdown_wait: std::time::Duration::from_millis(20),
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let cleanups = Arc::new(AtomicUsize::new(0));
    let applying = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let cleanups = Arc::clone(&cleanups);
        async move {
            root.apply(
                Arc::new(HangingActivationFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin("hanging-activation", "1")),
                    entered,
                    cleanups,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    let loading = runtime.resource_snapshot();
    assert_eq!(loading.effect_transactions.current, 1);
    assert_eq!(loading.effects.current, 1);
    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        applying.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if cleanups.load(Ordering::Acquire) == 1
                && runtime.snapshot().fibers.is_empty()
                && runtime.resource_snapshot().cleanup_runs.current == 0
                && runtime.resource_snapshot().effect_transactions.current == 0
                && runtime.resource_snapshot().effects.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed-out application did not finish its persistent rollback and disposal");
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn reconfiguration_waiter_timeout_leaves_one_busy_runtime_owned_transaction() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let activated = Arc::new(Notify::new());
    let configurations = Arc::new(Mutex::new(Vec::new()));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(BlockingReconfigurationFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("timed-out-reconfiguration", "1")),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                activated: Arc::clone(&activated),
                configurations: Arc::clone(&configurations),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    activated.notified().await;

    let reconfiguration = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.reconfigure(Value::from(1)).await }
    });
    tokio::task::spawn_blocking({
        let entered = Arc::clone(&entered);
        move || entered.wait()
    })
    .await
    .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        reconfiguration.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    assert_eq!(
        fiber.reconfigure(Value::from(2)).await.unwrap_err(),
        MetaError::Busy {
            operation: "plugin reconfiguration",
        },
    );

    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    activated.notified().await;
    assert_eq!(
        *configurations.lock().expect("configuration log poisoned"),
        vec![Value::Null, Value::from(1)],
    );

    let settled = loop {
        match fiber.reconfigure(Value::from(2)).await {
            Err(MetaError::Busy {
                operation: "plugin reconfiguration",
            }) => tokio::task::yield_now().await,
            result => break result,
        }
    };
    assert!(matches!(settled.unwrap().state, FiberState::Active));
    assert_eq!(
        *configurations.lock().expect("configuration log poisoned"),
        vec![Value::Null, Value::from(1), Value::from(2)],
    );
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn activation_panics_become_failed_fibers_without_poisoning_the_runtime() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(PanickingActivationFactory(FactorySpec::new(
                FactoryIdentity::builtin("panicking-activation", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        fiber.snapshot().state,
        FiberState::Failed(ref error) if error.contains("panicked")
    ));
    let healthy = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(FactorySpec::new(FactoryIdentity::builtin(
                "healthy-after-panic",
                "1",
            )))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(healthy.snapshot().state, FiberState::Active));
}

#[derive(Debug)]
struct PanickingCleanupFactory(FactorySpec);

#[async_trait]
impl PluginFactory for PanickingCleanupFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context.defer(
            "panicking cleanup",
            Box::new(|| async move { panic!("cleanup panic evidence") }.boxed()),
        )
    }
}

#[tokio::test]
async fn cleanup_panics_become_joinable_failures() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(PanickingCleanupFactory(FactorySpec::new(
                FactoryIdentity::builtin("panicking-cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let report = fiber.dispose().await;
    assert_eq!(report.failures().len(), 1);
    assert_eq!(report.failures()[0].label, "panicking cleanup");
    assert!(report.failures()[0].error.contains("panicked"));
}

#[derive(Debug)]
struct PanickingDropEndpoint;

impl Drop for PanickingDropEndpoint {
    fn drop(&mut self) {
        panic!("endpoint drop panic evidence");
    }
}

#[async_trait]
impl ServiceEndpoint for PanickingDropEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct PanickingDropFactory(FactorySpec);

#[async_trait]
impl PluginFactory for PanickingDropFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context.provide(
            "panicking-drop",
            "test.panicking-drop",
            V1,
            Arc::new(PanickingDropEndpoint),
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn endpoint_destructor_panics_are_owned_cleanup_failures_not_runtime_terminal_state() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(PanickingDropFactory(FactorySpec::new(
                FactoryIdentity::builtin("panicking-drop", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();

    let report = fiber.dispose().await;
    assert_eq!(report.failures().len(), 1);
    assert_eq!(
        report.failures()[0].label,
        "withdraw dynamic service supply"
    );
    assert!(
        report.failures()[0]
            .error
            .contains("service endpoint destructor panicked")
    );
    assert!(runtime.snapshot().terminal.is_none());

    let survivor = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(FactorySpec::new(FactoryIdentity::builtin(
                "post-endpoint-panic",
                "1",
            )))),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&survivor).await;
    assert!(survivor.dispose().await.is_clean());
}

#[derive(Debug)]
struct DropFactory {
    spec: FactorySpec,
    dropped: Arc<AtomicUsize>,
}

impl Drop for DropFactory {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl PluginFactory for DropFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn dropping_all_public_owners_releases_registered_fibers() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::default();
    let handle = runtime
        .root()
        .apply(
            Arc::new(DropFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("drop-probe", "1")),
                dropped: Arc::clone(&dropped),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    drop(runtime);
    drop(handle);
    assert_eq!(dropped.load(Ordering::Acquire), 1);
}

#[derive(Debug)]
struct FailingCleanupFactory(FactorySpec);

#[async_trait]
impl PluginFactory for FailingCleanupFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context.defer(
            "forced failure",
            Box::new(|| async { Err("cleanup evidence".to_owned()) }.boxed()),
        )
    }
}

#[derive(Debug)]
struct CancelledDisposalFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: CancellationToken,
    cleaned: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for CancelledDisposalFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let entered = Arc::clone(&self.entered);
        let release = self.release.clone();
        let cleaned = Arc::clone(&self.cleaned);
        context.defer(
            "cancelled disposal",
            Box::new(move || {
                async move {
                    entered.notify_one();
                    release.cancelled().await;
                    cleaned.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn cancelling_public_dispose_does_not_cancel_runtime_owned_cleanup() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let cleaned = Arc::new(AtomicUsize::new(0));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(CancelledDisposalFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("cancelled-disposal", "1")),
                entered: Arc::clone(&entered),
                release: release.clone(),
                cleaned: Arc::clone(&cleaned),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let cancelled = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.dispose().await }
    });
    entered.notified().await;
    cancelled.abort();
    let _ = cancelled.await;
    release.cancel();

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), fiber.dispose())
        .await
        .expect("a later disposer did not join cleanup");
    assert!(report.is_clean());
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
    assert!(matches!(fiber.snapshot().state, FiberState::Disposed));
    assert!(runtime.snapshot().fibers.is_empty());
}

#[derive(Debug)]
struct PanickingFactoryPayload;

impl Drop for PanickingFactoryPayload {
    fn drop(&mut self) {
        panic!("panic payload destructor panicked");
    }
}

#[derive(Debug)]
struct PanickingFactoryDrop;

impl Drop for PanickingFactoryDrop {
    fn drop(&mut self) {
        std::panic::panic_any(PanickingFactoryPayload);
    }
}

#[async_trait]
impl PluginFactory for PanickingFactoryDrop {
    fn identity(&self) -> FactoryIdentity {
        FactoryIdentity::builtin("panicking-factory-drop", "1")
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn reconciliation_contains_a_panicking_caught_payload_destructor() {
    let runtime = Runtime::default();
    let factory = Arc::new(PanickingFactoryDrop);
    let broken = runtime
        .root()
        .apply(factory.clone(), Value::Null)
        .await
        .unwrap();
    let survivor = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(FactorySpec::new(FactoryIdentity::builtin(
                "payload-drop-survivor",
                "1",
            )))),
            Value::Null,
        )
        .await
        .unwrap();
    drop(factory);

    let broken_disposal = std::panic::AssertUnwindSafe(broken.dispose())
        .catch_unwind()
        .await;
    assert!(
        broken_disposal.is_ok(),
        "a caught panic payload destructor escaped reconciliation"
    );
    assert!(broken_disposal.unwrap().is_clean());
    let survivor_report =
        tokio::time::timeout(std::time::Duration::from_secs(1), survivor.dispose())
            .await
            .expect("the reconciliation scheduler wedged after containing a panic payload");
    assert!(survivor_report.is_clean());
}

#[tokio::test]
async fn dispose_and_shutdown_are_joinable_and_preserve_cleanup_reports() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(FailingCleanupFactory(FactorySpec::new(
                FactoryIdentity::builtin("cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let (first, second) = tokio::join!(fiber.dispose(), fiber.dispose());
    assert_eq!(first, second);
    assert_eq!(first.failures()[0].label, "forced failure");
    assert!(matches!(fiber.snapshot().state, FiberState::Disposed));

    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(FailingCleanupFactory(FactorySpec::new(
                FactoryIdentity::builtin("shutdown-cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
    assert_eq!(first, second);
    assert_eq!(first.report().failures().len(), 1);
    assert!(matches!(
        runtime
            .root()
            .apply(
                Arc::new(PassiveFactory(FactorySpec::new(FactoryIdentity::builtin(
                    "late", "1"
                )))),
                Value::Null,
            )
            .await,
        Err(MetaError::RuntimeShuttingDown)
    ));
}
