use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    Context, ContractVersion, DeadlineLimits, DispatchMode, EventOptions, FactoryIdentity,
    FiberState, InvocationContext, MetaError, PluginDescriptor, PluginFactory, ProviderChannel,
    Provision, Requirement, Result, Runtime, RuntimeLimits, ServiceEndpoint, TopologyLimits,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::Poll;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

mod support;

use support::{
    ContextCaptureFactory, Echo, EndpointFactory, ListenerCaptureFactory, NoopHandler,
    PassiveFactory,
};

const V1: ContractVersion = ContractVersion(1);

#[tokio::test]
async fn cancelling_apply_before_handle_acknowledgement_disposes_the_active_fiber() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let mut application = Box::pin(root.apply(
        Arc::new(PassiveFactory(PluginDescriptor::new(
            FactoryIdentity::builtin("unacknowledged-apply", "1"),
        ))),
        Value::Null,
    ));

    loop {
        assert!(matches!(
            futures_util::poll!(&mut application),
            Poll::Pending
        ));
        if !runtime.snapshot().fibers.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime
                .snapshot()
                .fibers
                .iter()
                .any(|fiber| matches!(fiber.state, FiberState::Active))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runtime-owned activation did not finish");

    drop(application);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().fibers.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unacknowledged apply stranded an active Fiber");
}

#[derive(Debug)]
struct TerminalizedActivationFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for TerminalizedActivationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.on(
            "terminal-publication",
            Arc::new(NoopHandler),
            EventOptions::default(),
        )?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn terminal_runtime_never_publishes_an_in_flight_activation() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let application = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(TerminalizedActivationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "terminalized-activation",
                        "1",
                    )),
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    runtime.mark_terminal("test terminal fence");
    release.notify_one();

    let fiber = application.await.unwrap().unwrap();
    assert!(matches!(
        fiber.snapshot().state,
        FiberState::Failed(ref error) if error.contains("test terminal fence")
    ));
}

#[derive(Debug)]
struct BlockingValidationFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

#[async_trait]
impl PluginFactory for BlockingValidationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        self.entered.wait();
        self.release.wait();
        Ok(config)
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_linearizes_with_apply_after_arbitrary_validation() {
    let runtime = Runtime::default();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let root = runtime.root();
    let application = tokio::spawn({
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(BlockingValidationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "validation-race",
                        "1",
                    )),
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });
    tokio::task::spawn_blocking({
        let entered = Arc::clone(&entered);
        move || entered.wait()
    })
    .await
    .unwrap();
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown completed before pre-close validation released its admission lease",
    );
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    assert!(matches!(
        application.await.unwrap(),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert!(shutdown.await.unwrap().is_clean());
    assert!(runtime.snapshot().fibers.is_empty());
}

#[tokio::test(start_paused = true)]
async fn application_waiter_timeout_does_not_release_a_running_blocking_preparation() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let application = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(BlockingValidationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "timed-out-preparation",
                        "1",
                    )),
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });
    tokio::task::spawn_blocking({
        let entered = Arc::clone(&entered);
        move || entered.wait()
    })
    .await
    .unwrap();

    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        application.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    let running = runtime.resource_snapshot();
    assert_eq!(running.preparations.current, 1);
    assert_eq!(running.fibers.current, 1);

    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let resources = runtime.resource_snapshot();
            if resources.preparations.current == 0
                && resources.fibers.current == 0
                && resources.retained_plugin_bytes.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached blocking preparation did not release its proof reservations");
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct BlockingRootCleanupFactory {
    descriptor: PluginDescriptor,
    label: &'static str,
    entered: tokio::sync::mpsc::UnboundedSender<&'static str>,
    release: CancellationToken,
}

