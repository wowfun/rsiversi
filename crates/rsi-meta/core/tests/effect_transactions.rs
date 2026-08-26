use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, Cleanup, ConfigValue, Context, ContractVersion, EffectHandle, FactoryIdentity,
    FiberState, InvocationContext, MetaError, PluginFactory, PreparedActivation, ProviderChannel,
    Result, Runtime, RuntimeLimits, ServiceEndpoint, TopologyLimits,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

mod support;

use support::{ContextCaptureFactory, FactorySpec};

const V1: ContractVersion = ContractVersion(1);

async fn wait_effect_resources_zero(runtime: &Runtime) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = runtime.resource_snapshot();
            if snapshot.effect_transactions.current == 0 && snapshot.effects.current == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("effect cleanup did not release its owned reservations");
}

async fn active_context(runtime: &Runtime, name: &str) -> (rsi_meta::FiberHandle, Context) {
    let captured = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin(name, "1")),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&fiber).await;
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captured its Context");
    (fiber, context)
}

fn record_cleanup(order: &Arc<Mutex<Vec<&'static str>>>, label: &'static str) -> Cleanup {
    let order = Arc::clone(order);
    Box::new(move || {
        async move {
            order.lock().expect("cleanup order poisoned").push(label);
            Ok(())
        }
        .boxed()
    })
}

fn expected_cleanup_panic() -> std::result::Result<(), String> {
    panic!("expected cleanup panic");
}

