use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, ContractVersion, FactoryIdentity, FiberHandle,
    FiberState, InvocationContext, IsolationId, Message, MetaError, PluginFactory,
    PreparedActivation, ProviderChannel, Requirement, Result, Runtime, RuntimeLimits,
    ServiceEndpoint, SupplyHandle, TopologyLimits,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[path = "support/resolver.rs"]
mod resolver;
mod support;
use resolver::resolved;

use support::{Echo, PassiveFactory};

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct BlockingDynamicProvider {
    _identity: FactoryIdentity,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    supply: Arc<Mutex<Option<SupplyHandle>>>,
    self_service: Arc<Mutex<Option<Capability>>>,
    self_response: Arc<Mutex<Option<Vec<u8>>>>,
}

#[async_trait]
impl PluginFactory for BlockingDynamicProvider {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context();
        let (supply, self_service) =
            context.provide_and_capture("dynamic", "test.dynamic", V1, Arc::new(Echo))?;
        let self_response = self_service
            .clone()
            .invoke(Message::new(b"self".as_slice()))
            .await?;
        *self.supply.lock().expect("supply capture poisoned") = Some(supply);
        *self.self_service.lock().expect("service capture poisoned") = Some(self_service);
        *self
            .self_response
            .lock()
            .expect("self response capture poisoned") = Some(self_response.as_bytes().to_vec());
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[derive(Debug)]
struct ImmediateDynamicProvider;

#[async_trait]
impl PluginFactory for ImmediateDynamicProvider {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let _supply = plan
            .context()
            .provide("dynamic", "test.dynamic", V1, Arc::new(Echo))?;
        Ok(())
    }
}

