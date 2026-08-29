use super::*;

#[derive(Debug)]
struct PreparedInjectionFactory {
    prepare_count: Arc<AtomicUsize>,
    activation_count: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for PreparedInjectionFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.prepare_count.fetch_add(1, Ordering::AcqRel);
        Ok(
            PreparedActivation::new(config.clone()).requiring(Requirement::new(
                "prepared-dependency",
                "test.prepared",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        assert!(plan.inject("prepared-dependency").is_some());
        self.activation_count.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[tokio::test]
async fn prepared_injection_waits_for_an_actual_active_supply() {
    let runtime = Runtime::default();
    let prepare_count = Arc::new(AtomicUsize::new(0));
    let activation_count = Arc::new(AtomicUsize::new(0));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(PreparedInjectionFactory {
                prepare_count: Arc::clone(&prepare_count),
                activation_count: Arc::clone(&activation_count),
            })),
            Value::Null,
        )
        .await
        .unwrap();

    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert_eq!(prepare_count.load(Ordering::Acquire), 1);
    assert_eq!(activation_count.load(Ordering::Acquire), 0);

    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(EndpointFactory::new(
                FactoryIdentity::linked("prepared-provider", "1"),
                "prepared-dependency",
                "test.prepared",
                V1,
                Arc::new(Echo),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;

    assert_eq!(prepare_count.load(Ordering::Acquire), 1);
    assert_eq!(activation_count.load(Ordering::Acquire), 1);
    assert!(provider.dispose().await.is_clean());
    assert!(consumer.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct ConfigSelectedInjectionFactory {
    prepare_count: Arc<AtomicUsize>,
    providers: Arc<Mutex<Vec<(rsi_meta::FiberId, rsi_meta::FiberGeneration)>>>,
}

#[async_trait]
impl PluginFactory for ConfigSelectedInjectionFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.prepare_count.fetch_add(1, Ordering::AcqRel);
        let key = config
            .as_str()
            .ok_or_else(|| MetaError::InvalidConfig("expected a service key".to_owned()))?;
        Ok(
            PreparedActivation::new(config.clone()).requiring(Requirement::new(
                key,
                "test.prepared-config",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let key = plan
            .config()
            .as_str()
            .expect("prepared config remains a service key");
        self.providers
            .lock()
            .expect("provider observations poisoned")
            .push(
                plan.inject(key)
                    .expect("prepared requirement is injected exactly")
                    .provider(),
            );
        Ok(())
    }
}

#[tokio::test]
async fn reconfigure_retains_the_fresh_prepared_injection_and_updates_dependents() {
    let runtime = Runtime::default();
    let provider_a = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(EndpointFactory::new(
                FactoryIdentity::linked("provider-a", "1"),
                "attempt-a",
                "test.prepared-config",
                V1,
                Arc::new(Echo),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let prepare_count = Arc::new(AtomicUsize::new(0));
    let providers = Arc::new(Mutex::new(Vec::new()));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ConfigSelectedInjectionFactory {
                prepare_count: Arc::clone(&prepare_count),
                providers: Arc::clone(&providers),
            })),
            Value::String("attempt-a".to_owned()),
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;
    assert_eq!(prepare_count.load(Ordering::Acquire), 1);
    assert_eq!(
        providers.lock().expect("provider observations poisoned")[0],
        (provider_a.id(), provider_a.snapshot().generation)
    );

    let pending = consumer
        .reconfigure(Value::String("attempt-b".to_owned()))
        .await
        .unwrap();
    assert!(matches!(pending.state, FiberState::Pending(_)));
    assert_eq!(prepare_count.load(Ordering::Acquire), 2);
    assert_eq!(
        providers
            .lock()
            .expect("provider observations poisoned")
            .len(),
        1,
        "the old injection must not activate the replacement config"
    );

    let provider_b = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(EndpointFactory::new(
                FactoryIdentity::linked("provider-b", "1"),
                "attempt-b",
                "test.prepared-config",
                V1,
                Arc::new(Echo),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;
    assert_eq!(prepare_count.load(Ordering::Acquire), 2);
    assert_eq!(
        providers.lock().expect("provider observations poisoned")[1],
        (provider_b.id(), provider_b.snapshot().generation)
    );

    assert!(consumer.dispose().await.is_clean());
    assert!(provider_b.dispose().await.is_clean());
    assert!(provider_a.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
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

#[derive(Debug)]
struct ReplaceableProviderFactory {
    spec: FactorySpec,
    context: Arc<Mutex<Option<Context>>>,
    supply: Arc<Mutex<Option<SupplyHandle>>>,
}

#[async_trait]
impl PluginFactory for ReplaceableProviderFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        let supply = context.provide(
            "fenced-attempt",
            "test.fenced-attempt",
            V1,
            Arc::new(TaggedEndpoint(b"old")),
        )?;
        *self.context.lock().expect("provider Context poisoned") = Some(context);
        *self.supply.lock().expect("provider supply poisoned") = Some(supply);
        Ok(())
    }
}

#[derive(Debug)]
struct BindingFencedAttemptFactory {
    prepare_count: Arc<AtomicUsize>,
    activation_count: Arc<AtomicUsize>,
    first_entered: Arc<Notify>,
    responses: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[async_trait]
impl PluginFactory for BindingFencedAttemptFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if *config != json!({ "raw": "desired" }) {
            return Err(MetaError::InvalidConfig(
                "preparation did not receive the immutable raw desired config".to_owned(),
            ));
        }
        let attempt = self.prepare_count.fetch_add(1, Ordering::AcqRel) + 1;
        Ok(
            PreparedActivation::new(json!({ "normalized_attempt": attempt })).requiring(
                Requirement::new("fenced-attempt", "test.fenced-attempt", V1),
            ),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let activation = self.activation_count.load(Ordering::Acquire) + 1;
        assert_eq!(
            plan.config()
                .get("normalized_attempt")
                .and_then(Value::as_u64),
            Some(activation as u64),
            "each fresh activation consumes that attempt's normalized output"
        );
        let response = plan
            .inject("fenced-attempt")
            .expect("fenced attempt has its exact injection")
            .clone()
            .invoke(Message::new(Vec::new()))
            .await?;
        self.responses
            .lock()
            .expect("activation responses poisoned")
            .push(response.into_parts().0);
        if self.activation_count.fetch_add(1, Ordering::AcqRel) == 0 {
            self.first_entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[tokio::test]
async fn binding_change_fences_loading_and_freshly_prepares_the_replacement_attempt() {
    let runtime = Runtime::default();
    let provider_context = Arc::new(Mutex::new(None));
    let provider_supply = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ReplaceableProviderFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("replaceable-provider", "1")),
                context: Arc::clone(&provider_context),
                supply: Arc::clone(&provider_supply),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;

    let prepare_count = Arc::new(AtomicUsize::new(0));
    let activation_count = Arc::new(AtomicUsize::new(0));
    let first_entered = Arc::new(Notify::new());
    let responses = Arc::new(Mutex::new(Vec::new()));
    let application = tokio::spawn({
        let root = runtime.root();
        let prepare_count = Arc::clone(&prepare_count);
        let activation_count = Arc::clone(&activation_count);
        let first_entered = Arc::clone(&first_entered);
        let responses = Arc::clone(&responses);
        async move {
            root.apply(
                crate::resolved(Arc::new(BindingFencedAttemptFactory {
                    prepare_count,
                    activation_count,
                    first_entered,
                    responses,
                })),
                json!({ "raw": "desired" }),
            )
            .await
        }
    });
    first_entered.notified().await;
    assert_eq!(prepare_count.load(Ordering::Acquire), 1);

    let first_supply = provider_supply
        .lock()
        .expect("provider supply poisoned")
        .clone()
        .expect("provider captured its first supply");
    assert!(first_supply.dispose().await.is_clean());
    let replacement = provider_context
        .lock()
        .expect("provider Context poisoned")
        .clone()
        .expect("provider captured its Context")
        .provide(
            "fenced-attempt",
            "test.fenced-attempt",
            V1,
            Arc::new(TaggedEndpoint(b"new")),
        )
        .unwrap();

    let consumer = application.await.unwrap().unwrap();
    support::wait_active(&consumer).await;
    assert_eq!(prepare_count.load(Ordering::Acquire), 2);
    assert_eq!(activation_count.load(Ordering::Acquire), 2);
    assert_eq!(
        *responses.lock().expect("activation responses poisoned"),
        vec![b"old".to_vec(), b"new".to_vec()]
    );

    assert!(consumer.dispose().await.is_clean());
    assert!(replacement.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct ActiveBindingReplacementFactory {
    prepare_count: AtomicUsize,
    activation_count: Arc<AtomicUsize>,
    replacement_prepare_entered: Arc<Barrier>,
    replacement_prepare_release: Arc<Barrier>,
    cleanups: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct PreparationPressureConsumer {
    preparations: Arc<AtomicUsize>,
    activations: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for PreparationPressureConsumer {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.preparations.fetch_add(1, Ordering::AcqRel);
        Ok(
            PreparedActivation::new(config.clone()).requiring(Requirement::new(
                "fenced-attempt",
                "test.fenced-attempt",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        assert!(plan.inject("fenced-attempt").is_some());
        self.activations.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[derive(Debug)]
struct PreparationSlotBlocker {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[async_trait]
impl PluginFactory for PreparationSlotBlocker {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.entered.send(()).expect("test waits for preparation");
        self.release
            .lock()
            .expect("preparation release poisoned")
            .recv()
            .expect("test releases preparation");
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

async fn occupy_preparation_slot(
    runtime: &Runtime,
) -> (
    std::sync::mpsc::SyncSender<()>,
    std::thread::JoinHandle<Result<rsi_meta::PreparedPlugin>>,
) {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let preparing_runtime = runtime.clone();
    let blocker = std::thread::spawn(move || {
        preparing_runtime.prepare(
            crate::resolved(Arc::new(PreparationSlotBlocker {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            })),
            Value::Null,
        )
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    (release_tx, blocker)
}

async fn wait_for_service_release(runtime: &Runtime) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while runtime.resource_snapshot().services.current != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("withdrawal did not remove the old service");
}

#[tokio::test(start_paused = true)]
async fn automatic_binding_refresh_waits_for_transient_preparation_capacity() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: rsi_meta::ExecutionLimits {
            maximum_concurrent_preparations: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let provider_context = Arc::new(Mutex::new(None));
    let provider_supply = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ReplaceableProviderFactory {
                spec: FactorySpec::new(FactoryIdentity::linked(
                    "preparation-pressure-provider",
                    "1",
                )),
                context: Arc::clone(&provider_context),
                supply: Arc::clone(&provider_supply),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;

    let preparations = Arc::new(AtomicUsize::new(0));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(PreparationPressureConsumer {
                preparations: Arc::clone(&preparations),
                activations: Arc::clone(&activations),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;

    let (release_tx, blocker) = occupy_preparation_slot(&runtime).await;
    assert_eq!(runtime.resource_snapshot().preparations.current, 1);

    let old_supply = provider_supply
        .lock()
        .expect("provider supply poisoned")
        .take()
        .expect("provider captured its first supply");
    let mut withdrawal = tokio::spawn(async move { old_supply.dispose().await });
    wait_for_service_release(&runtime).await;

    // Under the faulty fail-fast path this completes with a Busy failure.
    // Under the correct path virtual time advances while reconciliation waits
    // without retaining its global slot.
    let withdrawal_before_capacity = tokio::select! {
        result = &mut withdrawal => Some(result.expect("withdrawal task remained healthy")),
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => None,
    };
    let mut consumer_updates = consumer.subscribe();
    let replacement = provider_context
        .lock()
        .expect("provider Context poisoned")
        .clone()
        .expect("provider captured its Context")
        .provide(
            "fenced-attempt",
            "test.fenced-attempt",
            V1,
            Arc::new(TaggedEndpoint(b"replacement")),
        )
        .unwrap();
    let state_before_capacity = tokio::select! {
        changed = consumer_updates.changed() => {
            changed.expect("consumer watch stays open");
            Some(consumer_updates.borrow().state.clone())
        }
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => None,
    };

    release_tx.send(()).unwrap();
    let prepared = tokio::task::spawn_blocking(move || blocker.join().unwrap())
        .await
        .unwrap()
        .unwrap();
    drop(prepared);
    let withdrawal_report = match withdrawal_before_capacity {
        Some(report) => report,
        None => withdrawal
            .await
            .expect("withdrawal task remained healthy after capacity returned"),
    };
    assert!(withdrawal_report.is_clean());

    let recovery = consumer.wait_active(&CancellationToken::new()).await;
    let resources_after_capacity = runtime.resource_snapshot();
    let state_after_capacity = consumer.snapshot().state;
    assert!(consumer.dispose().await.is_clean());
    assert!(replacement.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());

    assert!(
        recovery.is_ok(),
        "transient preparation pressure terminalized the consumer: before={state_before_capacity:?}, after={state_after_capacity:?}, preparations={:?}",
        resources_after_capacity.preparations,
    );
    assert_eq!(preparations.load(Ordering::Acquire), 2);
    assert_eq!(activations.load(Ordering::Acquire), 2);
}

#[async_trait]
impl PluginFactory for ActiveBindingReplacementFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if self.prepare_count.fetch_add(1, Ordering::AcqRel) == 1 {
            self.replacement_prepare_entered.wait();
            self.replacement_prepare_release.wait();
        }
        Ok(
            PreparedActivation::new(config.clone()).requiring(Requirement::new(
                "fenced-attempt",
                "test.fenced-attempt",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        assert!(plan.inject("fenced-attempt").is_some());
        self.activation_count.fetch_add(1, Ordering::AcqRel);
        let cleanups = Arc::clone(&self.cleanups);
        plan.defer(
            "observe active binding replacement",
            Box::new(move || {
                Box::pin(async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_binding_change_prepares_before_retirement_and_reuses_no_attempt() {
    let runtime = Runtime::default();
    let provider_context = Arc::new(Mutex::new(None));
    let provider_supply = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ReplaceableProviderFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("active-replacement-provider", "1")),
                context: Arc::clone(&provider_context),
                supply: Arc::clone(&provider_supply),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;

    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let activation_count = Arc::new(AtomicUsize::new(0));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ActiveBindingReplacementFactory {
                prepare_count: AtomicUsize::new(0),
                activation_count: Arc::clone(&activation_count),
                replacement_prepare_entered: Arc::clone(&entered),
                replacement_prepare_release: Arc::clone(&release),
                cleanups: Arc::clone(&cleanups),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;

    let first_supply = provider_supply
        .lock()
        .expect("provider supply poisoned")
        .clone()
        .expect("provider captured its first supply");
    let withdrawal = tokio::spawn(async move { first_supply.dispose().await });
    tokio::task::spawn_blocking({
        let entered = Arc::clone(&entered);
        move || entered.wait()
    })
    .await
    .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    assert!(withdrawal.await.unwrap().is_clean());
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert_eq!(cleanups.load(Ordering::Acquire), 1);

    let replacement = provider_context
        .lock()
        .expect("provider Context poisoned")
        .clone()
        .expect("provider captured its Context")
        .provide(
            "fenced-attempt",
            "test.fenced-attempt",
            V1,
            Arc::new(TaggedEndpoint(b"replacement")),
        )
        .unwrap();
    support::wait_active(&consumer).await;
    assert_eq!(activation_count.load(Ordering::Acquire), 2);

    assert!(consumer.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 2);
    assert!(replacement.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct RejectingBindingReplacementFactory {
    prepare_count: AtomicUsize,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for RejectingBindingReplacementFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if self.prepare_count.fetch_add(1, Ordering::AcqRel) != 0 {
            return Err(MetaError::InvalidConfig(
                "fresh binding attempt was rejected".to_owned(),
            ));
        }
        Ok(
            PreparedActivation::new(config.clone()).requiring(Requirement::new(
                "fenced-attempt",
                "test.fenced-attempt",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        assert!(plan.inject("fenced-attempt").is_some());
        let cleanups = Arc::clone(&self.cleanups);
        plan.defer(
            "observe rejected binding replacement retirement",
            Box::new(move || {
                Box::pin(async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn invalidated_active_binding_and_rejected_fresh_prepare_settles_failed() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(EndpointFactory::new(
                FactoryIdentity::linked("rejected-binding-provider", "1"),
                "fenced-attempt",
                "test.fenced-attempt",
                V1,
                Arc::new(Echo),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RejectingBindingReplacementFactory {
                prepare_count: AtomicUsize::new(0),
                cleanups: Arc::clone(&cleanups),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;

    assert!(provider.dispose().await.is_clean());
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), consumer.wait_settled())
        .await
        .expect("replacement preparation failure must settle");
    assert!(matches!(
        settled.state,
        FiberState::Failed(ref error) if error.contains("fresh binding attempt was rejected")
    ));
    assert_eq!(cleanups.load(Ordering::Acquire), 1);

    assert!(consumer.dispose().await.is_clean());
    assert_eq!(
        cleanups.load(Ordering::Acquire),
        1,
        "the failed Fiber has no active generation left to retire twice"
    );
    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);
}

#[derive(Debug)]
struct RejectingReconfigurationFactory {
    cleanups: Arc<AtomicUsize>,
    context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for RejectingReconfigurationFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if *config == Value::String("reject".to_owned()) {
            return Err(MetaError::InvalidConfig(
                "replacement configuration was rejected".to_owned(),
            ));
        }
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().provide(
            "stable-after-reconfigure-failure",
            "test.reconfigure-failure",
            V1,
            Arc::new(Echo),
        )?;
        *self.context.lock().expect("captured Context poisoned") = Some(plan.context().clone());
        let cleanups = Arc::clone(&self.cleanups);
        plan.defer(
            "observe replacement retirement",
            Box::new(move || {
                Box::pin(async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
            }),
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn rejected_reconfigure_keeps_the_existing_generation_active() {
    let runtime = Runtime::default();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let context = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(RejectingReconfigurationFactory {
                cleanups: Arc::clone(&cleanups),
                context: Arc::clone(&context),
            })),
            Value::String("accepted".to_owned()),
        )
        .await
        .unwrap();
    support::wait_active(&fiber).await;
    let before = fiber.snapshot();

    let error = fiber
        .reconfigure(Value::String("reject".to_owned()))
        .await
        .unwrap_err();
    assert!(matches!(error, MetaError::InvalidConfig(_)));
    let after = fiber.snapshot();
    assert!(matches!(after.state, FiberState::Active));
    assert_eq!(after.generation, before.generation);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    let active_context = context
        .lock()
        .expect("captured Context poisoned")
        .clone()
        .expect("active generation captured its Context");
    let response = active_context
        .service("stable-after-reconfigure-failure")
        .unwrap()
        .invoke(Message::new(b"still active".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.into_parts().0, b"still active");

    assert!(fiber.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct BlockingReplacementPreparationFactory {
    prepare_count: AtomicUsize,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingReplacementPreparationFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if self.prepare_count.fetch_add(1, Ordering::AcqRel) == 1 {
            self.entered.wait();
            self.release.wait();
        }
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let cleanups = Arc::clone(&self.cleanups);
        plan.defer(
            "observe prepared replacement ordering",
            Box::new(move || {
                Box::pin(async move {
                    cleanups.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_preparation_retains_the_active_generation_and_accounts_both_payloads() {
    let runtime = Runtime::default();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let fiber = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(BlockingReplacementPreparationFactory {
                prepare_count: AtomicUsize::new(0),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                cleanups: Arc::clone(&cleanups),
            })),
            Value::String("old desired payload".to_owned()),
        )
        .await
        .unwrap();
    support::wait_active(&fiber).await;
    let active = fiber.snapshot();
    let retained_before = runtime.resource_snapshot().retained_plugin_bytes.current;

    let reconfiguration = tokio::spawn({
        let fiber = fiber.clone();
        async move {
            fiber
                .reconfigure(Value::String("new desired payload".to_owned()))
                .await
        }
    });
    tokio::task::spawn_blocking({
        let entered = Arc::clone(&entered);
        move || entered.wait()
    })
    .await
    .unwrap();

    let during = fiber.snapshot();
    assert!(matches!(during.state, FiberState::Active));
    assert_eq!(during.generation, active.generation);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);
    assert!(
        runtime.resource_snapshot().retained_plugin_bytes.current > retained_before,
        "raw desired and normalized replacement reservations coexist with the active attempt"
    );

    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();
    let replaced = reconfiguration.await.unwrap().unwrap();
    assert!(matches!(replaced.state, FiberState::Active));
    assert_ne!(replaced.generation, active.generation);
    assert_eq!(cleanups.load(Ordering::Acquire), 1);

    assert!(fiber.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 2);
    assert!(runtime.shutdown().await.is_complete());
}
