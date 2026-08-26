use super::*;

#[derive(Debug)]
struct BlockingActivationFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    cleaned: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingActivationFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context();
        context.on(
            "cancelled-activation",
            Arc::new(NoopHandler),
            EventOptions::default(),
        )?;
        let cleaned = Arc::clone(&self.cleaned);
        context.defer(
            "cancelled activation",
            Box::new(move || {
                async move {
                    cleaned.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancelling_apply_rolls_back_the_runtime_owned_activation() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let cleaned = Arc::new(AtomicUsize::new(0));
    let root = runtime.root();
    let apply = tokio::spawn({
        let entered = Arc::clone(&entered);
        let cleaned = Arc::clone(&cleaned);
        async move {
            root.apply(
                Arc::new(BlockingActivationFactory {
                    spec: FactorySpec::new(FactoryIdentity::builtin("cancelled-apply", "1")),
                    entered,
                    cleaned,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    apply.abort();
    let _ = apply.await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().fibers.is_empty() && cleaned.load(Ordering::Acquire) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled apply did not roll back");

    let replacement = runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("replacement-listener", "1")),
                context: Arc::new(Mutex::new(None)),
                listener: Arc::new(Mutex::new(None)),
                dispose_during_activation: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(replacement.snapshot().state, FiberState::Active));
}

#[tokio::test]
async fn captured_context_cannot_cross_a_reconfiguration_generation() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("context-generation", "1")),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let old = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captured its context");
    fiber.reconfigure(Value::Null).await.unwrap();

    assert!(matches!(
        old.service("undeclared"),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(matches!(
        old.on("stale", Arc::new(NoopHandler), EventOptions::default()),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(matches!(
        old.defer("stale", Box::new(|| async move { Ok(()) }.boxed())),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(matches!(
        old.dispatch("stale", DispatchMode::Emit, Value::Null).await,
        Err(MetaError::StaleContext { .. })
    ));
}

#[derive(Debug)]
struct PanicCleanupSerializationFactory {
    spec: FactorySpec,
    cleanup_entered: Arc<Notify>,
    cleanup_release: Arc<Notify>,
    panic_once: AtomicBool,
}

#[async_trait]
impl PluginFactory for PanicCleanupSerializationFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context();
        let config = plan.config();
        if config.as_ref() == &Value::from(1) && !self.panic_once.swap(true, Ordering::AcqRel) {
            let entered = Arc::clone(&self.cleanup_entered);
            let release = Arc::clone(&self.cleanup_release);
            context.defer(
                "panic cleanup serialization",
                Box::new(move || {
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    }
                    .boxed()
                }),
            )?;
            panic!("serialized panic cleanup evidence");
        }
        Ok(())
    }
}

#[tokio::test]
async fn activation_panic_cleanup_remains_inside_the_transition_transaction() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("panic-dependency", "1"),
                "panic-dependency",
                "test.dependency",
                V1,
                Arc::new(Echo),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let consumer = runtime
        .root()
        .apply(
            Arc::new(PanicCleanupSerializationFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("panic-cleanup-consumer", "1"))
                    .requiring(Requirement::new("panic-dependency", "test.dependency", V1)),
                cleanup_entered: Arc::clone(&cleanup_entered),
                cleanup_release: Arc::clone(&cleanup_release),
                panic_once: AtomicBool::new(false),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));

    let reconfiguration = tokio::spawn({
        let consumer = consumer.clone();
        async move { consumer.reconfigure(Value::from(1)).await }
    });
    cleanup_entered.notified().await;
    let mut provider_disposal = tokio::spawn(async move { provider.dispose().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut provider_disposal)
            .await
            .is_err(),
        "dependency reconciliation overlapped panic cleanup"
    );
    cleanup_release.notify_one();
    assert!(provider_disposal.await.unwrap().is_clean());
    let snapshot = reconfiguration.await.unwrap().unwrap();
    assert!(
        matches!(snapshot.state, FiberState::Failed(_)),
        "reconfiguration returned {snapshot:?}"
    );
}