#[derive(Debug)]
struct ActivationRootWitness {
    identity: FactoryIdentity,
    observed_transactions: Arc<std::sync::atomic::AtomicUsize>,
    cleanups: Arc<std::sync::atomic::AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for ActivationRootWitness {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let cleanups = Arc::clone(&self.cleanups);
        plan.context().defer(
            "activation-root cleanup",
            Box::new(move || {
                async move {
                    cleanups.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;
        self.observed_transactions.store(
            plan.context()
                .runtime()
                .resource_snapshot()
                .effect_transactions
                .current,
            std::sync::atomic::Ordering::Release,
        );
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn runtime_installs_and_closes_an_activation_root_transaction_around_plugin_entry() {
    let runtime = Runtime::default();
    let observed_transactions = Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
    let cleanups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let applying = tokio::spawn({
        let root = runtime.root();
        let observed_transactions = Arc::clone(&observed_transactions);
        let cleanups = Arc::clone(&cleanups);
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(ActivationRootWitness {
                    identity: FactoryIdentity::builtin("activation-root-witness", "1"),
                    observed_transactions,
                    cleanups,
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });

    entered.notified().await;
    assert_eq!(
        observed_transactions.load(std::sync::atomic::Ordering::Acquire),
        1,
        "the Runtime must own the activation wrapper before plugin code runs"
    );
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 1);
    assert_eq!(runtime.resource_snapshot().effects.current, 1);

    release.notify_one();
    let fiber = applying.await.unwrap().unwrap();
    support::wait_active(&fiber).await;
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 1);
    assert_eq!(runtime.resource_snapshot().effects.current, 1);
    assert_eq!(cleanups.load(std::sync::atomic::Ordering::Acquire), 0);
    assert!(fiber.dispose().await.is_clean());
    assert_eq!(cleanups.load(std::sync::atomic::Ordering::Acquire), 1);
    wait_effect_resources_zero(&runtime).await;
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct OrderedEndpoint(Arc<Mutex<Vec<&'static str>>>);

impl Drop for OrderedEndpoint {
    fn drop(&mut self) {
        self.0
            .lock()
            .expect("cleanup order poisoned")
            .push("supply");
    }
}

#[async_trait]
impl ServiceEndpoint for OrderedEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct RootLifoFactory {
    identity: FactoryIdentity,
    order: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for RootLifoFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.defer("root first", record_cleanup(&self.order, "root-first"))?;
        let _supply = plan.context().provide(
            "root-lifo",
            "test.root-lifo",
            V1,
            Arc::new(OrderedEndpoint(Arc::clone(&self.order))),
        )?;
        plan.defer("root last", record_cleanup(&self.order, "root-last"))?;
        Ok(())
    }
}

#[tokio::test]
async fn activation_root_owns_setup_effects_and_dynamic_supply_in_one_lifo_transaction() {
    let runtime = Runtime::default();
    let order = Arc::new(Mutex::new(Vec::new()));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(RootLifoFactory {
                identity: FactoryIdentity::builtin("root-lifo", "1"),
                order: Arc::clone(&order),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&fiber).await;

    let resources = runtime.resource_snapshot();
    assert_eq!(resources.effect_transactions.current, 1);
    assert_eq!(resources.effects.current, 3);
    assert_eq!(resources.services.current, 1);
    assert!(order.lock().expect("cleanup order poisoned").is_empty());

    assert!(fiber.dispose().await.is_clean());
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["root-last", "supply", "root-first"]
    );
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.effect_transactions.current, 0);
    assert_eq!(resources.effects.current, 0);
    assert_eq!(resources.services.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct BlockingChildCleanup {
    identity: FactoryIdentity,
    order: Arc<Mutex<Vec<&'static str>>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for BlockingChildCleanup {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let order = Arc::clone(&self.order);
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        plan.defer(
            "blocking child cleanup",
            Box::new(move || {
                async move {
                    order
                        .lock()
                        .expect("cleanup order poisoned")
                        .push("child-entered");
                    entered.notify_one();
                    release.notified().await;
                    order
                        .lock()
                        .expect("cleanup order poisoned")
                        .push("child-done");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct PanickingParentWithChild {
    identity: FactoryIdentity,
    child: Arc<BlockingChildCleanup>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for PanickingParentWithChild {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.defer("parent root", record_cleanup(&self.order, "parent"))?;
        let _child = plan
            .context()
            .apply(self.child.clone(), Value::Null)
            .await?;
        panic!("expected activation panic");
    }
}

#[tokio::test]
async fn activation_panic_defers_the_root_claim_until_children_finish_rollback() {
    let runtime = Runtime::default();
    let order = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let application = tokio::spawn({
        let root = runtime.root();
        let order = Arc::clone(&order);
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(PanickingParentWithChild {
                    identity: FactoryIdentity::builtin("panicking-parent", "1"),
                    child: Arc::new(BlockingChildCleanup {
                        identity: FactoryIdentity::builtin("blocking-child", "1"),
                        order: Arc::clone(&order),
                        entered,
                        release,
                    }),
                    order,
                }),
                Value::Null,
            )
            .await
        }
    });

    entered.notified().await;
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["child-entered"],
        "the parent root must not autoabort ahead of its owned child"
    );
    assert!(!application.is_finished());

    release.notify_one();
    let fiber = application.await.unwrap().unwrap();
    assert!(matches!(
        fiber.snapshot().state,
        FiberState::Failed(ref error) if error.contains("plugin activation panicked")
    ));
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["child-entered", "child-done", "parent"]
    );
    wait_effect_resources_zero(&runtime).await;
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn retirement_claims_the_wrapper_before_setup_and_waits_for_its_undo() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "wrapper-first-effect").await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut transaction = context.begin_effect("install contribution").unwrap();

    let disposal = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.dispose().await }
    });
    let mut snapshots = fiber.subscribe();
    loop {
        if matches!(snapshots.borrow().state, FiberState::Unloading) {
            break;
        }
        snapshots.changed().await.unwrap();
    }
    assert!(!disposal.is_finished());
    assert!(order.lock().expect("cleanup order poisoned").is_empty());

    transaction
        .defer("remove first", record_cleanup(&order, "first"))
        .unwrap();
    transaction
        .defer("remove second", record_cleanup(&order, "second"))
        .unwrap();
    assert!(matches!(
        transaction.commit(),
        Err(MetaError::StaleContext { .. })
    ));

    let report = disposal.await.unwrap();
    assert!(report.is_clean());
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["second", "first"]
    );
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 0);
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn dropping_an_open_transaction_autoaborts_without_retiring_the_fiber() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "dropped-effect").await;
    let cleaned = Arc::new(Notify::new());
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut transaction = context.begin_effect("temporary contribution").unwrap();
    transaction
        .defer("remove temporary contribution", {
            let cleaned = Arc::clone(&cleaned);
            let count = Arc::clone(&count);
            Box::new(move || {
                async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    cleaned.notify_one();
                    Err("expected autoabort failure".to_owned())
                }
                .boxed()
            })
        })
        .unwrap();

    drop(transaction);
    cleaned.notified().await;
    wait_effect_resources_zero(&runtime).await;
    assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 1);
    assert!(matches!(fiber.snapshot().state, FiberState::Active));
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 0);
    assert_eq!(runtime.resource_snapshot().effects.current, 0);

    let retirement = fiber.dispose().await;
    assert_eq!(retirement.total_failures(), 1);
    assert_eq!(
        retirement.failures()[0].label,
        "remove temporary contribution"
    );
    assert_eq!(retirement.failures()[0].error, "expected autoabort failure");
    assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 1);
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn cancelling_a_handle_waiter_does_not_cancel_its_owned_cleanup_driver() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "cancelled-effect-waiter").await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut transaction = context.begin_effect("persistent cleanup driver").unwrap();
    transaction
        .defer("blocked cleanup", {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let calls = Arc::clone(&calls);
            Box::new(move || {
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    entered.notify_one();
                    release.notified().await;
                    Ok(())
                }
                .boxed()
            })
        })
        .unwrap();
    let handle = transaction.commit().unwrap();

    let cancelled = tokio::spawn({
        let handle = handle.clone();
        async move { handle.dispose().await }
    });
    entered.notified().await;
    cancelled.abort();
    let _ = cancelled.await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);

    release.notify_one();
    assert!(handle.dispose().await.is_clean());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn a_committed_effect_handle_disposes_once_and_all_callers_join_its_report() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "effect-handle").await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut transaction = context.begin_effect("committed contribution").unwrap();
    transaction
        .defer("remove committed contribution", {
            let calls = Arc::clone(&calls);
            Box::new(move || {
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    Err("expected cleanup failure".to_owned())
                }
                .boxed()
            })
        })
        .unwrap();
    let handle: EffectHandle = transaction.commit().unwrap();

    let second_handle = handle.clone();
    let (left, right) = tokio::join!(handle.dispose(), second_handle.dispose());
    for report in [&left, &right] {
        assert_eq!(report.total_failures(), 1);
        assert_eq!(report.failures()[0].label, "remove committed contribution");
        assert_eq!(report.failures()[0].error, "expected cleanup failure");
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 0);
    assert_eq!(runtime.resource_snapshot().effects.current, 0);

    let retirement = fiber.dispose().await;
    assert_eq!(retirement.total_failures(), 1);
    assert_eq!(
        retirement.failures()[0].label,
        "remove committed contribution"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn transaction_capacity_is_generation_and_runtime_bounded_then_reusable() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_effect_transactions_per_fiber: 1,
            maximum_effect_transactions: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (first_fiber, first_context) = active_context(&runtime, "transaction-quota-a").await;
    let (second_fiber, second_context) = active_context(&runtime, "transaction-quota-b").await;
    let first = first_context.begin_effect("held transaction").unwrap();

    assert!(matches!(
        first_context.begin_effect("same generation"),
        Err(MetaError::CapacityExhausted {
            resource: "effect transactions"
        })
    ));
    assert!(matches!(
        second_context.begin_effect("same runtime"),
        Err(MetaError::CapacityExhausted {
            resource: "effect transactions"
        })
    ));
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 1);
    assert_eq!(runtime.resource_snapshot().effect_transactions.rejected, 2);

    assert!(first.abort().await.is_clean());
    let replacement = second_context.begin_effect("reused capacity").unwrap();
    assert!(replacement.abort().await.is_clean());
    assert_eq!(runtime.resource_snapshot().effect_transactions.current, 0);

    assert!(second_fiber.dispose().await.is_clean());
    assert!(first_fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_preserves_cross_transaction_lifo_when_an_open_owner_drops() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "retirement-effect-lifo").await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut early = context.begin_effect("early open transaction").unwrap();
    early
        .defer("early cleanup", record_cleanup(&order, "early"))
        .unwrap();

    let late_entered = Arc::new(Notify::new());
    let late_release = Arc::new(Notify::new());
    let mut late = context.begin_effect("late committed transaction").unwrap();
    late.defer("late cleanup", {
        let order = Arc::clone(&order);
        let entered = Arc::clone(&late_entered);
        let release = Arc::clone(&late_release);
        Box::new(move || {
            async move {
                entered.notify_one();
                release.notified().await;
                order.lock().expect("cleanup order poisoned").push("late");
                Ok(())
            }
            .boxed()
        })
    })
    .unwrap();
    let _late_handle = late.commit().unwrap();

    let disposal = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.dispose().await }
    });
    let mut snapshots = fiber.subscribe();
    loop {
        if matches!(snapshots.borrow().state, FiberState::Unloading) {
            break;
        }
        snapshots.changed().await.unwrap();
    }
    late_entered.notified().await;

    drop(early);
    tokio::task::yield_now().await;
    assert!(order.lock().expect("cleanup order poisoned").is_empty());

    late_release.notify_one();
    assert!(disposal.await.unwrap().is_clean());
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["late", "early"]
    );
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn dropping_an_open_transaction_off_executor_still_autoaborts() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "off-executor-effect-drop").await;
    let cleaned = Arc::new(Notify::new());
    let mut transaction = context.begin_effect("off-executor transaction").unwrap();
    transaction
        .defer("off-executor cleanup", {
            let cleaned = Arc::clone(&cleaned);
            Box::new(move || {
                async move {
                    cleaned.notify_one();
                    Ok(())
                }
                .boxed()
            })
        })
        .unwrap();

    std::thread::spawn(move || drop(transaction))
        .join()
        .unwrap();
    cleaned.notified().await;
    wait_effect_resources_zero(&runtime).await;
    assert!(matches!(fiber.snapshot().state, FiberState::Active));
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn effect_failures_and_panics_do_not_skip_later_lifo_cleanup() {
    let runtime = Runtime::default();
    let (fiber, context) = active_context(&runtime, "effect-failure-lifo").await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut transaction = context.begin_effect("fallible effect group").unwrap();
    transaction
        .defer("clean", record_cleanup(&order, "clean"))
        .unwrap();
    transaction
        .defer("panic", {
            let order = Arc::clone(&order);
            Box::new(move || {
                async move {
                    order.lock().expect("cleanup order poisoned").push("panic");
                    expected_cleanup_panic()
                }
                .boxed()
            })
        })
        .unwrap();
    transaction
        .defer("error", {
            let order = Arc::clone(&order);
            Box::new(move || {
                async move {
                    order.lock().expect("cleanup order poisoned").push("error");
                    Err("expected cleanup error".to_owned())
                }
                .boxed()
            })
        })
        .unwrap();
    let handle = transaction.commit().unwrap();

    let report = handle.dispose().await;
    assert_eq!(report.total_failures(), 2);
    assert_eq!(report.failures()[0].label, "error");
    assert_eq!(report.failures()[1].label, "panic");
    assert_eq!(report.failures()[1].error, "cleanup panicked");
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["error", "panic", "clean"]
    );

    let retirement = fiber.dispose().await;
    assert_eq!(retirement.total_failures(), 2);
    assert_eq!(retirement.failures(), report.failures());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct RecursivelyPanickingCleanupPayload;

