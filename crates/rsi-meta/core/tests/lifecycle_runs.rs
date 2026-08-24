use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    CleanupPhase, CleanupReport, Context, ContractVersion, DeadlineLimits, FactoryIdentity,
    MetaError, PluginDescriptor, PluginFactory, Requirement, Result, Runtime, RuntimeLimits,
    ShutdownOutcome,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::Notify;

#[test]
fn cleanup_report_wire_rejects_contradictory_failure_metadata() {
    let retained_failure = serde_json::json!({
        "label": "effect",
        "error": "failed",
    });
    let contradictory = [
        serde_json::json!({
            "failures": [retained_failure],
            "total_failures": 0,
            "truncated": false,
        }),
        serde_json::json!({
            "failures": [],
            "total_failures": 1,
            "truncated": false,
        }),
        serde_json::json!({
            "failures": [],
            "total_failures": 0,
            "truncated": true,
        }),
    ];

    for value in contradictory {
        assert!(
            serde_json::from_value::<CleanupReport>(value).is_err(),
            "contradictory cleanup metadata was accepted"
        );
    }

    for value in [
        serde_json::json!({
            "failures": [{ "label": "eff", "error": "err" }],
            "total_failures": 1,
            "truncated": true,
        }),
        serde_json::json!({
            "failures": [{ "label": "effect", "error": "failed" }],
            "total_failures": 2,
            "truncated": true,
        }),
        serde_json::json!({
            "failures": [],
            "total_failures": 1,
            "truncated": true,
        }),
    ] {
        serde_json::from_value::<CleanupReport>(value)
            .expect("valid cleanup truncation metadata was rejected");
    }
}

#[derive(Debug)]
struct FailingActivationWithBlockingCleanup {
    descriptor: PluginDescriptor,
    cleanup_entered: Arc<Notify>,
    cleanup_release: Arc<Notify>,
    cleanups: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct UnexpectedActivation(PluginDescriptor);

#[async_trait]
impl PluginFactory for UnexpectedActivation {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        panic!("a missing dependency must keep the test Fiber pending")
    }
}

