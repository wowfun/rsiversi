use super::*;

#[tokio::test]
async fn missing_dependency_converges_and_provider_retires_after_consumer_cleanup() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let consumer_factory = Arc::new(ConsumerFactory::new(Arc::clone(&cleanup)));
    let consumer = runtime
        .root()
        .intercept("echo", json!({ "source": "direct-edge" }))
        .unwrap()
        .apply(consumer_factory.clone(), Value::Null)
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let provider_factory = Arc::new(ProviderFactory::new(Arc::clone(&cleanup)));
    let provider = runtime
        .root()
        .apply(provider_factory.clone(), Value::Null)
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
    assert_eq!(
        provider_factory
            .overlays
            .lock()
            .expect("overlay log poisoned")[0],
        vec![json!({ "source": "direct-edge" })]
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
        descriptor: PluginDescriptor,
        handle: Arc<Mutex<Option<rsi_meta::ServiceHandle>>>,
    }

    #[async_trait]
    impl PluginFactory for CaptureFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
            *self.handle.lock().expect("handle poisoned") = Some(context.service("echo")?);
            Ok(())
        }
    }

    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(Arc::new(ProviderFactory::new(cleanup)), Value::Null)
        .await
        .unwrap();
    wait_active(&provider).await;
    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("capture", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                handle: Arc::clone(&captured),
            }),
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
    let consumer_generation = consumer.snapshot().generation;
    provider.dispose().await;
    assert_eq!(
        old.open().unwrap_err(),
        MetaError::StaleContext {
            fiber: consumer.id(),
            generation: consumer_generation,
        }
    );
}

#[tokio::test]
async fn captured_service_handle_is_generation_fenced_after_consumer_reloads() {
    #[derive(Debug)]
    struct CaptureFactory {
        descriptor: PluginDescriptor,
        handle: Arc<Mutex<Option<rsi_meta::ServiceHandle>>>,
    }

    #[async_trait]
    impl PluginFactory for CaptureFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
            *self.handle.lock().expect("handle poisoned") = Some(context.service("echo")?);
            Ok(())
        }
    }

    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory::new(Arc::new(Mutex::new(Vec::new())))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&provider).await;
    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("capture-reload", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                handle: Arc::clone(&captured),
            }),
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
    assert_eq!(
        old.open().unwrap_err(),
        MetaError::StaleContext {
            fiber: consumer.id(),
            generation: old_generation,
        }
    );
    let current = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("replacement service captured");
    assert_eq!(
        current
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"current".to_vec()))
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
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&first).await;
    let duplicate = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(duplicate.snapshot().state, FiberState::Failed(_)));

    let (private, _) = runtime.root().isolate_fresh("echo").unwrap();
    let isolated = private
        .apply(Arc::new(ProviderFactory::new(cleanup)), Value::Null)
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
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    let right_provider = right
        .apply(Arc::new(ProviderFactory::new(cleanup)), Value::Null)
        .await
        .unwrap();

    wait_active(&left_provider).await;
    wait_active(&right_provider).await;
}
