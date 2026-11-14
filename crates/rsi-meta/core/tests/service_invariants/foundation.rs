use super::*;

#[tokio::test]
async fn failed_activation_retries_only_when_its_dependency_binding_changes() {
    #[derive(Debug, Default)]
    struct FailFirst(AtomicUsize);
    #[async_trait]
    impl PluginFactory for FailFirst {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(
                PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                    "echo",
                    "test.echo",
                    V1,
                )),
            )
        }
        async fn activate(&self, _: ActivationPlan) -> Result<()> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(MetaError::Service("first binding fails".into()));
            }
            Ok(())
        }
    }
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    let factory = Arc::new(FailFirst::default());
    let consumer = runtime
        .root()
        .apply(crate::resolved(factory.clone()), Value::Null)
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Failed(_)));
    let unrelated = runtime
        .root()
        .isolate("echo", IsolationId(99))
        .unwrap()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(Arc::new(Mutex::new(
                Vec::new(),
            ))))),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(factory.0.load(Ordering::SeqCst), 1);
    provider.reconfigure(Value::Bool(true)).await.unwrap();
    wait_active(&consumer).await;
    assert_eq!(factory.0.load(Ordering::SeqCst), 2);
    assert!(consumer.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(unrelated.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct CleanupHolder {
    captured: Mutex<Option<(rsi_meta::Context, Capability)>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for CleanupHolder {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "echo",
                "test.echo",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let service = plan.inject("echo").unwrap().clone();
        *self.captured.lock().unwrap() = Some((plan.context().clone(), service));
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        plan.defer(
            "wait for holder probe",
            Box::new(move || {
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(())
                })
            }),
        )
    }
}

async fn existing_holder_window(dynamic_withdrawal: bool, shutdown: bool) {
    let runtime = Runtime::default();
    let captured = Arc::new(CleanupHolder::default());
    let context = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(support::ContextCaptureFactory {
                spec: support::FactorySpec::new(FactoryIdentity::linked("holder-provider", "1")),
                context: Arc::clone(&context),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let context = context.lock().unwrap().take().unwrap();
    let supply = context
        .provide("echo", "test.echo", V1, Arc::new(support::Echo))
        .unwrap();
    let consumer = runtime
        .root()
        .apply(crate::resolved(captured.clone()), Value::Null)
        .await
        .unwrap();
    let retirement = tokio::spawn({
        let provider = provider.clone();
        async move {
            if dynamic_withdrawal {
                supply.dispose().await
            } else {
                provider.dispose().await
            }
        }
    });
    captured.entered.notified().await;
    let (old_context, service) = captured.captured.lock().unwrap().take().unwrap();
    // This call is outside the cleanup callback: possession, not callback identity,
    // grants the documented window while the provider joins exact dependents.
    assert_eq!(
        service
            .invoke(Message::new(b"during retirement".to_vec()))
            .await
            .unwrap()
            .as_bytes(),
        b"during retirement"
    );
    assert!(matches!(
        old_context.defer("too late", Box::new(|| Box::pin(async { Ok(()) }))),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(!retirement.is_finished());
    let shutdown_task = if shutdown {
        let mut call = service.open().unwrap();
        let task = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.shutdown().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !runtime.snapshot().shutting_down {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            service.open(),
            Err(MetaError::RuntimeShuttingDown)
        ));
        assert!(matches!(
            service
                .invoke(Message::new(b"after shutdown".to_vec()))
                .await,
            Err(MetaError::RuntimeShuttingDown)
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), call.recv())
                .await
                .expect("admitted call must terminate")
                .is_err(),
            "shutdown cancels the already-admitted call"
        );
        drop(call);
        Some(task)
    } else {
        None
    };
    captured.release.notify_one();
    assert!(retirement.await.unwrap().is_clean());
    assert!(matches!(
        service.open(),
        Err(MetaError::StaleCapability | MetaError::RuntimeShuttingDown)
    ));
    drop(service);
    if let Some(task) = shutdown_task {
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .expect("released holders must let shutdown finish")
                .unwrap()
                .is_clean()
        );
    }
    assert!(consumer.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn provider_retirement_keeps_existing_holders_callable_until_dependents_finish() {
    existing_holder_window(false, false).await;
}

#[tokio::test]
async fn supply_withdrawal_keeps_existing_holders_callable_until_dependents_finish() {
    existing_holder_window(true, false).await;
}

#[tokio::test]
async fn missing_dependency_converges_and_provider_retires_after_consumer_cleanup() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let consumer_factory = Arc::new(ConsumerFactory::new(Arc::clone(&cleanup)));
    let consumer = runtime
        .root()
        .apply(crate::resolved(consumer_factory.clone()), Value::Null)
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let provider_factory = Arc::new(ProviderFactory::new(Arc::clone(&cleanup)));
    let provider = runtime
        .root()
        .apply(crate::resolved(provider_factory.clone()), Value::Null)
        .await
        .unwrap();
    wait_active(&provider).await;
    wait_active(&consumer).await;
    assert_eq!(
        consumer_factory
            .observed
            .lock()
            .expect("observation log poisoned")
            .as_slice(),
        &[b"active".to_vec()]
    );
    let report = provider.dispose().await;
    assert!(report.is_clean(), "{report:?}");
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert_eq!(
        cleanup.lock().expect("cleanup log poisoned").as_slice(),
        &["consumer", "provider"]
    );
}

#[tokio::test]
async fn captured_service_handle_is_fenced_after_its_provider_retires() {
    #[derive(Debug)]
    struct CaptureFactory {
        _identity: FactoryIdentity,
        handle: Arc<Mutex<Option<rsi_meta::Capability>>>,
    }

    #[async_trait]
    impl PluginFactory for CaptureFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(
                PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                    "echo",
                    "test.echo",
                    V1,
                )),
            )
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            *self.handle.lock().expect("handle poisoned") = Some(
                plan.inject("echo")
                    .expect("prepared echo requirement must be injected")
                    .clone(),
            );
            Ok(())
        }
    }

    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&provider).await;
    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureFactory {
                _identity: FactoryIdentity::linked("capture", "1"),
                handle: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let old = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("service captured");
    provider.dispose().await;
    assert_eq!(old.open().unwrap_err(), MetaError::StaleCapability);
}