#[tokio::test]
async fn dropping_inserted_apply_waiter_off_executor_still_disposes_the_fiber() {
    let runtime = Runtime::default();
    let prepared = runtime
        .prepare(
            Arc::new(UnexpectedActivation(
                PluginDescriptor::new(FactoryIdentity::builtin("off-executor-apply-drop", "1"))
                    .requiring(Requirement::new(
                        "missing",
                        "test.missing",
                        ContractVersion(1),
                    )),
            )),
            Value::Null,
        )
        .unwrap();
    let mut application = Box::pin({
        let root = runtime.root();
        async move { root.apply_prepared(prepared).await }
    });
    assert!(futures_util::poll!(&mut application).is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().fibers.len() == 1
                && runtime.resource_snapshot().scheduler_workers.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the background scheduler worker did not yield to the nested application");

    let dropped = std::thread::spawn(move || drop(application)).join();
    assert!(
        dropped.is_ok(),
        "destroying an admitted waiter outside Tokio panicked"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().fibers.is_empty()
                && runtime.resource_snapshot().scheduler_workers.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("off-executor waiter destruction stranded its Fiber");

    assert!(runtime.shutdown().await.is_complete());
}

#[async_trait]
impl PluginFactory for FailingActivationWithBlockingCleanup {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "earlier cleanup",
            Box::new(move || {
                let cleanups = Arc::clone(&cleanups);
                async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;

        let cleanup_entered = Arc::clone(&self.cleanup_entered);
        let cleanup_release = Arc::clone(&self.cleanup_release);
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "blocking cleanup",
            Box::new(move || {
                let cleanup_entered = Arc::clone(&cleanup_entered);
                let cleanup_release = Arc::clone(&cleanup_release);
                let cleanups = Arc::clone(&cleanups);
                async move {
                    cleanup_entered.notify_one();
                    cleanup_release.notified().await;
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;

        Err(MetaError::Activation(
            "expected activation failure".to_owned(),
        ))
    }
}

#[tokio::test]
async fn dropping_apply_during_rollback_cannot_cancel_claimed_cleanup() {
    let runtime = Runtime::default();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let cleanups = Arc::new(AtomicUsize::new(0));

    let application = tokio::spawn({
        let root = runtime.root();
        let cleanup_entered = Arc::clone(&cleanup_entered);
        let cleanup_release = Arc::clone(&cleanup_release);
        let cleanups = Arc::clone(&cleanups);
        async move {
            root.apply(
                Arc::new(FailingActivationWithBlockingCleanup {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "cancelled-rollback",
                        "1",
                    )),
                    cleanup_entered,
                    cleanup_release,
                    cleanups,
                }),
                Value::Null,
            )
            .await
        }
    });

    cleanup_entered.notified().await;
    application.abort();
    assert!(application.await.unwrap_err().is_cancelled());
    cleanup_release.notify_waiters();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().fibers.is_empty() && cleanups.load(Ordering::Acquire) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("caller cancellation lost cleanup already claimed by rollback");
}

#[derive(Debug)]
struct BlockingShutdownCleanup {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingShutdownCleanup {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "blocking shutdown cleanup",
            Box::new(move || {
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                let cleanups = Arc::clone(&cleanups);
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

#[tokio::test]
async fn shutdown_timeout_is_a_waiter_outcome_and_later_join_reaches_complete() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            shutdown_wait: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cleanups = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            Arc::new(BlockingShutdownCleanup {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "blocking-shutdown",
                    "1",
                )),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                cleanups: Arc::clone(&cleanups),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let first = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    entered.notified().await;
    let first = first.await.unwrap();
    let ShutdownOutcome::TimedOut { report, unresolved } = first else {
        panic!("the first waiter reported completion while cleanup was blocked");
    };
    assert!(report.is_clean());
    let unfinished = runtime.resource_snapshot().cleanup_runs;
    assert_eq!(unfinished.current, 1);
    assert_eq!(unfinished.high_watermark, 1);
    assert_eq!(unresolved.total, 1);
    assert!(!unresolved.truncated);
    assert_eq!(unresolved.samples.len(), 1);
    assert_eq!(unresolved.samples[0].phase, CleanupPhase::RunningEffects);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);
    assert_eq!(runtime.snapshot().fibers.len(), 1);

    release.notify_waiters();
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), runtime.shutdown())
        .await
        .expect("a later shutdown waiter did not join persistent cleanup");
    assert!(matches!(second, ShutdownOutcome::Complete(ref report) if report.is_clean()));
    assert_eq!(runtime.resource_snapshot().cleanup_runs.current, 0);
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(runtime.snapshot().fibers.is_empty());
}

#[derive(Debug)]
struct IndependentShutdownCleanup {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for IndependentShutdownCleanup {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        context.defer(
            "independent shutdown cleanup",
            Box::new(move || {
                let entered = Arc::clone(&entered);
                async move {
                    entered.notify_one();
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test(start_paused = true)]
async fn held_prepared_proof_delays_completion_without_blocking_root_cleanup() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            shutdown_wait: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let cleanup_entered = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(IndependentShutdownCleanup {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "proof-independent-root",
                    "1",
                )),
                entered: Arc::clone(&cleanup_entered),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let proof = runtime
        .prepare(
            Arc::new(RetainedFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "held-shutdown-proof",
                    "1",
                )),
                _retained: Arc::new(()),
            }),
            Value::Null,
        )
        .unwrap();

    let first = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        cleanup_entered.notified(),
    )
    .await
    .expect("held proof head-of-line blocked an independent root cleanup");
    assert!(runtime.snapshot().fibers.is_empty());
    assert!(!first.is_finished());
    let held = runtime.resource_snapshot();
    assert!(matches!(
        runtime.prepare(
            Arc::new(RetainedFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "post-close-proof",
                    "1",
                )),
                _retained: Arc::new(()),
            }),
            Value::Null,
        ),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert_eq!(runtime.resource_snapshot(), held);

    tokio::time::advance(std::time::Duration::from_millis(21)).await;
    assert!(matches!(
        first.await.unwrap(),
        ShutdownOutcome::TimedOut { .. }
    ));
    assert_eq!(runtime.resource_snapshot().fibers.current, 1);

    drop(proof);
    assert!(runtime.shutdown().await.is_complete());
    let complete = runtime.resource_snapshot();
    assert_eq!(complete.preparations.current, 0);
    assert_eq!(complete.fibers.current, 0);
    assert_eq!(complete.service_calls.current, 0);
    assert_eq!(complete.event_dispatches.current, 0);
    assert_eq!(complete.event_callbacks.current, 0);
    assert_eq!(complete.cleanup_runs.current, 0);
    assert_eq!(complete.scheduler_workers.current, 0);
    let revision = runtime.snapshot().revision;
    assert!(matches!(
        runtime.prepare(
            Arc::new(RetainedFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "after-complete-proof",
                    "1",
                )),
                _retained: Arc::new(()),
            }),
            Value::Null,
        ),
        Err(MetaError::RuntimeShuttingDown)
    ));
    tokio::task::yield_now().await;
    assert_eq!(runtime.resource_snapshot(), complete);
    assert_eq!(runtime.snapshot().revision, revision);
}

