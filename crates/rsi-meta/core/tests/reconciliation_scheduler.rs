use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, ContractVersion, DeadlineLimits, ExecutionLimits, FactoryIdentity, FiberHandle,
    FiberState, IsolationId, MetaError, PluginFactory, PreparedActivation, Requirement, Result,
    Runtime, RuntimeLimits, ServiceEndpoint,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[path = "support/resolver.rs"]
mod resolver;
mod support;
use resolver::resolved;

use support::{Echo, EndpointFactory, FactorySpec, PassiveFactory};

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct NoopEndpoint;

#[async_trait]
impl ServiceEndpoint for NoopEndpoint {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        _: rsi_meta::ProviderChannel<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct SchedulerProvider {
    spec: FactorySpec,
}

#[async_trait]
impl PluginFactory for SchedulerProvider {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context.provide("scheduler", "test.scheduler", V1, Arc::new(NoopEndpoint))?;
        Ok(())
    }
}

#[derive(Debug)]
struct SchedulerConsumer {
    spec: FactorySpec,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for SchedulerConsumer {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "scheduler consumer",
            Box::new(move || {
                let cleanups = Arc::clone(&cleanups);
                async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn retirement_yields_its_only_reconciliation_slot_to_dependents() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(SchedulerConsumer {
                spec: FactorySpec::new(FactoryIdentity::linked("scheduler-consumer", "1"))
                    .requiring(Requirement::new("scheduler", "test.scheduler", V1)),
                cleanups: Arc::clone(&cleanups),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(SchedulerProvider {
                spec: FactorySpec::new(FactoryIdentity::linked("scheduler-provider", "1")),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), provider.dispose())
        .await
        .expect("provider retirement deadlocked behind its only scheduler slot");
    assert!(report.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    assert!(consumer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_reconciliation_handoffs_never_overtake_the_resource_ledger() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let mut fibers = Vec::new();
    for index in 0..64 {
        fibers.push(
            runtime
                .root()
                .apply(
                    crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                        FactoryIdentity::linked(format!("handoff-{index}"), "1"),
                    )))),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
    }

    let disposals = fibers.into_iter().map(|fiber| {
        tokio::spawn(async move {
            let report = fiber.dispose().await;
            assert!(report.is_clean());
        })
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        futures_util::future::join_all(disposals),
    )
    .await
    .expect("a saturated reconciliation handoff panicked or stranded its run")
    .into_iter()
    .for_each(|result| result.unwrap());

    let reconciliations = runtime.resource_snapshot().reconciliations;
    assert_eq!(reconciliations.current, 0);
    assert_eq!(reconciliations.high_watermark, 1);
    assert_eq!(reconciliations.rejected, 0);
    let workers = runtime.resource_snapshot().scheduler_workers;
    assert_eq!(workers.current, 0);
    assert_eq!(workers.high_watermark, 1);
    assert_eq!(workers.rejected, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct NestedChild(FactorySpec);

#[async_trait]
impl PluginFactory for NestedChild {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct AwaitingParent {
    spec: FactorySpec,
    child: Arc<Mutex<Option<FiberHandle>>>,
}

#[async_trait]
impl PluginFactory for AwaitingParent {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let child = context
            .apply(
                crate::resolved(Arc::new(NestedChild(FactorySpec::new(
                    FactoryIdentity::linked("nested-scheduler-child", "1"),
                )))),
                Value::Null,
            )
            .await?;
        *self.child.lock().expect("child capture poisoned") = Some(child);
        Ok(())
    }
}

#[tokio::test]
async fn parent_activation_can_await_child_apply_with_one_reconciliation_slot() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let captured = Arc::new(Mutex::new(None));
    let parent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.root().apply(
            crate::resolved(Arc::new(AwaitingParent {
                spec: FactorySpec::new(FactoryIdentity::linked("nested-scheduler-parent", "1")),
                child: Arc::clone(&captured),
            })),
            Value::Null,
        ),
    )
    .await
    .expect("parent activation deadlocked while awaiting its child")
    .unwrap();

    assert!(matches!(parent.snapshot().state, FiberState::Active));
    let child = captured
        .lock()
        .expect("child capture poisoned")
        .clone()
        .expect("parent activation completed its child apply");
    assert!(matches!(child.snapshot().state, FiberState::Active));
    let scheduler = runtime.resource_snapshot().reconciliations;
    assert_eq!(scheduler.current, 0);
    assert_eq!(scheduler.high_watermark, 1);

    assert!(parent.dispose().await.is_clean());
    assert!(matches!(child.snapshot().state, FiberState::Disposed));
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct RecordingTarget {
    spec: FactorySpec,
    configurations: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl PluginFactory for RecordingTarget {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let config = Arc::clone(plan.config());
        self.configurations
            .lock()
            .expect("configuration log poisoned")
            .push((*config).clone());
        Ok(())
    }
}

#[derive(Debug)]
struct AwaitingReconfiguration {
    spec: FactorySpec,
    target: FiberHandle,
}

#[async_trait]
impl PluginFactory for AwaitingReconfiguration {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        self.target
            .reconfigure(serde_json::json!({"revision": 2}))
            .await?;
        Ok(())
    }
}

#[tokio::test]
async fn activation_can_await_other_reconfiguration_with_one_reconciliation_slot() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let configurations = Arc::new(Mutex::new(Vec::new()));
    let target = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RecordingTarget {
                spec: FactorySpec::new(FactoryIdentity::linked(
                    "nested-reconfiguration-target",
                    "1",
                )),
                configurations: Arc::clone(&configurations),
            })),
            serde_json::json!({"revision": 1}),
        )
        .await
        .unwrap();

    let parent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.root().apply(
            crate::resolved(Arc::new(AwaitingReconfiguration {
                spec: FactorySpec::new(FactoryIdentity::linked(
                    "nested-reconfiguration-parent",
                    "1",
                )),
                target: target.clone(),
            })),
            Value::Null,
        ),
    )
    .await
    .expect("activation deadlocked while awaiting another Fiber reconfiguration")
    .unwrap();

    assert!(matches!(parent.snapshot().state, FiberState::Active));
    assert_eq!(
        *configurations.lock().expect("configuration log poisoned"),
        vec![
            serde_json::json!({"revision": 1}),
            serde_json::json!({"revision": 2}),
        ]
    );
    assert_eq!(
        runtime.resource_snapshot().reconciliations.high_watermark,
        1
    );

    assert!(parent.dispose().await.is_clean());
    assert!(target.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct AwaitingDisposal {
    spec: FactorySpec,
    target: FiberHandle,
}

#[async_trait]
impl PluginFactory for AwaitingDisposal {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        let report = self.target.dispose().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(rsi_meta::MetaError::Activation(
                "nested disposal reported cleanup failures".to_owned(),
            ))
        }
    }
}

#[tokio::test]
async fn activation_can_await_other_disposal_with_one_reconciliation_slot() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let target = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(NestedChild(FactorySpec::new(
                FactoryIdentity::linked("nested-disposal-target", "1"),
            )))),
            Value::Null,
        )
        .await
        .unwrap();

    let parent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.root().apply(
            crate::resolved(Arc::new(AwaitingDisposal {
                spec: FactorySpec::new(FactoryIdentity::linked("nested-disposal-parent", "1")),
                target: target.clone(),
            })),
            Value::Null,
        ),
    )
    .await
    .expect("activation deadlocked while awaiting another Fiber disposal")
    .unwrap();

    assert!(matches!(parent.snapshot().state, FiberState::Active));
    assert!(matches!(target.snapshot().state, FiberState::Disposed));
    assert_eq!(
        runtime.resource_snapshot().reconciliations.high_watermark,
        1
    );

    assert!(parent.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct ReentrantProviderDisposal {
    spec: FactorySpec,
    provider: FiberHandle,
    activations: AtomicUsize,
    entered: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for ReentrantProviderDisposal {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        if self.activations.fetch_add(1, Ordering::AcqRel) == 0 {
            return Ok(());
        }
        self.entered.notify_one();
        let report = self.provider.dispose().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(MetaError::Activation(
                "reentrant provider disposal reported cleanup failures".to_owned(),
            ))
        }
    }
}

