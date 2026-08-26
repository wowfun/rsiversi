use super::*;

#[derive(Debug)]
struct BlockingLoadingProviderFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Debug)]
struct ReconciliationProbeFactory {
    spec: FactorySpec,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
    activated: Arc<Notify>,
}

#[derive(Debug)]
struct BlockingConsumerFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for BlockingConsumerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for ReconciliationProbeFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        if let Some(entered) = &self.entered {
            entered.notify_one();
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        self.activated.notify_one();
        Ok(())
    }
}

#[tokio::test]
async fn one_slow_pending_fiber_does_not_block_independent_reconciliation() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 2,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let slow_activated = Arc::new(Notify::new());
    let fast_activated = Arc::new(Notify::new());
    let requirement = || Requirement::new("reconcile", "test.reconcile", V1);
    runtime
        .root()
        .apply(
            Arc::new(ReconciliationProbeFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("slow-pending", "1"))
                    .requiring(requirement()),
                entered: Some(Arc::clone(&entered)),
                release: Some(Arc::clone(&release)),
                activated: Arc::clone(&slow_activated),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(ReconciliationProbeFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("fast-pending", "1"))
                    .requiring(requirement()),
                entered: None,
                release: None,
                activated: Arc::clone(&fast_activated),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("reconciliation-provider", "1"),
                "reconcile",
                "test.reconcile",
                V1,
                Arc::new(Echo),
            )),
            Value::Null,
        )
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("slow reconciliation did not start");
    let fast_result =
        tokio::time::timeout(std::time::Duration::from_secs(5), fast_activated.notified()).await;
    release.notify_one();
    assert!(
        fast_result.is_ok(),
        "an independent Fiber waited behind a slow reconciliation"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), slow_activated.notified())
        .await
        .expect("slow reconciliation did not finish after release");
}

#[async_trait]
impl PluginFactory for BlockingLoadingProviderFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context.provide("cycle-a", "test.cycle-a", V1, Arc::new(Echo))?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn loading_supply_is_not_an_external_binding_or_a_fabricated_cycle_edge() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("cycle-shared-provider", "1"),
                "cycle-shared",
                "test.cycle-shared",
                V1,
                Arc::new(Echo),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let loading = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(BlockingLoadingProviderFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin("cycle-loading-provider", "1"))
                        .requiring(Requirement::new("cycle-shared", "test.cycle-shared", V1)),
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    let pending = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                FactorySpec::new(FactoryIdentity::builtin("cycle-pending", "1"))
                    .requiring(Requirement::new("cycle-a", "test.cycle-a", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        pending.snapshot().state,
        FiberState::Pending(ref report)
            if matches!(report.reasons.as_slice(), [rsi_meta::PendingReason::MissingService { service, .. }] if service.as_ref() == "cycle-a")
    ));
    release.notify_one();
    let loading = loading.await.unwrap().unwrap();
    support::wait_active(&pending).await;
    assert!(pending.dispose().await.is_clean());
    assert!(loading.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn unrelated_pending_fiber_does_not_cancel_a_consumer_of_a_published_provider() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("published-provider", "1"),
                "shared",
                "test.shared",
                V1,
                Arc::new(Echo),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let consumer = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(BlockingConsumerFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin("loading-consumer", "1"))
                        .requiring(Requirement::new("shared", "test.shared", V1)),
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;

    let unrelated_pending = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                FactorySpec::new(FactoryIdentity::builtin("unrelated-pending", "1"))
                    .requiring(Requirement::new("missing", "test.missing", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        unrelated_pending.snapshot().state,
        FiberState::Pending(_)
    ));

    release.notify_one();
    let consumer = tokio::time::timeout(std::time::Duration::from_secs(2), consumer)
        .await
        .expect("consumer activation did not settle")
        .unwrap()
        .unwrap();
    assert!(
        matches!(consumer.snapshot().state, FiberState::Active),
        "an unrelated missing requirement cancelled a valid activation"
    );

    assert!(unrelated_pending.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