#[tokio::test]
async fn captured_service_handle_is_generation_fenced_after_consumer_reloads() {
    #[derive(Debug)]
    struct CaptureFactory {
        _identity: FactoryIdentity,
        handle: Arc<Mutex<Option<rsi_meta::Capability>>>,
    }

    #[async_trait]
    impl PluginFactory for CaptureFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(
                PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                    "echo",
                    "test.echo",
                    V1,
                )),
            )
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            *self.handle.lock().expect("handle poisoned") = Some(
                plan.inject("echo")
                    .expect("prepared echo requirement must be injected")
                    .clone(),
            );
            Ok(())
        }
    }

    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(Arc::new(Mutex::new(
                Vec::new(),
            ))))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&provider).await;
    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureFactory {
                _identity: FactoryIdentity::linked("capture-reload", "1"),
                handle: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let old = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("service captured");
    let old_generation = consumer.snapshot().generation;

    let reloaded = consumer.reconfigure(Value::Null).await.unwrap();
    assert_ne!(reloaded.generation, old_generation);
    assert_eq!(old.open().unwrap_err(), MetaError::StaleCapability);
    let current = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("replacement service captured");
    assert_eq!(
        current
            .invoke(Message::new(b"current".to_vec()))
            .await
            .unwrap()
            .as_bytes(),
        b"current"
    );
}

#[tokio::test]
async fn service_isolation_allows_private_provider_slots() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let first = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(Arc::clone(&cleanup)))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&first).await;
    let duplicate = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(Arc::clone(&cleanup)))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(duplicate.snapshot().state, FiberState::Failed(_)));

    let (private, _) = runtime.root().isolate_fresh("echo").unwrap();
    let isolated = private
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&isolated).await;
}

#[tokio::test]
async fn cloned_context_isolation_branches_remain_independent() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let base = runtime.root();
    let left = base.clone().isolate("echo", IsolationId(41)).unwrap();
    let right = base.isolate("echo", IsolationId(42)).unwrap();

    let left_provider = left
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(Arc::clone(&cleanup)))),
            Value::Null,
        )
        .await
        .unwrap();
    let right_provider = right
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(cleanup))),
            Value::Null,
        )
        .await
        .unwrap();

    wait_active(&left_provider).await;
    wait_active(&right_provider).await;
}

#[tokio::test]
async fn shutdown_fences_existing_holder_window_and_cancels_admitted_calls() {
    existing_holder_window(false, true).await;
}