#[tokio::test(start_paused = true)]
async fn service_change_cancels_reentrant_activation_before_the_transition_deadline() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_secs(10),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(SchedulerProvider {
                spec: FactorySpec::new(FactoryIdentity::linked("reentrant-disposal-provider", "1")),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ReentrantProviderDisposal {
                spec: FactorySpec::new(FactoryIdentity::linked("reentrant-disposal-consumer", "1"))
                    .requiring(Requirement::new("scheduler", "test.scheduler", V1)),
                provider: provider.clone(),
                activations: AtomicUsize::new(0),
                entered: Arc::clone(&entered),
            })),
            Value::Null,
        )
        .await
        .unwrap();

    let reconfiguration = tokio::spawn({
        let consumer = consumer.clone();
        async move {
            consumer
                .reconfigure(serde_json::json!({"revision": 2}))
                .await
        }
    });
    entered.notified().await;
    tokio::time::timeout(std::time::Duration::from_secs(1), reconfiguration)
        .await
        .expect("service withdrawal waited for the activation deadline to break a transition cycle")
        .unwrap()
        .unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), provider.dispose())
        .await
        .expect("provider disposal did not finish after cancelling the stale activation");
    assert!(report.is_clean());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if matches!(consumer.snapshot().state, FiberState::Pending(_)) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the dependent did not converge after provider withdrawal");

    assert!(consumer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct ConfigPointerConsumer {
    spec: FactorySpec,
    pointers: Arc<Mutex<Vec<usize>>>,
}

#[async_trait]
impl PluginFactory for ConfigPointerConsumer {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let config = Arc::clone(plan.config());
        self.pointers
            .lock()
            .expect("config pointer log poisoned")
            .push(Arc::as_ptr(&config) as usize);
        Ok(())
    }
}