#[derive(Debug)]
struct LastOwnerDynamicProvider {
    _identity: FactoryIdentity,
    endpoint_drops: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for LastOwnerDynamicProvider {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let _supply = plan.context().provide(
            "last-owner",
            "test.last-owner",
            V1,
            Arc::new(DropEndpoint(Arc::clone(&self.endpoint_drops))),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct DropEndpoint(Arc<AtomicUsize>);

impl Drop for DropEndpoint {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[async_trait]
impl ServiceEndpoint for DropEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct DuplicateCaptureProvider {
    _identity: FactoryIdentity,
    rejected_endpoint_drops: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for DuplicateCaptureProvider {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context();
        let (_supply, _capability) =
            context.provide_and_capture("duplicate", "test.duplicate", V1, Arc::new(Echo))?;
        let resources_before = context.runtime().resource_snapshot();
        assert!(matches!(
            context.provide_and_capture(
                "duplicate",
                "test.duplicate",
                V1,
                Arc::new(DropEndpoint(Arc::clone(&self.rejected_endpoint_drops))),
            ),
            Err(MetaError::DuplicateProvider { .. })
        ));
        assert_eq!(self.rejected_endpoint_drops.load(Ordering::Relaxed), 1);
        let after_duplicate = context.runtime().resource_snapshot();
        assert_eq!(
            after_duplicate.effects.current,
            resources_before.effects.current
        );
        assert_eq!(
            after_duplicate.services.current,
            resources_before.services.current
        );
        assert_eq!(
            after_duplicate.capability_entries.current,
            resources_before.capability_entries.current
        );
        assert!(matches!(
            context.provide_and_capture(
                "capacity",
                "test.capacity",
                V1,
                Arc::new(DropEndpoint(Arc::clone(&self.rejected_endpoint_drops))),
            ),
            Err(MetaError::CapacityExhausted {
                resource: "services"
            })
        ));
        assert_eq!(self.rejected_endpoint_drops.load(Ordering::Relaxed), 2);
        let after_capacity = context.runtime().resource_snapshot();
        assert_eq!(
            after_capacity.effects.current,
            resources_before.effects.current
        );
        assert_eq!(
            after_capacity.services.current,
            resources_before.services.current
        );
        assert_eq!(
            after_capacity.capability_entries.current,
            resources_before.capability_entries.current
        );
        assert!(matches!(
            context.service("capacity"),
            Err(MetaError::ServiceUnavailable { .. })
        ));
        Ok(())
    }
}

#[derive(Debug)]
struct TaggedEndpoint(&'static [u8]);

#[async_trait]
impl ServiceEndpoint for TaggedEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        while channel.recv().await.is_some() {
            channel.send(Message::new(self.0)).await?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct IsolatedSupplies {
    left_supply: SupplyHandle,
    left_service: Capability,
    left_response: Vec<u8>,
    right_supply: SupplyHandle,
    right_service: Capability,
    right_response: Vec<u8>,
}

#[derive(Debug)]
struct IsolatedDynamicProvider {
    _identity: FactoryIdentity,
    captured: Arc<Mutex<Option<IsolatedSupplies>>>,
}

#[async_trait]
impl PluginFactory for IsolatedDynamicProvider {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let left = context.clone().isolate("dynamic", IsolationId(41))?;
        let right = context.isolate("dynamic", IsolationId(42))?;
        let left_supply = left.provide(
            "dynamic",
            "test.dynamic",
            V1,
            Arc::new(TaggedEndpoint(b"left")),
        )?;
        let right_supply = right.provide(
            "dynamic",
            "test.dynamic",
            V1,
            Arc::new(TaggedEndpoint(b"right")),
        )?;
        let left_service = left.service("dynamic")?;
        let right_service = right.service("dynamic")?;
        let left_response = left_service
            .clone()
            .invoke(Message::new(b"request".as_slice()))
            .await?
            .as_bytes()
            .to_vec();
        let right_response = right_service
            .clone()
            .invoke(Message::new(b"request".as_slice()))
            .await?
            .as_bytes()
            .to_vec();
        *self.captured.lock().expect("isolated supplies poisoned") = Some(IsolatedSupplies {
            left_supply,
            left_service,
            left_response,
            right_supply,
            right_service,
            right_response,
        });
        Ok(())
    }
}

async fn wait_pending(handle: &FiberHandle) {
    let mut snapshots = handle.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(snapshots.borrow().state, FiberState::Pending(_)) {
                return;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .expect("Fiber did not converge to Pending");
}

#[tokio::test]
async fn provide_and_capture_reserves_capability_before_supply_publication() -> Result<()> {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_capability_entries: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let context = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(support::ContextCaptureFactory {
                spec: support::FactorySpec::new(FactoryIdentity::linked("capture-owner", "1")),
                context: Arc::clone(&context),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let context = context
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captured its context");

    let existing = context.provide("existing", "test.existing", V1, Arc::new(Echo))?;
    let occupying_capability = context.service("existing")?;
    assert!(matches!(
        context.provide_and_capture("candidate", "test.candidate", V1, Arc::new(Echo)),
        Err(MetaError::CapacityExhausted {
            resource: "capability entries"
        })
    ));
    assert!(matches!(
        context.service("candidate"),
        Err(MetaError::ServiceUnavailable { .. })
    ));
    let snapshot = runtime.resource_snapshot();
    assert_eq!(snapshot.services.current, 1);
    assert_eq!(snapshot.capability_entries.current, 1);

    drop(occupying_capability);
    let (candidate, capability) =
        context.provide_and_capture("candidate", "test.candidate", V1, Arc::new(Echo))?;
    let response = capability
        .invoke(Message::new(b"captured".as_slice()))
        .await?;
    assert_eq!(response.as_bytes(), b"captured");
    assert_eq!(runtime.resource_snapshot().services.current, 2);

    drop((capability, candidate, existing));
    assert!(owner.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 0);
    Ok(())
}

#[tokio::test]
async fn rejected_loading_capture_detaches_its_unpublished_cleanup_and_endpoint() -> Result<()> {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_services: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })?;
    let rejected_endpoint_drops = Arc::new(AtomicUsize::new(0));
    let owner = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(DuplicateCaptureProvider {
                _identity: FactoryIdentity::linked("duplicate-capture", "1"),
                rejected_endpoint_drops: Arc::clone(&rejected_endpoint_drops),
            })),
            Value::Null,
        )
        .await?;
    assert!(matches!(owner.snapshot().state, FiberState::Active));
    assert_eq!(rejected_endpoint_drops.load(Ordering::Relaxed), 2);
    assert_eq!(runtime.resource_snapshot().services.current, 1);
    assert_eq!(runtime.resource_snapshot().effects.current, 1);
    assert!(owner.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
    Ok(())
}

#[tokio::test]
async fn dormant_supply_cleanup_does_not_retain_the_last_runtime_owner() -> Result<()> {
    let runtime = Runtime::default();
    let endpoint_drops = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(LastOwnerDynamicProvider {
        _identity: FactoryIdentity::linked("last-owner-dynamic-provider", "1"),
        endpoint_drops: Arc::clone(&endpoint_drops),
    });
    let factory_weak = Arc::downgrade(&factory);
    let owner = runtime
        .root()
        .apply(resolved(factory.clone()), Value::Null)
        .await?;
    support::wait_active(&owner).await;
    assert_eq!(runtime.resource_snapshot().services.current, 1);

    drop(factory);
    drop(runtime);
    drop(owner);

    assert!(
        factory_weak.upgrade().is_none(),
        "the Runtime-owned supply cleanup formed a last-owner cycle"
    );
    assert_eq!(endpoint_drops.load(Ordering::Relaxed), 1);
    Ok(())
}

#[tokio::test]
async fn loading_supply_occupies_the_slot_is_self_visible_and_publishes_only_when_active() {
    let runtime = rsi_meta::Runtime::default();
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(
                PassiveFactory::new(FactoryIdentity::linked("dynamic-consumer", "1"))
                    .requiring(Requirement::new("dynamic", "test.dynamic", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let supply = Arc::new(Mutex::new(None));
    let self_service = Arc::new(Mutex::new(None));
    let self_response = Arc::new(Mutex::new(None));
    let provider_apply = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let supply = Arc::clone(&supply);
        let self_service = Arc::clone(&self_service);
        let self_response = Arc::clone(&self_response);
        async move {
            root.apply(
                crate::resolved(Arc::new(BlockingDynamicProvider {
                    _identity: FactoryIdentity::linked("dynamic-provider", "1"),
                    entered,
                    release,
                    supply,
                    self_service,
                    self_response,
                })),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert_eq!(runtime.resource_snapshot().services.current, 1);
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 1);
    assert_eq!(runtime.resource_snapshot().effects.current, 1);
    assert_eq!(
        self_response
            .lock()
            .expect("self response capture poisoned")
            .as_deref(),
        Some(b"self".as_slice())
    );

    let contender = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "dynamic-contender",
                "1",
                rsi_meta::UpdateMode::Replayable,
                Arc::new(ImmediateDynamicProvider),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        contender.snapshot().state,
        FiberState::Failed(ref error) if error.contains("service slot already has a provider")
    ));

    release.notify_one();
    let provider = provider_apply.await.unwrap().unwrap();
    support::wait_active(&consumer).await;
    let captured_service = self_service
        .lock()
        .expect("service capture poisoned")
        .take()
        .expect("Loading provider captured its own service");

    let captured_supply = supply
        .lock()
        .expect("supply capture poisoned")
        .take()
        .expect("provider captured its supply handle");
    assert!(captured_supply.dispose().await.is_clean());
    wait_pending(&consumer).await;
    assert!(matches!(provider.snapshot().state, FiberState::Active));
    assert!(captured_service.open().is_err());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    drop(captured_service);
    drop(captured_supply);

    assert!(contender.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn one_generation_can_own_the_same_key_in_two_complete_isolation_slots() {
    let runtime = rsi_meta::Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(IsolatedDynamicProvider {
                _identity: FactoryIdentity::linked("isolated-dynamic-provider", "1"),
                captured: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;

    let isolated = captured
        .lock()
        .expect("isolated supplies poisoned")
        .take()
        .expect("activation captured both isolated supplies");
    assert_ne!(isolated.left_supply.id(), isolated.right_supply.id());
    assert_eq!(isolated.left_response, b"left");
    assert_eq!(isolated.right_response, b"right");
    assert_eq!(runtime.resource_snapshot().services.current, 2);

    assert!(isolated.left_supply.dispose().await.is_clean());
    assert!(isolated.left_service.open().is_err());
    assert_eq!(runtime.resource_snapshot().services.current, 1);
    assert_eq!(
        isolated
            .right_service
            .clone()
            .invoke(Message::new(b"still-right".as_slice()))
            .await
            .unwrap()
            .as_bytes(),
        b"right"
    );

    assert!(isolated.right_supply.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    drop(isolated);
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