#[async_trait]
impl PluginFactory for BlockingRootCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let label = self.label;
        let entered = self.entered.clone();
        let release = self.release.clone();
        context.defer(
            label,
            Box::new(move || {
                async move {
                    entered.send(label).map_err(|error| error.to_string())?;
                    release.cancelled().await;
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn shutdown_starts_all_roots_before_waiting_for_cleanup() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: rsi_meta::ExecutionLimits {
            maximum_concurrent_reconciliations: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let release = CancellationToken::new();
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    for label in ["first", "second"] {
        let fiber = runtime
            .root()
            .apply(
                Arc::new(BlockingRootCleanupFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(label, "1")),
                    label,
                    entered: entered.clone(),
                    release: release.clone(),
                }),
                Value::Null,
            )
            .await
            .unwrap();
        assert!(matches!(fiber.snapshot().state, FiberState::Active));
    }

    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    let both_entered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let first = entries.recv().await.expect("first cleanup entered");
        let second = entries.recv().await.expect("second cleanup entered");
        [first, second]
    })
    .await;
    release.cancel();
    assert!(shutdown.await.unwrap().is_clean());
    assert!(
        both_entered.is_ok(),
        "shutdown waited for one root before starting the next"
    );
}

#[derive(Debug)]
struct SerializedReconfigureFactory {
    descriptor: PluginDescriptor,
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[async_trait]
impl PluginFactory for SerializedReconfigureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        if !config.is_null() {
            self.entered.send(()).expect("test waiter still exists");
            self.release
                .lock()
                .expect("release receiver poisoned")
                .recv()
                .expect("test releases validation");
        }
        Ok(config)
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reconfigure_is_rejected_without_an_internal_waiter() {
    let runtime = Runtime::default();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let factory = Arc::new(SerializedReconfigureFactory {
        descriptor: PluginDescriptor::new(FactoryIdentity::builtin("serialized-reconfigure", "1")),
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    let fiber = runtime
        .root()
        .apply(factory.clone(), Value::Null)
        .await
        .unwrap();
    let first = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.reconfigure(Value::from(1)).await }
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    assert_eq!(
        fiber.reconfigure(Value::from(2)).await.unwrap_err(),
        MetaError::Busy {
            operation: "plugin reconfiguration"
        }
    );
    release_tx.send(()).unwrap();
    assert!(matches!(
        first.await.unwrap().unwrap().state,
        FiberState::Active
    ));
}

#[derive(Debug)]
struct RuntimeOwnedReconfigureFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    activations: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl PluginFactory for RuntimeOwnedReconfigureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        if !config.is_null() {
            self.entered.wait();
            self.release.wait();
        }
        Ok(config)
    }

    async fn activate(&self, _: Context, config: Arc<Value>) -> Result<()> {
        self.activations
            .lock()
            .expect("activation log poisoned")
            .push((*config).clone());
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_reconfiguration_finishes_after_the_initiating_future_is_dropped() {
    let runtime = Runtime::default();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let activations = Arc::new(Mutex::new(Vec::new()));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(RuntimeOwnedReconfigureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "runtime-owned-reconfigure",
                    "1",
                )),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                activations: Arc::clone(&activations),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let initial_generation = fiber.snapshot().generation;
    let retained_before = runtime.resource_snapshot().retained_plugin_bytes.current;
    let reconfiguration = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.reconfigure(Value::from(1)).await }
    });
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .unwrap();
    let retained_during = runtime.resource_snapshot().retained_plugin_bytes.current;
    reconfiguration.abort();
    assert!(reconfiguration.await.unwrap_err().is_cancelled());
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    assert_eq!(
        retained_during,
        retained_before + runtime.limits().payloads.maximum_config_bytes,
        "normalization must account for the staged configuration alongside the old Arc<Value>",
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = fiber.snapshot();
            if snapshot.generation != initial_generation
                && matches!(snapshot.state, FiberState::Active)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Runtime-owned reconfiguration did not converge");
    assert_eq!(
        activations
            .lock()
            .expect("activation log poisoned")
            .as_slice(),
        &[Value::Null, Value::from(1)]
    );
    let old_config_bytes = serde_json::to_vec(&Value::Null).unwrap().len();
    let new_config_bytes = serde_json::to_vec(&Value::from(1)).unwrap().len();
    let retained_after = runtime.resource_snapshot().retained_plugin_bytes;
    assert_eq!(
        retained_after.current,
        retained_before - old_config_bytes + new_config_bytes,
        "successful transfer must retain only descriptor plus the new configuration",
    );
    assert!(retained_after.high_watermark >= retained_during);
}