#[tokio::test]
async fn service_reconciliation_freshly_prepares_a_distinct_attempt_configuration() {
    let runtime = Runtime::default();
    let provider_factory = || {
        Arc::new(SchedulerProvider {
            spec: FactorySpec::new(FactoryIdentity::linked("config-arc-provider", "1")),
        }) as Arc<dyn PluginFactory>
    };
    let provider = runtime
        .root()
        .apply(
            crate::resolver::resolved_dyn(provider_factory()),
            Value::Null,
        )
        .await
        .unwrap();
    let pointers = Arc::new(Mutex::new(Vec::new()));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ConfigPointerConsumer {
                spec: FactorySpec::new(FactoryIdentity::linked("config-arc-consumer", "1"))
                    .requiring(Requirement::new("scheduler", "test.scheduler", V1)),
                pointers: Arc::clone(&pointers),
            })),
            serde_json::json!({"nested": [1, 2, 3]}),
        )
        .await
        .unwrap();

    assert!(provider.dispose().await.is_clean());
    runtime
        .root()
        .apply(
            crate::resolver::resolved_dyn(provider_factory()),
            Value::Null,
        )
        .await
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();
    let pointers = pointers.lock().expect("config pointer log poisoned");
    assert_eq!(pointers.len(), 2);
    assert_ne!(
        pointers[0], pointers[1],
        "a binding-identity change must not reactivate a consumed prepared value"
    );
}

#[derive(Debug)]
struct SpawnedAwaitingParent(FactorySpec);

#[async_trait]
impl PluginFactory for SpawnedAwaitingParent {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        tokio::spawn(async move {
            context
                .apply(
                    crate::resolved(Arc::new(NestedChild(FactorySpec::new(
                        FactoryIdentity::linked("spawned-nested-child", "1"),
                    )))),
                    Value::Null,
                )
                .await
        })
        .await
        .expect("spawned child task remains healthy")?;
        Ok(())
    }
}

#[tokio::test]
async fn paused_activation_allows_spawned_child_progress_with_one_slot() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    let parent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.root().apply(
            crate::resolved(Arc::new(SpawnedAwaitingParent(FactorySpec::new(
                FactoryIdentity::linked("spawned-nested-parent", "1"),
            )))),
            Value::Null,
        ),
    )
    .await
    .expect("a spawned nested apply deadlocked behind its paused parent")
    .unwrap();
    assert!(matches!(parent.snapshot().state, FiberState::Active));
    assert_eq!(
        runtime.resource_snapshot().reconciliations.high_watermark,
        1
    );
    assert!(parent.dispose().await.is_clean());
}

