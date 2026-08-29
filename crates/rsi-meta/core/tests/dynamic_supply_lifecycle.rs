use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, Context, ContractVersion, Emit, EmitEventHandler,
    ExecutionLimits, FactoryIdentity, FiberHandle, FiberState, InvocationContext, LocalEvent,
    LocalEventOptions, Message, MetaError, PluginFactory, PreparedActivation, ProviderChannel,
    Requirement, Result, RuntimeLimits, ServiceEndpoint, SupplyHandle,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[path = "support/resolver.rs"]
mod resolver;
mod support;
use resolver::resolved;

use support::{Echo, PassiveFactory};

const V1: ContractVersion = ContractVersion(1);

struct WithdrawalFenceEvent;

impl LocalEvent for WithdrawalFenceEvent {
    const KEY: &'static str = "test.withdrawal-fence";
    type Value = ();
    type Error = std::convert::Infallible;
    type Mode = Emit;
}

#[derive(Debug)]
struct BlockingDropHandler {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Drop for BlockingDropHandler {
    fn drop(&mut self) {
        if let Some(entered) = self.entered.lock().expect("drop entry poisoned").take() {
            let _ = entered.send(());
        }
        tokio::task::block_in_place(|| {
            let _ = self.release.lock().expect("drop release poisoned").recv();
        });
    }
}

impl EmitEventHandler<WithdrawalFenceEvent> for BlockingDropHandler {
    fn handle(&self, (): &()) {}
}

#[derive(Debug)]
struct WithdrawalFenceProvider {
    handler: Mutex<Option<Arc<BlockingDropHandler>>>,
}

#[async_trait]
impl PluginFactory for WithdrawalFenceProvider {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().provide(
            "withdrawal-fence",
            "test.withdrawal-fence",
            V1,
            Arc::new(Echo),
        )?;
        let handler = self
            .handler
            .lock()
            .expect("handler holder poisoned")
            .take()
            .expect("the blocking handler is registered once");
        plan.context()
            .on_emit::<WithdrawalFenceEvent, _>(handler, LocalEventOptions::default())?;
        Ok(())
    }
}

#[derive(Debug)]
struct GatedWithdrawalConsumer {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for GatedWithdrawalConsumer {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "withdrawal-fence",
                "test.withdrawal-fence",
                V1,
            )),
        )
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_withdrawal_fences_loading_dependents_before_listener_destruction() {
    let runtime = rsi_meta::Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (drop_entered_sender, drop_entered_receiver) = tokio::sync::oneshot::channel();
    let (drop_release_sender, drop_release_receiver) = std::sync::mpsc::sync_channel(1);
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(WithdrawalFenceProvider {
                handler: Mutex::new(Some(Arc::new(BlockingDropHandler {
                    entered: Mutex::new(Some(drop_entered_sender)),
                    release: Mutex::new(drop_release_receiver),
                }))),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;

    let activation_entered = Arc::new(Notify::new());
    let activation_release = Arc::new(Notify::new());
    let mut application = tokio::spawn({
        let root = runtime.root();
        let activation_entered = Arc::clone(&activation_entered);
        let activation_release = Arc::clone(&activation_release);
        async move {
            root.apply(
                crate::resolved(Arc::new(GatedWithdrawalConsumer {
                    entered: activation_entered,
                    release: activation_release,
                })),
                Value::Null,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), activation_entered.notified())
        .await
        .expect("dependent activation did not enter");

    let provider_disposal = tokio::spawn({
        let provider = provider.clone();
        async move { provider.dispose().await }
    });
    tokio::time::timeout(Duration::from_secs(2), drop_entered_receiver)
        .await
        .expect("provider cleanup did not reach listener destruction")
        .expect("listener destruction entry sender dropped");

    activation_release.notify_one();
    let consumer = tokio::time::timeout(Duration::from_secs(2), &mut application)
        .await
        .expect("dependent activation did not settle across provider withdrawal")
        .expect("dependent application task remained healthy")
        .expect("dependent application remained valid");
    let state_before_cleanup_continued = consumer.snapshot().state;

    drop_release_sender
        .send(())
        .expect("provider listener destructor still waits for release");
    let report = tokio::time::timeout(Duration::from_secs(2), provider_disposal)
        .await
        .expect("provider disposal did not finish after listener release")
        .expect("provider disposal task remained healthy");
    assert!(report.is_clean());
    wait_pending(&consumer).await;
    assert!(consumer.dispose().await.is_clean());
    drop(provider);
    assert!(runtime.shutdown().await.is_complete());

    assert!(
        !matches!(state_before_cleanup_continued, FiberState::Active),
        "a Loading dependent published an Active generation after its exact provider was withdrawn"
    );
}

async fn wait_pending(handle: &FiberHandle) {
    let mut snapshots = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(snapshots.borrow().state, FiberState::Pending(_)) {
                return;
            }
            snapshots.changed().await.expect("Fiber watch stays open");
        }
    })
    .await
    .expect("Fiber did not converge to Pending");
}

#[derive(Debug)]
struct BlockingEndpoint {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for BlockingEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let Some(frame) = channel.recv().await else {
            return Ok(());
        };
        self.entered.notify_one();
        self.release.notified().await;
        channel.send(frame).await
    }
}

