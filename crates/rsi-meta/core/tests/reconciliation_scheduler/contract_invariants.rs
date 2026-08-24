use super::*;

#[derive(Debug)]
struct BlockingDeclaredProviderFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Debug)]
struct ReconciliationProbeFactory {
    descriptor: PluginDescriptor,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
    activated: Arc<Notify>,
}

#[derive(Debug)]
struct BlockingConsumerFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for BlockingConsumerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for ReconciliationProbeFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("slow-pending", "1"))
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("fast-pending", "1"))
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
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "reconciliation-provider",
                    "1",
                ))
                .providing(Provision::new("reconcile", "test.reconcile", V1)),
                endpoint: Arc::new(Echo),
            }),
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
impl PluginFactory for BlockingDeclaredProviderFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.provide("cycle-a", "test.cycle-a", V1, Arc::new(Echo))?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn cycle_diagnostics_follow_loading_fibers_actual_bindings() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "cycle-shared-provider",
                    "1",
                ))
                .providing(Provision::new(
                    "cycle-shared",
                    "test.cycle-shared",
                    V1,
                )),
                endpoint: Arc::new(Echo),
            }),
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
                Arc::new(BlockingDeclaredProviderFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "cycle-loading-provider",
                        "1",
                    ))
                    .requiring(Requirement::new("cycle-shared", "test.cycle-shared", V1))
                    .providing(Provision::new("cycle-a", "test.cycle-a", V1)),
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
                PluginDescriptor::new(FactoryIdentity::builtin("cycle-pending", "1"))
                    .requiring(Requirement::new("cycle-a", "test.cycle-a", V1))
                    .providing(Provision::new("cycle-shared", "test.cycle-shared", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let reported_cycle = matches!(
        pending.snapshot().state,
        FiberState::Pending(ref report)
            if report.reasons.iter().any(|reason| matches!(reason, rsi_meta::PendingReason::DependencyCycle { .. }))
    );
    release.notify_one();
    loading.await.unwrap().unwrap();

    assert!(
        !reported_cycle,
        "a loading fiber's unused declared provider became a false cycle edge"
    );
}

#[tokio::test]
async fn pending_declaration_does_not_cancel_a_consumer_of_a_published_provider() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "published-provider",
                    "1",
                ))
                .providing(Provision::new("shared", "test.shared", V1)),
                endpoint: Arc::new(Echo),
            }),
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
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "loading-consumer",
                        "1",
                    ))
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

    let pending_declaration = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("pending-declaration", "1"))
                    .requiring(Requirement::new("missing", "test.missing", V1))
                    .providing(Provision::new("shared", "test.shared", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        pending_declaration.snapshot().state,
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
        "a declaration that never published cancelled a valid activation"
    );

    assert!(pending_declaration.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
