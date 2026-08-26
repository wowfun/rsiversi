use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, Context, ContractVersion, FactoryIdentity,
    FiberGeneration, FiberHandle, FiberSnapshot, FiberState, InvocationContext, Message, MetaError,
    PluginFactory, PreparedActivation, ProviderChannel, Requirement, Result, Runtime,
    RuntimeLimits, ServiceEndpoint, SupplyHandle, TopologyLimits,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Barrier;

const SERVICE: &str = "race-service";
const CONTRACT: &str = "test.dynamic-race";
const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct Echo;

#[async_trait]
impl ServiceEndpoint for Echo {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CapturingFactory {
    identity: FactoryIdentity,
    context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for CapturingFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[derive(Debug)]
struct InitialProviderFactory {
    identity: FactoryIdentity,
    context: Arc<Mutex<Option<Context>>>,
    supply: Arc<Mutex<Option<SupplyHandle>>>,
}

#[async_trait]
impl PluginFactory for InitialProviderFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let supply = context.provide(SERVICE, CONTRACT, V1, Arc::new(Echo))?;
        *self.context.lock().expect("context capture poisoned") = Some(context);
        *self.supply.lock().expect("supply capture poisoned") = Some(supply);
        Ok(())
    }
}

#[derive(Debug)]
struct CountingConsumerFactory {
    identity: FactoryIdentity,
    activations: Arc<AtomicUsize>,
    services: Arc<Mutex<Vec<Capability>>>,
}

#[async_trait]
impl PluginFactory for CountingConsumerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone())
            .requiring(Requirement::new(SERVICE, CONTRACT, V1)))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let service = plan
            .inject(SERVICE)
            .expect("resolved activation plan contains the required service")
            .clone();
        self.services
            .lock()
            .expect("service capture poisoned")
            .push(service);
        self.activations.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

async fn wait_pending(handle: &FiberHandle) -> FiberSnapshot {
    let mut snapshots = handle.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = snapshots.borrow().clone();
            if matches!(snapshot.state, FiberState::Pending(_)) {
                return snapshot;
            }
            snapshots
                .changed()
                .await
                .expect("live Fiber retains its snapshot sender");
        }
    })
    .await
    .expect("Fiber did not converge to Pending")
}

async fn wait_active_after(
    handle: &FiberHandle,
    previous: Option<FiberGeneration>,
) -> FiberSnapshot {
    let mut snapshots = handle.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = snapshots.borrow().clone();
            if matches!(snapshot.state, FiberState::Active)
                && previous.is_none_or(|generation| snapshot.generation != generation)
            {
                return snapshot;
            }
            snapshots
                .changed()
                .await
                .expect("live Fiber retains its snapshot sender");
        }
    })
    .await
    .expect("Fiber did not converge to a new Active generation")
}

async fn assert_echo(service: Capability, payload: &'static [u8]) {
    let response = service
        .invoke(Message::new(payload))
        .await
        .expect("echo call succeeds");
    assert_eq!(response.as_bytes(), payload);
}