#[derive(Clone, Debug)]
struct CapturedSupplies {
    blocking_supply: SupplyHandle,
    blocking_service: Capability,
    independent_supply: SupplyHandle,
    independent_service: Capability,
}

#[derive(Debug)]
struct TwoSupplyFactory {
    _identity: FactoryIdentity,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    captured: Arc<Mutex<Option<CapturedSupplies>>>,
}

#[async_trait]
impl PluginFactory for TwoSupplyFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context();
        let blocking_supply = context.provide(
            "blocking",
            "test.blocking",
            V1,
            Arc::new(BlockingEndpoint {
                entered: Arc::clone(&self.entered),
                release: Arc::clone(&self.release),
            }),
        )?;
        let independent_supply =
            context.provide("independent", "test.independent", V1, Arc::new(Echo))?;
        let blocking_service = context.service("blocking")?;
        let independent_service = context.service("independent")?;
        *self.captured.lock().expect("supply capture poisoned") = Some(CapturedSupplies {
            blocking_supply,
            blocking_service,
            independent_supply,
            independent_service,
        });
        Ok(())
    }
}

#[tokio::test]
async fn capability_drain_yields_the_reconciliation_slot() {
    let runtime = rsi_meta::Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let captured = Arc::new(Mutex::new(None));
    let blocked = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(TwoSupplyFactory {
                _identity: FactoryIdentity::linked("capability-drain-blocker", "1"),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                captured: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let independent = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(PassiveFactory::new(FactoryIdentity::linked(
                "capability-drain-independent",
                "1",
            )))),
            Value::Null,
        )
        .await
        .unwrap();
    let supplies = captured
        .lock()
        .expect("supply capture poisoned")
        .take()
        .expect("provider captured its supplies");
    let call = tokio::spawn({
        let service = supplies.blocking_service.clone();
        async move { service.invoke(Message::new(b"held".as_slice())).await }
    });
    entered.notified().await;

    let blocked_disposal = tokio::spawn({
        let blocked = blocked.clone();
        async move { blocked.dispose().await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while supplies.blocking_service.open().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retirement did not fence the capability");

    let independent_report = tokio::time::timeout(Duration::from_secs(1), independent.dispose())
        .await
        .expect("capability drain retained the only reconciliation slot");
    assert!(independent_report.is_clean());

    release.notify_one();
    assert!(call.await.unwrap().is_ok());
    assert!(blocked_disposal.await.unwrap().is_clean());
    drop(supplies);
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn one_supply_withdrawal_drains_only_its_calls_and_survives_waiter_cancellation() {
    let runtime = rsi_meta::Runtime::default();
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(
                PassiveFactory::new(FactoryIdentity::linked("blocking-consumer", "1"))
                    .requiring(Requirement::new("blocking", "test.blocking", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let captured = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(TwoSupplyFactory {
                _identity: FactoryIdentity::linked("two-supply-provider", "1"),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                captured: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;
    support::wait_active(&consumer).await;
    let supplies = captured
        .lock()
        .expect("supply capture poisoned")
        .take()
        .expect("provider captured its supplies");

    let call = tokio::spawn({
        let service = supplies.blocking_service.clone();
        async move { service.invoke(Message::new(b"held".as_slice())).await }
    });
    entered.notified().await;

    let first_waiter = tokio::spawn({
        let supply = supplies.blocking_supply.clone();
        async move { supply.dispose().await }
    });
    wait_pending(&consumer).await;
    assert!(
        !first_waiter.is_finished(),
        "withdrawal must retain the admitted blocking call"
    );
    assert_eq!(runtime.resource_snapshot().services.current, 1);
    assert_eq!(
        supplies
            .independent_service
            .clone()
            .invoke(Message::new(b"independent".as_slice()))
            .await
            .unwrap()
            .as_bytes(),
        b"independent"
    );

    first_waiter.abort();
    assert!(first_waiter.await.unwrap_err().is_cancelled());
    release.notify_waiters();
    assert_eq!(call.await.unwrap().unwrap().as_bytes(), b"held");
    let report = tokio::time::timeout(Duration::from_secs(2), supplies.blocking_supply.dispose())
        .await
        .expect("Runtime-owned withdrawal driver did not survive waiter cancellation");
    assert!(report.is_clean());
    assert!(supplies.blocking_service.open().is_err());

    assert!(supplies.independent_supply.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert!(provider.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    drop(supplies);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct DropTrackedEndpoint(Arc<AtomicUsize>);

impl Drop for DropTrackedEndpoint {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl ServiceEndpoint for DropTrackedEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct BlockingSetupDisposalFactory {
    _identity: FactoryIdentity,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    supply: Arc<Mutex<Option<SupplyHandle>>>,
    endpoint_drops: Arc<AtomicUsize>,
    root_cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingSetupDisposalFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let root_cleanups = Arc::clone(&self.root_cleanups);
        plan.defer(
            "retained root cleanup",
            Box::new(move || {
                async move {
                    root_cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;
        let supply = plan.context().provide(
            "setup-disposal",
            "test.setup-disposal",
            V1,
            Arc::new(DropTrackedEndpoint(Arc::clone(&self.endpoint_drops))),
        )?;
        *self.supply.lock().expect("supply capture poisoned") = Some(supply);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn explicit_loading_supply_disposal_detaches_only_its_root_child_effect() {
    let runtime = rsi_meta::Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let supply = Arc::new(Mutex::new(None));
    let endpoint_drops = Arc::new(AtomicUsize::new(0));
    let root_cleanups = Arc::new(AtomicUsize::new(0));
    let application = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let supply = Arc::clone(&supply);
        let endpoint_drops = Arc::clone(&endpoint_drops);
        let root_cleanups = Arc::clone(&root_cleanups);
        async move {
            root.apply(
                crate::resolved(Arc::new(BlockingSetupDisposalFactory {
                    _identity: FactoryIdentity::linked("setup-disposal", "1"),
                    entered,
                    release,
                    supply,
                    endpoint_drops,
                    root_cleanups,
                })),
                Value::Null,
            )
            .await
        }
    });

    entered.notified().await;
    let supply = supply
        .lock()
        .expect("supply capture poisoned")
        .clone()
        .expect("activation captured its Loading supply");
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.effect_transactions.current, 1);
    assert_eq!(resources.effects.current, 2);
    assert_eq!(resources.services.current, 1);

    assert!(supply.dispose().await.is_clean());
    assert_eq!(endpoint_drops.load(Ordering::Acquire), 1);
    assert_eq!(root_cleanups.load(Ordering::Acquire), 0);
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.effect_transactions.current, 1);
    assert_eq!(resources.effects.current, 1);
    assert_eq!(resources.services.current, 0);

    release.notify_one();
    let fiber = application.await.unwrap().unwrap();
    support::wait_active(&fiber).await;
    assert!(fiber.dispose().await.is_clean());
    assert_eq!(root_cleanups.load(Ordering::Acquire), 1);
    assert_eq!(endpoint_drops.load(Ordering::Acquire), 1);
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.effect_transactions.current, 0);
    assert_eq!(resources.effects.current, 0);
    assert_eq!(resources.services.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct ContextCaptureFactory {
    _identity: FactoryIdentity,
    context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[tokio::test]
async fn withdrawn_endpoint_does_not_stay_alive_through_an_old_service_handle() {
    let runtime = rsi_meta::Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ContextCaptureFactory {
                _identity: FactoryIdentity::linked("endpoint-lifetime-owner", "1"),
                context: Arc::clone(&captured),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("provider captured its Context");
    let drops = Arc::new(AtomicUsize::new(0));
    let supply = context
        .provide(
            "tracked",
            "test.tracked",
            V1,
            Arc::new(DropTrackedEndpoint(Arc::clone(&drops))),
        )
        .unwrap();
    let stale_handle = context.service("tracked").unwrap();

    assert!(supply.dispose().await.is_clean());
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(stale_handle.open().is_err());
    drop(stale_handle);

    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct FailingSupplyFactory {
    _identity: FactoryIdentity,
    drops: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for FailingSupplyFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let _supply = plan.context().provide(
            "rollback",
            "test.rollback",
            V1,
            Arc::new(DropTrackedEndpoint(Arc::clone(&self.drops))),
        )?;
        Err(MetaError::Activation("requested failure".to_owned()))
    }
}

#[derive(Debug)]
struct ReplacementSupplyFactory;

#[async_trait]
impl PluginFactory for ReplacementSupplyFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let _supply = plan
            .context()
            .provide("rollback", "test.rollback", V1, Arc::new(Echo))?;
        Ok(())
    }
}

#[tokio::test]
async fn loading_failure_rolls_back_the_occupied_slot_reservation_and_endpoint() {
    let runtime = rsi_meta::Runtime::default();
    let drops = Arc::new(AtomicUsize::new(0));
    let failed = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(FailingSupplyFactory {
                _identity: FactoryIdentity::linked("failing-supply", "1"),
                drops: Arc::clone(&drops),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(failed.snapshot().state, FiberState::Failed(_)));
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(runtime.resource_snapshot().services.current, 0);

    let replacement = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "replacement-supply",
                "1",
                rsi_meta::UpdateMode::Replayable,
                Arc::new(ReplacementSupplyFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&replacement).await;
    assert_eq!(runtime.resource_snapshot().services.current, 1);

    assert!(replacement.dispose().await.is_clean());
    assert!(failed.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().services.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}