#[derive(Debug)]
struct RetainedFactory {
    descriptor: PluginDescriptor,
    _retained: Arc<()>,
}

#[async_trait]
impl PluginFactory for RetainedFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn disposed_handle_retains_only_small_snapshot_state() {
    let runtime = Runtime::default();
    let retained = Arc::new(());
    let retained_weak = Arc::downgrade(&retained);
    let fiber = runtime
        .root()
        .apply(
            Arc::new(RetainedFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("released", "1")),
                _retained: retained,
            }),
            Value::String("configuration payload".repeat(1_024)),
        )
        .await
        .unwrap();
    assert!(retained_weak.upgrade().is_some());

    assert!(fiber.dispose().await.is_clean());
    assert!(retained_weak.upgrade().is_none());
    assert_eq!(
        fiber.snapshot().factory,
        FactoryIdentity::builtin("released", "1")
    );
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);
}

#[derive(Debug)]
struct LongCleanupFailure(PluginDescriptor);

#[async_trait]
impl PluginFactory for LongCleanupFailure {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.defer(
            "cleanup",
            Box::new(|| async move { Err("界".repeat(4_096)) }.boxed()),
        )
    }
}

#[tokio::test]
async fn cleanup_failures_are_utf8_bounded_while_they_are_formatted() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_diagnostic_bytes: 16,
            ..rsi_meta::PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(LongCleanupFailure(PluginDescriptor::new(
                FactoryIdentity::builtin("bounded-cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();

    let report = fiber.dispose().await;
    assert_eq!(report.total_failures(), 1);
    assert!(report.is_truncated());
    assert_eq!(report.failures().len(), 1);
    let failure = &report.failures()[0];
    assert!(failure.label.len() + failure.error.len() <= 16);
    assert!(std::str::from_utf8(failure.error.as_bytes()).is_ok());
}

#[derive(Debug)]
struct BlockingReconfigurationFactory {
    descriptor: PluginDescriptor,
    normalization_entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    normalization_release: Arc<(Mutex<bool>, Condvar)>,
    cleanup_entered: Arc<Notify>,
    cleanup_release: Arc<Notify>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingReconfigurationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        if !config.is_null() {
            if let Some(entered) = self.normalization_entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let (released, wakeup) = &*self.normalization_release;
            let guard = released.lock().unwrap();
            drop(wakeup.wait_while(guard, |released| !*released).unwrap());
        }
        Ok(config)
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.cleanup_entered);
        let release = Arc::clone(&self.cleanup_release);
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "blocking reconfiguration cleanup",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disposal_does_not_deadlock_behind_a_blocking_reconfiguration() {
    let runtime = Runtime::default();
    let (normalization_entered_tx, normalization_entered_rx) = tokio::sync::oneshot::channel();
    let normalization_release = Arc::new((Mutex::new(false), Condvar::new()));
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let cleanups = Arc::new(AtomicUsize::new(0));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(BlockingReconfigurationFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "blocking-reconfiguration",
                    "1",
                )),
                normalization_entered: Mutex::new(Some(normalization_entered_tx)),
                normalization_release: Arc::clone(&normalization_release),
                cleanup_entered: Arc::clone(&cleanup_entered),
                cleanup_release: Arc::clone(&cleanup_release),
                cleanups: Arc::clone(&cleanups),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let id = fiber.id();

    let reconfiguration = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.reconfigure(Value::Bool(true)).await }
    });
    normalization_entered_rx
        .await
        .expect("blocking normalizer did not start");
    let disposal = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.dispose().await }
    });

    if tokio::time::timeout(
        std::time::Duration::from_secs(1),
        cleanup_entered.notified(),
    )
    .await
    .is_err()
    {
        let (released, wakeup) = &*normalization_release;
        *released.lock().unwrap() = true;
        wakeup.notify_all();
        cleanup_release.notify_one();
        panic!("disposal waited for the configuration gate while holding the transition");
    }

    let (released, wakeup) = &*normalization_release;
    *released.lock().unwrap() = true;
    wakeup.notify_all();
    let reconfiguration = tokio::time::timeout(std::time::Duration::from_secs(1), reconfiguration)
        .await
        .expect("reconfiguration remained deadlocked after normalization returned")
        .unwrap();
    assert!(matches!(
        reconfiguration,
        Err(MetaError::FiberDisposed { fiber }) if fiber == id
    ));

    cleanup_release.notify_one();
    assert!(disposal.await.unwrap().is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(runtime.snapshot().fibers.is_empty());
    let resources = runtime.resource_snapshot();
    assert_eq!(resources.preparations.current, 0);
    assert_eq!(resources.fibers.current, 0);
    assert_eq!(resources.retained_plugin_bytes.current, 0);
    assert_eq!(resources.reconciliations.current, 0);
    assert_eq!(resources.cleanup_runs.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct ControlledCleanupFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    failure: Option<&'static str>,
}

#[async_trait]
impl PluginFactory for ControlledCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        let failure = self.failure;
        context.defer(
            "controlled cleanup",
            Box::new(move || {
                async move {
                    entered.notify_one();
                    release.notified().await;
                    failure.map_or(Ok(()), |message| Err(message.to_owned()))
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn shutdown_joins_a_disposal_captured_before_registry_removal() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: rsi_meta::ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let target_entered = Arc::new(Notify::new());
    let target_release = Arc::new(Notify::new());
    let target = runtime
        .root()
        .apply(
            Arc::new(ControlledCleanupFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "reported-disposal",
                    "1",
                )),
                entered: Arc::clone(&target_entered),
                release: Arc::clone(&target_release),
                failure: Some("captured cleanup failure"),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let blocker_entered = Arc::new(Notify::new());
    let blocker_release = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(ControlledCleanupFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "shutdown-ordering-blocker",
                    "1",
                )),
                entered: Arc::clone(&blocker_entered),
                release: Arc::clone(&blocker_release),
                failure: None,
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let public_disposal = tokio::spawn(async move { target.dispose().await });
    target_entered.notified().await;
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(runtime.snapshot().shutting_down);

    target_release.notify_one();
    let public_report = public_disposal.await.unwrap();
    assert_eq!(public_report.total_failures(), 1);
    blocker_entered.notified().await;
    blocker_release.notify_one();

    let ShutdownOutcome::Complete(report) = shutdown.await.unwrap() else {
        panic!("shutdown did not complete after both persistent runs settled");
    };
    assert_eq!(report.total_failures(), 1);
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| failure.error.contains("captured cleanup failure")),
        "shutdown lost the report of a disposal removed after membership capture"
    );
}

#[derive(Debug)]
struct ParentWithControlledChild {
    descriptor: PluginDescriptor,
    child: Arc<ControlledCleanupFactory>,
    child_handle: Arc<Mutex<Option<rsi_meta::FiberHandle>>>,
}

#[async_trait]
impl PluginFactory for ParentWithControlledChild {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let factory: Arc<dyn PluginFactory> = self.child.clone();
        let child = context.apply(factory, Value::Null).await?;
        *self.child_handle.lock().unwrap() = Some(child);
        Ok(())
    }
}