impl Drop for RecursivelyPanickingCleanupPayload {
    fn drop(&mut self) {
        std::panic::panic_any(Self);
    }
}

#[test]
fn recursive_cleanup_panic_payload_is_bounded_without_skipping_sibling_undos() {
    const CHILD: &str = "RSI_META_RECURSIVE_CLEANUP_PAYLOAD_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(CHILD, "run")
            .args([
                "--exact",
                "recursive_cleanup_panic_payload_is_bounded_without_skipping_sibling_undos",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "recursive cleanup payload escaped its child process:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let runtime = Runtime::default();
            let (fiber, context) = active_context(&runtime, "recursive-cleanup-payload").await;
            let survivors = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut transaction = context.begin_effect("recursive cleanup payload").unwrap();
            transaction
                .defer("survivor", {
                    let survivors = Arc::clone(&survivors);
                    Box::new(move || {
                        async move {
                            survivors.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            Ok(())
                        }
                        .boxed()
                    })
                })
                .unwrap();
            transaction
                .defer(
                    "hostile cleanup",
                    Box::new(|| {
                        async move { std::panic::panic_any(RecursivelyPanickingCleanupPayload) }
                            .boxed()
                    }),
                )
                .unwrap();
            let handle = transaction.commit().unwrap();

            let report = handle.dispose().await;
            assert_eq!(survivors.load(std::sync::atomic::Ordering::Acquire), 1);
            assert_eq!(report.total_failures(), 1);
            assert_eq!(report.failures()[0].label, "hostile cleanup");
            assert_eq!(
                report.failures()[0].error,
                "cleanup and panic payload destruction panicked"
            );
            assert!(runtime.snapshot().terminal.is_none());
            assert_eq!(fiber.dispose().await.failures(), report.failures());
            assert!(runtime.shutdown().await.is_complete());
        });
}