#[derive(Debug)]
struct BlockingCleanupFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for BlockingCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        context.defer(
            "blocking cleanup",
            Box::new(move || {
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

#[tokio::test(start_paused = true)]
async fn shutdown_wait_reports_unresolved_work_and_later_joins_completion() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            service_call: std::time::Duration::from_millis(10),
            event_dispatch: std::time::Duration::from_millis(10),
            shutdown_wait: std::time::Duration::from_millis(20),
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let fiber = runtime
        .root()
        .apply(
            Arc::new(BlockingCleanupFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "shutdown-deadline",
                    "1",
                )),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    entered.notified().await;
    tokio::time::advance(std::time::Duration::from_millis(21)).await;
    let outcome = shutdown.await.unwrap();
    let rsi_meta::ShutdownOutcome::TimedOut { report, unresolved } = outcome else {
        panic!("blocked cleanup must outlive the first waiter")
    };
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(unresolved.total, 1);
    assert_eq!(unresolved.samples[0].fiber, fiber.id());
    assert!(runtime.snapshot().terminal.is_none());
    release.notify_one();
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct PanickingDropOnlyFactory(PluginDescriptor);

impl Drop for PanickingDropOnlyFactory {
    fn drop(&mut self) {
        panic!("factory drop panic evidence");
    }
}

#[async_trait]
impl PluginFactory for PanickingDropOnlyFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn non_quiescent_shutdown_is_cached_as_failure_without_repeated_deadlines() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            shutdown_wait: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(PanickingDropOnlyFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("panicking-factory-drop", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();

    for outcome in [runtime.shutdown().await, runtime.shutdown().await] {
        let rsi_meta::ShutdownOutcome::Failed { report, unresolved } = outcome else {
            panic!("a terminal non-quiescent shutdown was not cached as Failed");
        };
        assert_eq!(report.failures().len(), 1);
        assert_eq!(unresolved.total, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn terminalization_remains_authoritative_after_quiescent_shutdown_completion() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            shutdown_wait: std::time::Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let prepared = runtime
        .prepare(
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("shutdown-terminal-fence", "1"),
            ))),
            Value::Null,
        )
        .unwrap();
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    while !runtime.snapshot().shutting_down {
        tokio::task::yield_now().await;
    }

    runtime.mark_terminal("terminal evidence after shutdown admission closure");
    assert_eq!(
        runtime.snapshot().terminal.as_deref(),
        Some("terminal evidence after shutdown admission closure")
    );
    drop(prepared);
    tokio::time::advance(std::time::Duration::from_millis(21)).await;

    assert!(shutdown.await.unwrap().is_complete());
    assert_eq!(runtime.resource_snapshot().fibers.current, 0);
    assert_eq!(
        runtime.snapshot().terminal.as_deref(),
        Some("terminal evidence after shutdown admission closure")
    );
}

#[tokio::test]
async fn cancelling_the_shutdown_initiator_cannot_strand_followers() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(BlockingCleanupFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "blocking-cleanup",
                    "1",
                )),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let leader_runtime = runtime.clone();
    let leader = tokio::spawn(async move { leader_runtime.shutdown().await });
    entered.notified().await;
    leader.abort();
    let _ = leader.await;
    let followers = (0..64)
        .map(|_| {
            let follower_runtime = runtime.clone();
            tokio::spawn(async move { follower_runtime.shutdown().await })
        })
        .collect::<Vec<_>>();
    tokio::task::yield_now().await;
    assert!(followers.iter().all(|follower| !follower.is_finished()));
    release.notify_one();
    let reports = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        futures_util::future::join_all(followers),
    )
    .await
    .expect("shutdown followers must observe the single completion notification");
    assert!(reports.into_iter().all(|report| report.unwrap().is_clean()));
}

#[derive(Debug)]
struct PanickingActivationFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PanickingActivationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        panic!("activation panic evidence")
    }
}