#[tokio::test]
async fn shutdown_preserves_a_child_report_removed_before_parent_cleanup() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: rsi_meta::ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let child_entered = Arc::new(Notify::new());
    let child_release = Arc::new(Notify::new());
    let child_handle = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ParentWithControlledChild {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "parent-of-reported-child",
                    "1",
                )),
                child: Arc::new(ControlledCleanupFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "reported-child",
                        "1",
                    )),
                    entered: Arc::clone(&child_entered),
                    release: Arc::clone(&child_release),
                    failure: Some("captured child cleanup failure"),
                }),
                child_handle: Arc::clone(&child_handle),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let child = child_handle.lock().unwrap().take().unwrap();

    let public_disposal = tokio::spawn(async move { child.dispose().await });
    child_entered.notified().await;
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(runtime.snapshot().shutting_down);

    child_release.notify_one();
    assert_eq!(public_disposal.await.unwrap().total_failures(), 1);
    let ShutdownOutcome::Complete(report) = shutdown.await.unwrap() else {
        panic!("shutdown did not complete after child and parent disposal settled");
    };
    assert_eq!(report.total_failures(), 1);
    assert!(
        report
            .failures()
            .iter()
            .any(|failure| failure.error.contains("captured child cleanup failure")),
        "shutdown lost a completed child's report before parent cleanup claimed ownership"
    );
}