async fn active_context(runtime: &Runtime, identity: &'static str) -> (FiberHandle, Context) {
    let captured = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(CapturingFactory {
                identity: FactoryIdentity::builtin(identity, "1"),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .expect("context-capturing Fiber applies");
    assert!(matches!(fiber.snapshot().state, FiberState::Active));
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captured its Context");
    (fiber, context)
}

#[tokio::test]
async fn active_reprovide_mints_a_new_supply_and_an_old_handle_cannot_remove_it() {
    let runtime = Runtime::default();
    let activations = Arc::new(AtomicUsize::new(0));
    let injected_services = Arc::new(Mutex::new(Vec::new()));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CountingConsumerFactory {
                identity: FactoryIdentity::builtin("race-consumer", "1"),
                activations: Arc::clone(&activations),
                services: Arc::clone(&injected_services),
            }),
            Value::Null,
        )
        .await
        .expect("consumer applies while its supply is missing");
    wait_pending(&consumer).await;
    assert_eq!(activations.load(Ordering::Acquire), 0);

    let captured_context = Arc::new(Mutex::new(None));
    let captured_supply = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            Arc::new(InitialProviderFactory {
                identity: FactoryIdentity::builtin("race-provider", "1"),
                context: Arc::clone(&captured_context),
                supply: Arc::clone(&captured_supply),
            }),
            Value::Null,
        )
        .await
        .expect("provider applies");
    let first_consumer_generation = wait_active_after(&consumer, None).await.generation;
    assert_eq!(activations.load(Ordering::Acquire), 1);

    let context = captured_context
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("provider activation captured its Context");
    let first_supply = captured_supply
        .lock()
        .expect("supply capture poisoned")
        .clone()
        .expect("provider activation captured its supply");
    assert_eq!(first_supply.id().owner(), context.owner().unwrap());
    let first_service = injected_services.lock().expect("service capture poisoned")[0].clone();
    assert_echo(first_service.clone(), b"first").await;

    assert!(first_supply.dispose().await.is_clean());
    wait_pending(&consumer).await;
    assert!(first_service.open().is_err());
    drop(first_service);
    assert_eq!(runtime.resource_snapshot().services.current, 0);

    let second_supply = context
        .provide(SERVICE, CONTRACT, V1, Arc::new(Echo))
        .expect("the Active provider can reoccupy its released slot");
    assert_ne!(first_supply.id(), second_supply.id());
    assert_eq!(first_supply.id().owner(), second_supply.id().owner());
    let second_consumer = wait_active_after(&consumer, Some(first_consumer_generation)).await;
    assert_eq!(activations.load(Ordering::Acquire), 2);
    assert_eq!(
        injected_services
            .lock()
            .expect("service capture poisoned")
            .len(),
        2
    );

    assert!(first_supply.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 1);
    assert_eq!(consumer.snapshot(), second_consumer);
    let second_service = injected_services.lock().expect("service capture poisoned")[1].clone();
    assert_echo(second_service, b"second").await;

    assert!(second_supply.dispose().await.is_clean());
    wait_pending(&consumer).await;
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert!(provider.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    injected_services
        .lock()
        .expect("service capture poisoned")
        .clear();
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_generation_provide_has_exactly_one_winner() {
    let runtime = Runtime::default();
    let (provider, context) = active_context(&runtime, "concurrent-provider").await;
    let barrier = Arc::new(Barrier::new(2));

    let attempt = |context: Context, barrier: Arc<Barrier>| {
        tokio::spawn(async move {
            barrier.wait().await;
            context.provide(SERVICE, CONTRACT, V1, Arc::new(Echo))
        })
    };
    let left = attempt(context.clone(), Arc::clone(&barrier));
    let right = attempt(context.clone(), Arc::clone(&barrier));
    let (left, right) = tokio::join!(left, right);
    let left = left.expect("left provide task joins");
    let right = right.expect("right provide task joins");

    let (winner, loser) = match (left, right) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        (Ok(_), Ok(_)) => panic!("both concurrent provides occupied one slot"),
        (Err(left), Err(right)) => panic!("both concurrent provides failed: {left}; {right}"),
    };
    assert!(matches!(
        loser,
        MetaError::DuplicateProvider { ref service } if service.as_str() == SERVICE
    ));
    assert_eq!(winner.id().owner(), context.owner().unwrap());
    let services = runtime.resource_snapshot().services;
    assert_eq!(services.current, 1);
    assert_eq!(services.high_watermark, 1);

    assert!(winner.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn saturated_service_capacity_counts_rejection_and_is_reusable_after_dispose() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_services: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .expect("one service is a valid Runtime capacity");
    let (provider, context) = active_context(&runtime, "capacity-provider").await;

    let first = context
        .provide("first", CONTRACT, V1, Arc::new(Echo))
        .expect("first supply consumes the only service slot");
    let saturated = runtime.resource_snapshot().services;
    assert_eq!(saturated.current, 1);
    assert_eq!(saturated.high_watermark, 1);
    assert_eq!(saturated.rejected, 0);

    assert!(matches!(
        context.provide("replacement", CONTRACT, V1, Arc::new(Echo)),
        Err(MetaError::CapacityExhausted {
            resource: "services"
        })
    ));
    let rejected = runtime.resource_snapshot().services;
    assert_eq!(rejected.current, 1);
    assert_eq!(rejected.high_watermark, 1);
    assert_eq!(rejected.rejected, 1);

    assert!(first.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    let replacement = context
        .provide("replacement", CONTRACT, V1, Arc::new(Echo))
        .expect("disposed supply releases capacity for reuse");
    assert_ne!(first.id(), replacement.id());
    let reused = runtime.resource_snapshot().services;
    assert_eq!(reused.current, 1);
    assert_eq!(reused.high_watermark, 1);
    assert_eq!(reused.rejected, 1);

    assert!(replacement.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