#[derive(Debug)]
struct HangingActivationFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    cleanups: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct BlockingReconfigurationFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    activated: Arc<Notify>,
    configurations: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl PluginFactory for BlockingReconfigurationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        if config == 1 {
            self.entered.wait();
            self.release.wait();
        }
        Ok(config)
    }

    async fn activate(&self, _: Context, config: Arc<Value>) -> Result<()> {
        self.configurations
            .lock()
            .expect("configuration log poisoned")
            .push((*config).clone());
        self.activated.notify_one();
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for HangingActivationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let cleanups = Arc::clone(&self.cleanups);
        context.defer(
            "timed-out activation rollback",
            Box::new(move || {
                let cleanups = Arc::clone(&cleanups);
                async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn activation_waiter_timeout_keeps_runtime_owned_rollback_exactly_once() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            service_call: std::time::Duration::from_millis(10),
            event_dispatch: std::time::Duration::from_millis(10),
            shutdown_wait: std::time::Duration::from_millis(20),
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let cleanups = Arc::new(AtomicUsize::new(0));
    let applying = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let cleanups = Arc::clone(&cleanups);
        async move {
            root.apply(
                Arc::new(HangingActivationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "hanging-activation",
                        "1",
                    )),
                    entered,
                    cleanups,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        applying.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if cleanups.load(Ordering::Acquire) == 1
                && runtime.snapshot().fibers.is_empty()
                && runtime.resource_snapshot().cleanup_runs.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed-out application did not finish its persistent rollback and disposal");
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn reconfiguration_waiter_timeout_leaves_one_busy_runtime_owned_transaction() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: std::time::Duration::from_millis(10),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let activated = Arc::new(Notify::new());
    let configurations = Arc::new(Mutex::new(Vec::new()));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(BlockingReconfigurationFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "timed-out-reconfiguration",
                    "1",
                )),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                activated: Arc::clone(&activated),
                configurations: Arc::clone(&configurations),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    activated.notified().await;

    let reconfiguration = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.reconfigure(Value::from(1)).await }
    });
    tokio::task::spawn_blocking({
        let entered = Arc::clone(&entered);
        move || entered.wait()
    })
    .await
    .unwrap();
    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    assert_eq!(
        reconfiguration.await.unwrap().unwrap_err(),
        MetaError::Timeout("plugin transition"),
    );
    assert_eq!(
        fiber.reconfigure(Value::from(2)).await.unwrap_err(),
        MetaError::Busy {
            operation: "plugin reconfiguration",
        },
    );

    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    activated.notified().await;
    assert_eq!(
        *configurations.lock().expect("configuration log poisoned"),
        vec![Value::Null, Value::from(1)],
    );

    let settled = loop {
        match fiber.reconfigure(Value::from(2)).await {
            Err(MetaError::Busy {
                operation: "plugin reconfiguration",
            }) => tokio::task::yield_now().await,
            result => break result,
        }
    };
    assert!(matches!(settled.unwrap().state, FiberState::Active));
    assert_eq!(
        *configurations.lock().expect("configuration log poisoned"),
        vec![Value::Null, Value::from(1), Value::from(2)],
    );
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn activation_panics_become_failed_fibers_without_poisoning_the_runtime() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(PanickingActivationFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("panicking-activation", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        fiber.snapshot().state,
        FiberState::Failed(ref error) if error.contains("panicked")
    ));
    let healthy = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("healthy-after-panic", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(healthy.snapshot().state, FiberState::Active));
}

#[derive(Debug)]
struct PanickingCleanupFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PanickingCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.defer(
            "panicking cleanup",
            Box::new(|| async move { panic!("cleanup panic evidence") }.boxed()),
        )
    }
}