#[tokio::test]
async fn parent_and_public_child_disposal_join_one_descendant_report() {
    let runtime = Runtime::default();
    let child_entered = Arc::new(Notify::new());
    let child_release = Arc::new(Notify::new());
    let child_handle = Arc::new(Mutex::new(None));
    let parent = runtime
        .root()
        .apply(
            Arc::new(ParentWithControlledChild {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("joining-parent", "1")),
                child: Arc::new(ControlledCleanupFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "joining-child",
                        "1",
                    )),
                    entered: Arc::clone(&child_entered),
                    release: Arc::clone(&child_release),
                    failure: Some("joined child cleanup failure"),
                }),
                child_handle: Arc::clone(&child_handle),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let child = child_handle.lock().unwrap().take().unwrap();

    let parent_disposal = tokio::spawn(async move { parent.dispose().await });
    child_entered.notified().await;
    let child_disposal = tokio::spawn(async move { child.dispose().await });
    child_release.notify_one();

    assert_eq!(child_disposal.await.unwrap().total_failures(), 1);
    let parent_report = parent_disposal.await.unwrap();
    assert_eq!(parent_report.total_failures(), 1);
    assert_eq!(
        parent_report
            .failures()
            .iter()
            .filter(|failure| failure.error.contains("joined child cleanup failure"))
            .count(),
        1
    );
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct OrderedCleanupFailures {
    descriptor: PluginDescriptor,
    failures: &'static [(&'static str, &'static str)],
}

#[async_trait]
impl PluginFactory for OrderedCleanupFailures {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        for &(label, error) in self.failures {
            context.defer(
                label,
                Box::new(move || async move { Err(error.to_owned()) }.boxed()),
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ParentWithOrderedCleanupFailures {
    descriptor: PluginDescriptor,
    children: Vec<Arc<OrderedCleanupFailures>>,
}

#[async_trait]
impl PluginFactory for ParentWithOrderedCleanupFailures {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        for child in &self.children {
            let factory: Arc<dyn PluginFactory> = child.clone();
            context.apply(factory, Value::Null).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn merged_cleanup_reports_never_retain_entries_after_an_omitted_failure() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_diagnostic_bytes: 10,
            ..rsi_meta::PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let earlier_child = Arc::new(OrderedCleanupFailures {
        descriptor: PluginDescriptor::new(FactoryIdentity::builtin("earlier-child", "1")),
        // Cleanup is LIFO, so the four-byte failure is observed first.
        failures: &[("", "z"), ("aa", "bb")],
    });
    let later_child = Arc::new(OrderedCleanupFailures {
        descriptor: PluginDescriptor::new(FactoryIdentity::builtin("later-child", "1")),
        failures: &[("aaaa", "bbbb")],
    });
    let parent = runtime
        .root()
        .apply(
            Arc::new(ParentWithOrderedCleanupFailures {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "ordered-cleanup-parent",
                    "1",
                )),
                // Child cleanup is LIFO, so later_child consumes eight bytes
                // before earlier_child's four-byte then one-byte failures.
                children: vec![earlier_child, later_child],
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let report = parent.dispose().await;
    assert_eq!(report.total_failures(), 3);
    assert!(report.is_truncated());
    assert_eq!(report.failures().len(), 1);
    assert_eq!(report.failures()[0].label, "aaaa");
    assert_eq!(report.failures()[0].error, "bbbb");
    assert!(runtime.shutdown().await.is_complete());
}
