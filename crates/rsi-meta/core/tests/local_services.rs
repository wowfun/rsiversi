use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, FiberState, LocalContract, PluginFactory, PreparedActivation, Result, Runtime,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[path = "support/resolver.rs"]
mod resolver;
use resolver::resolved;

#[derive(Debug)]
struct Counter(u64);

struct CounterContract;

impl LocalContract for CounterContract {
    const KEY: &'static str = "test.counter";
    type Service = Counter;
}

#[derive(Debug)]
struct Consumer {
    observed: Arc<Mutex<Option<Arc<Counter>>>>,
}

#[async_trait]
impl PluginFactory for Consumer {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<CounterContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.observed.lock().expect("observation poisoned") =
            Some(plan.local::<CounterContract>()?);
        Ok(())
    }
}

#[derive(Debug)]
struct Provider {
    service: Arc<Counter>,
}

#[derive(Debug)]
struct ShutdownBlockingProvider {
    service: Arc<Counter>,
    cleanup_entered: Arc<Notify>,
    cleanup_release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for ShutdownBlockingProvider {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .provide_local::<CounterContract>(Arc::clone(&self.service))?;
        let entered = Arc::clone(&self.cleanup_entered);
        let release = Arc::clone(&self.cleanup_release);
        plan.defer(
            "hold Local supply during shutdown admission test",
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

#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .provide_local::<CounterContract>(Arc::clone(&self.service))?;
        Ok(())
    }
}

#[derive(Debug)]
struct OptionalObserver {
    activations: Arc<Mutex<Vec<Option<Arc<Counter>>>>>,
}

#[derive(Debug)]
struct DynamicOwner {
    context: Arc<Mutex<Option<rsi_meta::Context>>>,
}

#[async_trait]
impl PluginFactory for DynamicOwner {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[derive(Debug)]
struct RecordingConsumer {
    observed: Arc<Mutex<Vec<Arc<Counter>>>>,
}

#[async_trait]
impl PluginFactory for RecordingConsumer {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<CounterContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        self.observed
            .lock()
            .expect("observation poisoned")
            .push(plan.local::<CounterContract>()?);
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for OptionalObserver {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        self.activations
            .lock()
            .expect("activation log poisoned")
            .push(plan.context().lookup_local::<CounterContract>());
        Ok(())
    }
}

#[tokio::test]
async fn local_objects_keep_identity_and_only_hard_requirements_form_edges() {
    let runtime = Runtime::default();
    let observed = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(Consumer {
                observed: Arc::clone(&observed),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let optional_activations = Arc::new(Mutex::new(Vec::new()));
    let optional = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(OptionalObserver {
                activations: Arc::clone(&optional_activations),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(optional.snapshot().state, FiberState::Active);
    {
        let optional_log = optional_activations
            .lock()
            .expect("activation log poisoned");
        assert_eq!(optional_log.len(), 1);
        assert!(optional_log[0].is_none());
    }

    let service = Arc::new(Counter(41));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(Provider {
                service: Arc::clone(&service),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();

    let injected = observed
        .lock()
        .expect("observation poisoned")
        .clone()
        .expect("hard local requirement was injected");
    assert!(Arc::ptr_eq(&service, &injected));
    assert_eq!(injected.0, 41);
    assert!(Arc::ptr_eq(
        &service,
        &runtime
            .root()
            .lookup_local::<CounterContract>()
            .expect("active local service is visible")
    ));
    assert_eq!(
        optional_activations
            .lock()
            .expect("activation log poisoned")
            .len(),
        1,
        "optional lookup must not create a reconciliation edge"
    );

    assert!(provider.dispose().await.is_clean());
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert!(runtime.root().lookup_local::<CounterContract>().is_none());
    assert_eq!(injected.0, 41, "escaped Arc remains an ordinary Rust value");
    assert_eq!(
        optional_activations
            .lock()
            .expect("activation log poisoned")
            .len(),
        1
    );

    assert!(consumer.dispose().await.is_clean());
    assert!(optional.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn shutdown_withdraws_optional_local_lookup_before_plugin_cleanup() {
    let runtime = Runtime::default();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ShutdownBlockingProvider {
                service: Arc::new(Counter(1)),
                cleanup_entered: Arc::clone(&cleanup_entered),
                cleanup_release: Arc::clone(&cleanup_release),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(runtime.root().lookup_local::<CounterContract>().is_some());

    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    cleanup_entered.notified().await;
    assert!(runtime.snapshot().shutting_down);
    assert!(
        runtime.root().lookup_local::<CounterContract>().is_none(),
        "Local visibility must be withdrawn before plugin cleanup runs"
    );
    cleanup_release.notify_one();
    assert!(shutdown.await.unwrap().is_complete());
}

#[tokio::test]
async fn same_generation_reprovide_has_a_fresh_identity_and_replays_hard_consumers() {
    let runtime = Runtime::default();
    let owner_context = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(DynamicOwner {
                context: Arc::clone(&owner_context),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(owner.snapshot().state, FiberState::Active);

    let observed = Arc::new(Mutex::new(Vec::new()));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RecordingConsumer {
                observed: Arc::clone(&observed),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let context = owner_context
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("owner activated");
    let first_service = Arc::new(Counter(1));
    let first = context
        .provide_local::<CounterContract>(Arc::clone(&first_service))
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();
    let first_generation = consumer.snapshot().generation;

    assert!(first.dispose().await.is_clean());
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    let second_service = Arc::new(Counter(2));
    let second = context
        .provide_local::<CounterContract>(Arc::clone(&second_service))
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();

    assert_ne!(first.id(), second.id());
    assert_ne!(first_generation, consumer.snapshot().generation);
    {
        let log = observed.lock().expect("observation poisoned");
        assert_eq!(log.len(), 2);
        assert!(Arc::ptr_eq(&log[0], &first_service));
        assert!(Arc::ptr_eq(&log[1], &second_service));
    }

    assert!(first.dispose().await.is_clean());
    assert!(Arc::ptr_eq(
        &second_service,
        &context
            .lookup_local::<CounterContract>()
            .expect("old exact handle cannot withdraw a replacement")
    ));

    assert!(owner.dispose().await.is_clean());
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert!(consumer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn retired_generation_context_cannot_discover_an_active_local_service() {
    let runtime = Runtime::default();
    let service = Arc::new(Counter(7));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(Provider {
                service: Arc::clone(&service),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let captured = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(DynamicOwner {
                context: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let stale = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("owner activated");

    assert!(Arc::ptr_eq(
        &service,
        &stale.lookup_local::<CounterContract>().unwrap()
    ));
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.root().lookup_local::<CounterContract>().is_some());
    assert!(stale.lookup_local::<CounterContract>().is_none());

    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn local_isolation_is_nominal_and_context_scoped() {
    let runtime = Runtime::default();
    let public = runtime.root();
    let (private, isolation) = runtime
        .root()
        .isolate_local_fresh::<CounterContract>()
        .unwrap();
    assert_ne!(isolation, rsi_meta::LocalIsolationId(0));

    let service = Arc::new(Counter(9));
    let provider = private
        .clone()
        .apply(
            crate::resolved(Arc::new(Provider {
                service: Arc::clone(&service),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(provider.snapshot().state, FiberState::Active);
    assert!(public.lookup_local::<CounterContract>().is_none());
    assert!(Arc::ptr_eq(
        &service,
        &private.lookup_local::<CounterContract>().unwrap()
    ));

    let public_consumer = public
        .apply(
            crate::resolved(Arc::new(Consumer {
                observed: Arc::new(Mutex::new(None)),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        public_consumer.snapshot().state,
        FiberState::Pending(_)
    ));

    let private_observed = Arc::new(Mutex::new(None));
    let private_consumer = private
        .apply(
            crate::resolved(Arc::new(Consumer {
                observed: Arc::clone(&private_observed),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(private_consumer.snapshot().state, FiberState::Active);
    assert!(Arc::ptr_eq(
        &service,
        private_observed
            .lock()
            .expect("observation poisoned")
            .as_ref()
            .unwrap()
    ));

    assert!(provider.dispose().await.is_clean());
    assert!(private_consumer.dispose().await.is_clean());
    assert!(public_consumer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