#[tokio::test]
async fn cleanup_panics_become_joinable_failures() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(PanickingCleanupFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("panicking-cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let report = fiber.dispose().await;
    assert_eq!(report.failures().len(), 1);
    assert_eq!(report.failures()[0].label, "panicking cleanup");
    assert!(report.failures()[0].error.contains("panicked"));
}

#[derive(Debug)]
struct PanickingDropEndpoint;

impl Drop for PanickingDropEndpoint {
    fn drop(&mut self) {
        panic!("endpoint drop panic evidence");
    }
}

#[async_trait]
impl ServiceEndpoint for PanickingDropEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct PanickingDropFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PanickingDropFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.provide(
            "panicking-drop",
            "test.panicking-drop",
            V1,
            Arc::new(PanickingDropEndpoint),
        )
    }
}

#[tokio::test]
async fn unexpected_cleanup_driver_panics_terminalize_the_runtime() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(PanickingDropFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("panicking-drop", "1"))
                    .providing(Provision::new("panicking-drop", "test.panicking-drop", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();

    let report = fiber.dispose().await;
    assert_eq!(report.failures().len(), 1);
    assert!(report.failures()[0].error.contains("cleanup run panicked"));
    assert_eq!(
        runtime.snapshot().terminal.as_deref(),
        Some("runtime cleanup driver panicked")
    );
}

#[derive(Debug)]
struct DropFactory {
    descriptor: PluginDescriptor,
    dropped: Arc<AtomicUsize>,
}

impl Drop for DropFactory {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl PluginFactory for DropFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn dropping_all_public_owners_releases_registered_fibers() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::default();
    let handle = runtime
        .root()
        .apply(
            Arc::new(DropFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("drop-probe", "1")),
                dropped: Arc::clone(&dropped),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    drop(runtime);
    drop(handle);
    assert_eq!(dropped.load(Ordering::Acquire), 1);
}

#[derive(Debug)]
struct FailingCleanupFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for FailingCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.defer(
            "forced failure",
            Box::new(|| async { Err("cleanup evidence".to_owned()) }.boxed()),
        )
    }
}

#[derive(Debug)]
struct CancelledDisposalFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: CancellationToken,
    cleaned: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for CancelledDisposalFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        let release = self.release.clone();
        let cleaned = Arc::clone(&self.cleaned);
        context.defer(
            "cancelled disposal",
            Box::new(move || {
                async move {
                    entered.notify_one();
                    release.cancelled().await;
                    cleaned.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn cancelling_public_dispose_does_not_cancel_runtime_owned_cleanup() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let cleaned = Arc::new(AtomicUsize::new(0));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(CancelledDisposalFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "cancelled-disposal",
                    "1",
                )),
                entered: Arc::clone(&entered),
                release: release.clone(),
                cleaned: Arc::clone(&cleaned),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let cancelled = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.dispose().await }
    });
    entered.notified().await;
    cancelled.abort();
    let _ = cancelled.await;
    release.cancel();

    let report = tokio::time::timeout(std::time::Duration::from_secs(1), fiber.dispose())
        .await
        .expect("a later disposer did not join cleanup");
    assert!(report.is_clean());
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
    assert!(matches!(fiber.snapshot().state, FiberState::Disposed));
    assert!(runtime.snapshot().fibers.is_empty());
}

#[tokio::test]
async fn dispose_and_shutdown_are_joinable_and_preserve_cleanup_reports() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            Arc::new(FailingCleanupFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let (first, second) = tokio::join!(fiber.dispose(), fiber.dispose());
    assert_eq!(first, second);
    assert_eq!(first.failures()[0].label, "forced failure");
    assert!(matches!(fiber.snapshot().state, FiberState::Disposed));

    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(FailingCleanupFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("shutdown-cleanup", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
    assert_eq!(first, second);
    assert_eq!(first.report().failures().len(), 1);
    assert!(matches!(
        runtime
            .root()
            .apply(
                Arc::new(PassiveFactory(PluginDescriptor::new(
                    FactoryIdentity::builtin("late", "1")
                ))),
                Value::Null,
            )
            .await,
        Err(MetaError::RuntimeShuttingDown)
    ));
}

#[path = "lifecycle_invariants/contract_invariants.rs"]
mod contract_invariants;
#[path = "lifecycle_invariants/foundation.rs"]
mod foundation;