#[derive(Debug)]
struct BlockingCleanupConsumer {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for BlockingCleanupConsumer {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        context.defer(
            "blocked scheduler cleanup",
            Box::new(move || {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct SignalledAwaitingDisposal {
    spec: FactorySpec,
    target: FiberHandle,
    requested: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for SignalledAwaitingDisposal {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        self.requested.notify_one();
        let report = self.target.dispose().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(rsi_meta::MetaError::Activation(
                "nested disposal reported cleanup failures".to_owned(),
            ))
        }
    }
}

#[tokio::test]
async fn nested_disposal_intent_is_retained_while_the_target_is_already_reconciling() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(SchedulerProvider {
                spec: FactorySpec::new(FactoryIdentity::linked("overlap-provider", "1")),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let target = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(BlockingCleanupConsumer {
                spec: FactorySpec::new(FactoryIdentity::linked("overlap-consumer", "1"))
                    .requiring(Requirement::new("scheduler", "test.scheduler", V1)),
                entered: Arc::clone(&cleanup_entered),
                release: Arc::clone(&cleanup_release),
            })),
            Value::Null,
        )
        .await
        .unwrap();

    let provider_disposal = tokio::spawn(async move { provider.dispose().await });
    cleanup_entered.notified().await;

    let disposal_requested = Arc::new(Notify::new());
    let disposer = tokio::spawn({
        let root = runtime.root();
        let target = target.clone();
        let disposal_requested = Arc::clone(&disposal_requested);
        async move {
            root.apply(
                crate::resolved(Arc::new(SignalledAwaitingDisposal {
                    spec: FactorySpec::new(FactoryIdentity::linked("overlap-disposer", "1")),
                    target,
                    requested: disposal_requested,
                })),
                Value::Null,
            )
            .await
        }
    });
    disposal_requested.notified().await;
    cleanup_release.notify_one();

    let disposer = tokio::time::timeout(std::time::Duration::from_secs(1), disposer)
        .await
        .expect("a nested disposal intent was lost behind the active reconciliation")
        .unwrap()
        .unwrap();
    assert!(matches!(target.snapshot().state, FiberState::Disposed));
    assert!(provider_disposal.await.unwrap().is_clean());
    assert!(disposer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct HangingNestedChild {
    spec: FactorySpec,
    entered: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for HangingNestedChild {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[derive(Debug)]
struct AwaitingHangingChild {
    spec: FactorySpec,
    child_entered: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for AwaitingHangingChild {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context
            .apply(
                crate::resolved(Arc::new(HangingNestedChild {
                    spec: FactorySpec::new(FactoryIdentity::linked("cancelled-nested-child", "1")),
                    entered: Arc::clone(&self.child_entered),
                })),
                Value::Null,
            )
            .await?;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn timed_out_parent_requeues_its_cancelled_nested_apply_claim() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let child_entered = Arc::new(Notify::new());
    let applying = tokio::spawn({
        let root = runtime.root();
        let child_entered = Arc::clone(&child_entered);
        async move {
            root.apply(
                crate::resolved(Arc::new(AwaitingHangingChild {
                    spec: FactorySpec::new(FactoryIdentity::linked("cancelled-nested-parent", "1")),
                    child_entered,
                })),
                Value::Null,
            )
            .await
        }
    });
    child_entered.notified().await;

    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        applying.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let resources = runtime.resource_snapshot();
            if runtime.snapshot().fibers.is_empty()
                && resources.fibers.current == 0
                && resources.reconciliations.current == 0
                && resources.cleanup_runs.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a cancelled nested apply claim stranded its Fiber in the scheduler");
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct BlockingCleanupTarget {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingCleanupTarget {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "cancelled nested disposal cleanup",
            Box::new(move || {
                async move {
                    entered.notify_one();
                    release.notified().await;
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test(start_paused = true)]
async fn timed_out_parent_requeues_its_cancelled_nested_disposal_claim() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..ExecutionLimits::default()
        },
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let cleanups = Arc::new(AtomicUsize::new(0));
    let target = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(BlockingCleanupTarget {
                spec: FactorySpec::new(FactoryIdentity::linked("cancelled-disposal-target", "1")),
                entered: Arc::clone(&cleanup_entered),
                release: Arc::clone(&cleanup_release),
                cleanups: Arc::clone(&cleanups),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let applying = tokio::spawn({
        let root = runtime.root();
        let target = target.clone();
        async move {
            root.apply(
                crate::resolved(Arc::new(AwaitingDisposal {
                    spec: FactorySpec::new(FactoryIdentity::linked(
                        "cancelled-disposal-parent",
                        "1",
                    )),
                    target,
                })),
                Value::Null,
            )
            .await
        }
    });
    cleanup_entered.notified().await;

    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        applying.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    cleanup_release.notify_one();
    for _ in 0..1_000 {
        let resources = runtime.resource_snapshot();
        if matches!(target.snapshot().state, FiberState::Disposed)
            && runtime.snapshot().fibers.is_empty()
            && resources.reconciliations.current == 0
            && resources.cleanup_runs.current == 0
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let snapshot = runtime.snapshot();
    let resources = runtime.resource_snapshot();
    assert!(
        matches!(target.snapshot().state, FiberState::Disposed)
            && snapshot.fibers.is_empty()
            && resources.reconciliations.current == 0
            && resources.cleanup_runs.current == 0,
        "a cancelled nested disposal claim stranded its cleanup run: {snapshot:?}; {resources:?}",
    );
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(runtime.shutdown().await.is_complete());
}

#[path = "reconciliation_scheduler/contract_invariants.rs"]
mod contract_invariants;
#[path = "reconciliation_scheduler/foundation.rs"]
mod foundation;
