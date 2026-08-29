use super::*;

#[tokio::test]
async fn cancelling_apply_before_handle_acknowledgement_disposes_the_active_fiber() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let mut application = Box::pin(root.apply(
        crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
            FactoryIdentity::linked("unacknowledged-apply", "1"),
        )))),
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
struct DynamicListenerDropFactory {
    spec: FactorySpec,
    context: Arc<Mutex<Option<Context>>>,
    dropped: Arc<AtomicUsize>,
}

impl Drop for DynamicListenerDropFactory {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl PluginFactory for DynamicListenerDropFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

#[tokio::test]
async fn dormant_dynamic_listener_does_not_retain_the_last_runtime_owner() {
    let runtime = Runtime::default();
    let context = Arc::new(Mutex::new(None));
    let dropped = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(DynamicListenerDropFactory {
        spec: FactorySpec::new(FactoryIdentity::linked("dynamic-listener-drop", "1")),
        context: Arc::clone(&context),
        dropped: Arc::clone(&dropped),
    });
    let factory_weak = Arc::downgrade(&factory);
    let fiber = runtime
        .root()
        .apply(crate::resolved(factory.clone()), Value::Null)
        .await
        .unwrap();
    let context = context
        .lock()
        .expect("context capture poisoned")
        .take()
        .expect("activation captured its Context");
    let listener = context
        .on_emit::<NoopEvent, _>(Arc::new(NoopHandler), LocalEventOptions::default())
        .unwrap();

    drop(listener);
    drop(context);
    drop(factory);
    drop(runtime);
    drop(fiber);

    assert!(
        factory_weak.upgrade().is_none(),
        "the Runtime-owned listener cleanup formed a last-owner cycle"
    );
    assert_eq!(dropped.load(Ordering::Acquire), 1);
}

#[derive(Debug)]
struct TerminalizedActivationFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for TerminalizedActivationFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        context.on_emit::<NoopEvent, _>(Arc::new(NoopHandler), LocalEventOptions::default())?;
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
                crate::resolved(Arc::new(TerminalizedActivationFactory {
                    spec: FactorySpec::new(FactoryIdentity::linked("terminalized-activation", "1")),
                    entered,
                    release,
                })),
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
struct BlockingPreparationFactory {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

#[async_trait]
impl PluginFactory for BlockingPreparationFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.entered.wait();
        self.release.wait();
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_linearizes_with_apply_after_arbitrary_preparation() {
    let runtime = Runtime::default();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let root = runtime.root();
    let application = tokio::spawn({
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                crate::resolved(Arc::new(BlockingPreparationFactory { entered, release })),
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
    let mut shutdown = Box::pin(runtime.shutdown());
    assert!(
        matches!(futures_util::poll!(&mut shutdown), Poll::Pending),
        "shutdown completed before pre-close preparation released its admission lease",
    );
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    assert!(matches!(
        application.await.unwrap(),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert!(shutdown.await.is_clean());
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
                crate::resolved(Arc::new(BlockingPreparationFactory { entered, release })),
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
    spec: FactorySpec,
    label: &'static str,
    entered: tokio::sync::mpsc::UnboundedSender<&'static str>,
    release: CancellationToken,
}

#[async_trait]
impl PluginFactory for BlockingRootCleanupFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
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

#[derive(Debug)]
struct ExecutorPinnedCleanupFactory {
    spec: FactorySpec,
    entered: Arc<AtomicBool>,
    release: CancellationToken,
}

#[async_trait]
impl PluginFactory for ExecutorPinnedCleanupFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let entered = Arc::clone(&self.entered);
        let release = self.release.clone();
        plan.context().defer(
            "executor-pinned-cleanup",
            Box::new(move || {
                async move {
                    entered.store(true, Ordering::Release);
                    release.cancelled().await;
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_survives_the_initiating_executor_being_dropped() {
    let runtime = Runtime::default();
    let entered = Arc::new(AtomicBool::new(false));
    let release = CancellationToken::new();
    let fiber = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ExecutorPinnedCleanupFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("executor-pinned-cleanup", "1")),
                entered: Arc::clone(&entered),
                release: release.clone(),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let initiating_fiber = fiber.clone();
    let initiating_entered = Arc::clone(&entered);
    std::thread::spawn(move || {
        let transient = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        transient.block_on(async move {
            let mut disposal = Box::pin(initiating_fiber.dispose());
            loop {
                assert!(matches!(futures_util::poll!(&mut disposal), Poll::Pending));
                if initiating_entered.load(Ordering::Acquire) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });
    })
    .join()
    .unwrap();

    release.cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(1), fiber.dispose())
        .await
        .expect("Runtime-owned cleanup was stranded with the initiating executor");
    assert!(report.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_survives_the_initiating_executor_being_dropped() {
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            shutdown_wait: std::time::Duration::from_millis(100),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let release = CancellationToken::new();
    runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ExecutorPinnedCleanupFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("executor-pinned-shutdown", "1")),
                entered: Arc::clone(&entered),
                release: release.clone(),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let initiating_runtime = runtime.clone();
    let initiating_entered = Arc::clone(&entered);
    std::thread::spawn(move || {
        let transient = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        transient.block_on(async move {
            let mut shutdown = Box::pin(initiating_runtime.shutdown());
            loop {
                assert!(matches!(futures_util::poll!(&mut shutdown), Poll::Pending));
                if initiating_entered.load(Ordering::Acquire) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });
    })
    .join()
    .unwrap();

    release.cancel();
    assert!(
        runtime.shutdown().await.is_complete(),
        "Runtime-owned shutdown was stranded with the initiating executor"
    );
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
                crate::resolved(Arc::new(BlockingRootCleanupFactory {
                    spec: FactorySpec::new(FactoryIdentity::linked(label, "1")),
                    label,
                    entered: entered.clone(),
                    release: release.clone(),
                })),
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
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[async_trait]
impl PluginFactory for SerializedReconfigureFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if !config.is_null() {
            self.entered.send(()).expect("test waiter still exists");
            self.release
                .lock()
                .expect("release receiver poisoned")
                .recv()
                .expect("test releases preparation");
        }
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reconfigure_is_rejected_without_an_internal_waiter() {
    let runtime = Runtime::default();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let factory = Arc::new(SerializedReconfigureFactory {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    let fiber = runtime
        .root()
        .apply(crate::resolved(factory.clone()), Value::Null)
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
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    activations: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl PluginFactory for RuntimeOwnedReconfigureFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if !config.is_null() {
            self.entered.wait();
            self.release.wait();
        }
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let config = Arc::clone(plan.config());
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
            crate::resolved(Arc::new(RuntimeOwnedReconfigureFactory {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                activations: Arc::clone(&activations),
            })),
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
        retained_before
            + runtime.limits().payloads.maximum_config_bytes
            + runtime.limits().payloads.maximum_prepared_state_bytes
            + runtime.limits().topology.maximum_requirements_per_fiber
                * (runtime.limits().payloads.maximum_identifier_bytes * 2
                    + std::mem::size_of::<ContractVersion>())
            + 1,
        "preparation must account for the raw desired byte and the complete config, state, and requirement reservation alongside the active attempt",
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
        retained_before - (old_config_bytes * 2) + (new_config_bytes * 2),
        "successful transfer retains the new raw desired and normalized attempt, and releases both old values",
    );
    assert!(retained_after.high_watermark >= retained_during);
}

#[derive(Debug)]
struct BlockingCleanupFactory {
    spec: FactorySpec,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for BlockingCleanupFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
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
            crate::resolved(Arc::new(BlockingCleanupFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("shutdown-deadline", "1")),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            })),
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
struct PanickingDropOnlyFactory(FactorySpec);

impl Drop for PanickingDropOnlyFactory {
    fn drop(&mut self) {
        panic!("factory drop panic evidence");
    }
}

#[async_trait]
impl PluginFactory for PanickingDropOnlyFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn factory_destructor_panic_is_contained_and_shutdown_completion_is_cached() {
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
            crate::resolved(Arc::new(PanickingDropOnlyFactory(FactorySpec::new(
                FactoryIdentity::linked("panicking-factory-drop", "1"),
            )))),
            Value::Null,
        )
        .await
        .unwrap();

    for outcome in [runtime.shutdown().await, runtime.shutdown().await] {
        let rsi_meta::ShutdownOutcome::Complete(report) = outcome else {
            panic!("a contained factory destructor panic prevented quiescent shutdown");
        };
        assert!(report.is_clean());
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
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("shutdown-terminal-fence", "1"),
            )))),
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
            crate::resolved(Arc::new(BlockingCleanupFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("blocking-cleanup", "1")),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            })),
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
