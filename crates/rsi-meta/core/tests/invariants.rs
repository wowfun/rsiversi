use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    CallId, CleanupReport, Context, ContractVersion, DispatchMode, EventHandler, EventListenerId,
    EventOptions, EventOutcome, FactoryIdentity, FiberState, InvocationContext, MetaError,
    PluginDescriptor, PluginFactory, ProviderChannel, Provision, Requirement, Result, Runtime,
    RuntimeLimits, ServiceEndpoint, ServiceFrame, ServiceHandle,
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::Poll;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct PassiveFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PassiveFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn runtime_rejects_every_zero_limit_and_short_shutdown_deadline() {
    let zero = std::time::Duration::ZERO;
    let invalid_limits = [
        RuntimeLimits {
            maximum_fibers: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_services: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_event_listeners: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_effects_per_fiber: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_frame_bytes: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_config_bytes: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_concurrent_reconciliations: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            maximum_concurrent_service_calls: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            channel_capacity: 0,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            transition_timeout: zero,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            service_call_timeout: zero,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            event_callback_timeout: zero,
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            shutdown_timeout: zero,
            ..RuntimeLimits::default()
        },
    ];
    for limits in invalid_limits {
        assert!(matches!(
            Runtime::new(limits),
            Err(MetaError::InvalidInput(_))
        ));
    }
    assert!(
        Runtime::new(RuntimeLimits {
            service_call_timeout: std::time::Duration::from_secs(2),
            shutdown_timeout: std::time::Duration::from_secs(1),
            ..RuntimeLimits::default()
        })
        .is_err()
    );
}

#[tokio::test]
async fn limits_duplicates_contract_mismatches_and_wait_cancellation_fail_closed() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_fibers: 1,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let duplicate = PluginDescriptor::new(FactoryIdentity::builtin("duplicate", "1"))
        .requiring(Requirement::new("same", "one", V1))
        .requiring(Requirement::new("same", "two", V1));
    assert!(matches!(
        runtime
            .root()
            .apply(Arc::new(PassiveFactory(duplicate)), Value::Null)
            .await,
        Err(MetaError::InvalidInput(_))
    ));

    let pending = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("consumer", "1"))
                    .requiring(Requirement::new("missing", "expected", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        runtime
            .root()
            .apply(
                Arc::new(PassiveFactory(PluginDescriptor::new(
                    FactoryIdentity::builtin("over-capacity", "1")
                ))),
                Value::Null,
            )
            .await,
        Err(MetaError::CapacityExhausted { resource: "fibers" })
    ));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        pending.wait_active(&cancellation).await.unwrap_err(),
        MetaError::Cancelled
    );

    let mismatch_runtime = Runtime::default();
    let provider = mismatch_runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("provider", "1"))
                    .providing(Provision::new("slot", "actual", V1)),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(provider.snapshot().state, FiberState::Active));
    let consumer = mismatch_runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("mismatch", "1"))
                    .requiring(Requirement::new("slot", "expected", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        consumer.snapshot().state,
        FiberState::Pending(ref reasons)
            if reasons.iter().any(|reason| matches!(
                reason,
                rsi_meta::PendingReason::ContractMismatch { .. }
            ))
    ));
}

#[derive(Debug)]
struct ExpandingConfigFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for ExpandingConfigFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    fn validate_config(&self, _: Value) -> Result<Value> {
        Ok(Value::String("x".repeat(64)))
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct PanickingConfigFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PanickingConfigFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        if config.is_null() {
            Ok(config)
        } else {
            panic!("configuration validation panic")
        }
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn plugin_config_is_bounded_before_and_after_normalization() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_config_bytes: 32,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let descriptor =
        || PluginDescriptor::new(FactoryIdentity::builtin("bounded-configuration", "1"));

    assert!(matches!(
        runtime.prepare(
            Arc::new(PassiveFactory(descriptor())),
            Value::String("x".repeat(64)),
        ),
        Err(MetaError::InvalidConfig(_))
    ));
    assert!(matches!(
        runtime.prepare(Arc::new(ExpandingConfigFactory(descriptor())), Value::Null,),
        Err(MetaError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn config_validation_panics_have_one_error_classification() {
    let runtime = Runtime::default();
    let factory = Arc::new(PanickingConfigFactory(PluginDescriptor::new(
        FactoryIdentity::builtin("panicking-configuration", "1"),
    )));
    assert!(matches!(
        runtime.prepare(factory.clone(), Value::from(1)),
        Err(MetaError::InvalidConfig(_))
    ));

    let fiber = runtime.root().apply(factory, Value::Null).await.unwrap();
    assert!(matches!(
        fiber.reconfigure(Value::from(1)).await,
        Err(MetaError::InvalidConfig(_))
    ));
}

#[derive(Debug)]
struct QuotaFactory {
    descriptor: PluginDescriptor,
    effects: usize,
}

#[async_trait]
impl PluginFactory for QuotaFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        for provision in &self.descriptor.provides {
            context.provide(
                provision.key.clone(),
                provision.contract.clone(),
                provision.version,
                Arc::new(Echo),
            )?;
        }
        for index in 0..self.effects {
            context.defer(
                format!("effect-{index}"),
                Box::new(|| async move { Ok(()) }.boxed()),
            )?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn service_and_effect_quotas_fail_closed_at_their_owning_seams() {
    let service_runtime = Runtime::new(RuntimeLimits {
        maximum_services: 1,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let services = service_runtime
        .root()
        .apply(
            Arc::new(QuotaFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("service-quota", "1"))
                    .providing(Provision::new("first", "test.first", V1))
                    .providing(Provision::new("second", "test.second", V1)),
                effects: 0,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        services.snapshot().state,
        FiberState::Failed(ref error)
            if error.contains("bounded runtime capacity exhausted: services")
    ));

    let effect_runtime = Runtime::new(RuntimeLimits {
        maximum_effects_per_fiber: 1,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let effects = effect_runtime
        .root()
        .apply(
            Arc::new(QuotaFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("effect-quota", "1")),
                effects: 2,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        effects.snapshot().state,
        FiberState::Failed(ref error)
            if error.contains("bounded runtime capacity exhausted: effects")
    ));
}

#[derive(Debug)]
struct BlockingDeclaredProviderFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Debug)]
struct ReconciliationProbeFactory {
    descriptor: PluginDescriptor,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
    activated: Arc<Notify>,
}

#[async_trait]
impl PluginFactory for ReconciliationProbeFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        if let Some(entered) = &self.entered {
            entered.notify_one();
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        self.activated.notify_one();
        Ok(())
    }
}

#[tokio::test]
async fn one_slow_pending_fiber_does_not_block_independent_reconciliation() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_concurrent_reconciliations: 2,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let slow_activated = Arc::new(Notify::new());
    let fast_activated = Arc::new(Notify::new());
    let requirement = || Requirement::new("reconcile", "test.reconcile", V1);
    runtime
        .root()
        .apply(
            Arc::new(ReconciliationProbeFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("slow-pending", "1"))
                    .requiring(requirement()),
                entered: Some(Arc::clone(&entered)),
                release: Some(Arc::clone(&release)),
                activated: Arc::clone(&slow_activated),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(ReconciliationProbeFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("fast-pending", "1"))
                    .requiring(requirement()),
                entered: None,
                release: None,
                activated: Arc::clone(&fast_activated),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "reconciliation-provider",
                    "1",
                ))
                .providing(Provision::new("reconcile", "test.reconcile", V1)),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("slow reconciliation did not start");
    let fast_result =
        tokio::time::timeout(std::time::Duration::from_secs(5), fast_activated.notified()).await;
    release.notify_one();
    assert!(
        fast_result.is_ok(),
        "an independent Fiber waited behind a slow reconciliation"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), slow_activated.notified())
        .await
        .expect("slow reconciliation did not finish after release");
}

#[async_trait]
impl PluginFactory for BlockingDeclaredProviderFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.provide("cycle-a", "test.cycle-a", V1, Arc::new(Echo))?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn cycle_diagnostics_follow_loading_fibers_actual_bindings() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "cycle-shared-provider",
                    "1",
                ))
                .providing(Provision::new(
                    "cycle-shared",
                    "test.cycle-shared",
                    V1,
                )),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let loading = tokio::spawn({
        let root = runtime.root();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            root.apply(
                Arc::new(BlockingDeclaredProviderFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "cycle-loading-provider",
                        "1",
                    ))
                    .requiring(Requirement::new("cycle-shared", "test.cycle-shared", V1))
                    .providing(Provision::new("cycle-a", "test.cycle-a", V1)),
                    entered,
                    release,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    let pending = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("cycle-pending", "1"))
                    .requiring(Requirement::new("cycle-a", "test.cycle-a", V1))
                    .providing(Provision::new("cycle-shared", "test.cycle-shared", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let reported_cycle = matches!(
        pending.snapshot().state,
        FiberState::Pending(ref reasons)
            if reasons.iter().any(|reason| matches!(reason, rsi_meta::PendingReason::DependencyCycle { .. }))
    );
    release.notify_one();
    loading.await.unwrap().unwrap();

    assert!(
        !reported_cycle,
        "a loading fiber's unused declared provider became a false cycle edge"
    );
}

#[derive(Debug)]
struct Echo;

#[async_trait]
impl ServiceEndpoint for Echo {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct EndpointFactory {
    descriptor: PluginDescriptor,
    endpoint: Arc<dyn ServiceEndpoint>,
}

#[async_trait]
impl PluginFactory for EndpointFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        let provision = &self.descriptor.provides[0];
        context.provide(
            provision.key.clone(),
            provision.contract.clone(),
            provision.version,
            Arc::clone(&self.endpoint),
        )
    }
}

#[derive(Debug)]
struct CaptureFactory {
    descriptor: PluginDescriptor,
    slot: Arc<Mutex<Option<ServiceHandle>>>,
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        *self.slot.lock().expect("capture poisoned") =
            Some(context.service(self.descriptor.requires[0].key.clone())?);
        Ok(())
    }
}

#[derive(Debug)]
struct CancellationProbe {
    entered: Arc<Notify>,
    cancelled: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for CancellationProbe {
    async fn serve(&self, _: rsi_meta::InvocationContext, channel: ProviderChannel) -> Result<()> {
        self.entered.notify_one();
        let cancellation = channel.cancellation();
        cancellation.cancelled().await;
        self.cancelled.notify_one();
        Ok(())
    }
}

async fn captured_service(endpoint: Arc<dyn ServiceEndpoint>) -> (Runtime, ServiceHandle) {
    captured_service_with_runtime(Runtime::default(), endpoint).await
}

async fn captured_service_with_runtime(
    runtime: Runtime,
    endpoint: Arc<dyn ServiceEndpoint>,
) -> (Runtime, ServiceHandle) {
    let provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("provider", "1"))
                    .providing(Provision::new("echo", "test.echo", V1)),
                endpoint,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(provider.snapshot().state, FiberState::Active));
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("consumer", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    let service = slot.lock().expect("capture poisoned").clone().unwrap();
    (runtime, service)
}

#[tokio::test]
async fn dropping_a_call_cancels_its_provider_invocation() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let (_runtime, service) = captured_service(Arc::new(CancellationProbe {
        entered: Arc::clone(&entered),
        cancelled: Arc::clone(&cancelled),
    }))
    .await;
    let call = service.open().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("provider invocation did not start");
    drop(call);
    tokio::time::timeout(std::time::Duration::from_secs(2), cancelled.notified())
        .await
        .expect("provider did not observe cancellation");
}

#[tokio::test]
async fn runtime_bounds_concurrent_service_calls_before_provider_invocation() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let runtime = Runtime::new(RuntimeLimits {
        maximum_concurrent_service_calls: 1,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_runtime, service) = captured_service_with_runtime(
        runtime,
        Arc::new(CancellationProbe {
            entered: Arc::clone(&entered),
            cancelled: Arc::clone(&cancelled),
        }),
    )
    .await;
    let first = service.open().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("first provider invocation did not start");
    let second = service.open();
    let rejected = matches!(
        second,
        Err(MetaError::CapacityExhausted {
            resource: "service calls"
        })
    );
    drop(second);
    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(2), cancelled.notified())
        .await
        .expect("first provider invocation did not observe cancellation");
    assert!(rejected, "a second live service call bypassed admission");
    tokio::task::yield_now().await;
    drop(service.open().expect("completed calls release admission"));
}

#[derive(Debug)]
struct SerializedProbe {
    active: AtomicUsize,
    maximum: AtomicUsize,
    calls: AtomicUsize,
    first_entered: Notify,
    second_entered: Notify,
    release_first: Notify,
}

#[async_trait]
impl ServiceEndpoint for SerializedProbe {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum.fetch_max(active, Ordering::AcqRel);
        let call = self.calls.fetch_add(1, Ordering::AcqRel);
        if call == 0 {
            self.first_entered.notify_one();
            self.release_first.notified().await;
        } else {
            self.second_entered.notify_one();
        }
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
        Ok(())
    }
}

#[tokio::test]
async fn safe_rust_provider_callbacks_can_run_concurrently() {
    let probe = Arc::new(SerializedProbe {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        first_entered: Notify::new(),
        second_entered: Notify::new(),
        release_first: Notify::new(),
    });
    let (_runtime, service) = captured_service(probe.clone()).await;
    let first = service.clone();
    let second = service;
    let first_task = tokio::spawn(async move {
        first
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"first".to_vec()))
            .await
            .unwrap()
    });
    probe.first_entered.notified().await;
    let second_task = tokio::spawn(async move {
        second
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"second".to_vec()))
            .await
            .unwrap()
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        probe.second_entered.notified(),
    )
    .await
    .expect("the second callback was serialized behind the first");
    assert_eq!(probe.maximum.load(Ordering::Acquire), 2);
    probe.release_first.notify_one();
    let (first, second) = tokio::join!(first_task, second_task);
    assert_eq!(first.unwrap().as_bytes(), b"first");
    assert_eq!(second.unwrap().as_bytes(), b"second");
    assert_eq!(probe.maximum.load(Ordering::Acquire), 2);
}

#[derive(Debug)]
struct BlockingActivationFactory {
    descriptor: PluginDescriptor,
    entered: Arc<Notify>,
    cleaned: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingActivationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.on(
            "cancelled-activation",
            Arc::new(NoopHandler),
            EventOptions::default(),
        )?;
        let cleaned = Arc::clone(&self.cleaned);
        context.defer(
            "cancelled activation",
            Box::new(move || {
                async move {
                    cleaned.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )?;
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancelling_apply_rolls_back_the_runtime_owned_activation() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_event_listeners: 1,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let cleaned = Arc::new(AtomicUsize::new(0));
    let root = runtime.root();
    let apply = tokio::spawn({
        let entered = Arc::clone(&entered);
        let cleaned = Arc::clone(&cleaned);
        async move {
            root.apply(
                Arc::new(BlockingActivationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "cancelled-apply",
                        "1",
                    )),
                    entered,
                    cleaned,
                }),
                Value::Null,
            )
            .await
        }
    });
    entered.notified().await;
    apply.abort();
    let _ = apply.await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().fibers.is_empty() && cleaned.load(Ordering::Acquire) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled apply did not roll back");

    let replacement = runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "replacement-listener",
                    "1",
                )),
                context: Arc::new(Mutex::new(None)),
                listener: Arc::new(Mutex::new(None)),
                remove_while_staged: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(replacement.snapshot().state, FiberState::Active));
}

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

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
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
    assert!(runtime.shutdown().await.is_clean());
    release.wait();
    assert!(matches!(
        application.await.unwrap(),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert!(runtime.snapshot().fibers.is_empty());
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

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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
    let runtime = Runtime::default();
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
}

#[async_trait]
impl PluginFactory for SerializedReconfigureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        Ok(config)
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reconfigures_commit_distinct_generations() {
    let runtime = Runtime::default();
    let factory = Arc::new(SerializedReconfigureFactory {
        descriptor: PluginDescriptor::new(FactoryIdentity::builtin("serialized-reconfigure", "1")),
    });
    let fiber = runtime
        .root()
        .apply(factory.clone(), Value::Null)
        .await
        .unwrap();
    let first = fiber.clone();
    let second = fiber;
    let first = tokio::spawn(async move { first.reconfigure(Value::from(1)).await.unwrap() });
    let second = tokio::spawn(async move { second.reconfigure(Value::from(2)).await.unwrap() });
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();

    assert_ne!(first.generation, second.generation);
    assert!(matches!(first.state, FiberState::Active));
    assert!(matches!(second.state, FiberState::Active));
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

    async fn activate(&self, _: Context, config: Value) -> Result<()> {
        self.activations
            .lock()
            .expect("activation log poisoned")
            .push(config);
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
    let reconfiguration = tokio::spawn({
        let fiber = fiber.clone();
        async move { fiber.reconfigure(Value::from(1)).await }
    });
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .unwrap();
    reconfiguration.abort();
    assert!(reconfiguration.await.unwrap_err().is_cancelled());
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .unwrap();

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
}

#[derive(Debug)]
struct NormalizingFactory {
    descriptor: PluginDescriptor,
    validations: Arc<AtomicUsize>,
    activated_with: Arc<Mutex<Option<Value>>>,
}

#[async_trait]
impl PluginFactory for NormalizingFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        self.validations.fetch_add(1, Ordering::AcqRel);
        Ok(Value::from(config.as_u64().unwrap() + 1))
    }

    async fn activate(&self, _: Context, config: Value) -> Result<()> {
        *self.activated_with.lock().expect("activation poisoned") = Some(config);
        Ok(())
    }
}

#[derive(Debug)]
struct OneShotDescriptorFactory {
    descriptor: PluginDescriptor,
    calls: AtomicUsize,
}

#[async_trait]
impl PluginFactory for OneShotDescriptorFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        assert_eq!(
            self.calls.fetch_add(1, Ordering::AcqRel),
            0,
            "descriptor was called after preparation"
        );
        &self.descriptor
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn prepared_descriptor_is_the_only_descriptor_observation() {
    let runtime = Runtime::default();
    let prepared = runtime
        .prepare(
            Arc::new(OneShotDescriptorFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "one-shot-descriptor",
                    "1",
                )),
                calls: AtomicUsize::new(0),
            }),
            Value::Null,
        )
        .unwrap();
    let fiber = runtime.root().apply_prepared(prepared).await.unwrap();
    assert!(matches!(fiber.snapshot().state, FiberState::Active));
    assert_eq!(runtime.snapshot().fibers.len(), 1);
}

#[tokio::test]
async fn prepared_application_runs_a_stateful_normalizer_exactly_once() {
    let runtime = Runtime::default();
    let validations = Arc::new(AtomicUsize::new(0));
    let activated_with = Arc::new(Mutex::new(None));
    let prepared = runtime
        .prepare(
            Arc::new(NormalizingFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("normalizer", "1")),
                validations: Arc::clone(&validations),
                activated_with: Arc::clone(&activated_with),
            }),
            Value::from(1),
        )
        .unwrap();
    runtime.root().apply_prepared(prepared).await.unwrap();
    assert_eq!(validations.load(Ordering::Acquire), 1);
    assert_eq!(
        *activated_with.lock().expect("activation poisoned"),
        Some(Value::from(2))
    );
}

#[derive(Debug)]
struct NoopHandler;

#[async_trait]
impl EventHandler for NoopHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct ValueIdentityHandler(Arc<Mutex<Vec<Arc<Value>>>>);

#[async_trait]
impl EventHandler for ValueIdentityHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0
            .lock()
            .expect("event value capture poisoned")
            .push(value);
        Ok(EventOutcome::Continue(Value::Null))
    }
}

#[tokio::test]
async fn parallel_listeners_share_one_immutable_input_value() {
    let runtime = Runtime::default();
    let values = Arc::new(Mutex::new(Vec::new()));
    for name in ["shared-value-first", "shared-value-second"] {
        runtime
            .root()
            .apply(
                Arc::new(EventFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(name, "1")),
                    event: "shared-value",
                    handler: Arc::new(ValueIdentityHandler(Arc::clone(&values))),
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }
    runtime
        .root()
        .dispatch(
            "shared-value",
            DispatchMode::Parallel,
            Value::String("payload".repeat(1_024)),
        )
        .await
        .unwrap();

    let values = values.lock().expect("event value capture poisoned");
    assert_eq!(values.len(), 2);
    assert!(Arc::ptr_eq(&values[0], &values[1]));
}

#[derive(Debug)]
struct ListenerCaptureFactory {
    descriptor: PluginDescriptor,
    context: Arc<Mutex<Option<Context>>>,
    listener: Arc<Mutex<Option<EventListenerId>>>,
    remove_while_staged: bool,
}

#[async_trait]
impl PluginFactory for ListenerCaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        let listener = context.on("authority", Arc::new(NoopHandler), EventOptions::default())?;
        if self.remove_while_staged {
            assert!(context.off(listener));
        }
        *self.context.lock().expect("context capture poisoned") = Some(context);
        *self.listener.lock().expect("listener capture poisoned") = Some(listener);
        Ok(())
    }
}

#[derive(Debug)]
struct ContextCaptureFactory {
    descriptor: PluginDescriptor,
    context: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        *self.context.lock().expect("context capture poisoned") = Some(context);
        Ok(())
    }
}

#[tokio::test]
async fn captured_context_cannot_cross_a_reconfiguration_generation() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "context-generation",
                    "1",
                )),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let old = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captured its context");
    fiber.reconfigure(Value::Null).await.unwrap();

    assert!(matches!(
        old.service("undeclared"),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(matches!(
        old.on("stale", Arc::new(NoopHandler), EventOptions::default()),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(matches!(
        old.defer("stale", Box::new(|| async move { Ok(()) }.boxed())),
        Err(MetaError::StaleContext { .. })
    ));
    assert!(matches!(
        old.dispatch("stale", DispatchMode::Emit, Value::Null).await,
        Err(MetaError::StaleContext { .. })
    ));
}

#[tokio::test]
async fn listener_capacity_and_generation_authority_cover_active_and_staged_entries() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_event_listeners: 1,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let owner_context = Arc::new(Mutex::new(None));
    let owner_listener = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("listener-owner", "1")),
                context: Arc::clone(&owner_context),
                listener: Arc::clone(&owner_listener),
                remove_while_staged: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let owner_context = owner_context.lock().unwrap().clone().unwrap();
    let owner_listener = owner_listener.lock().unwrap().unwrap();
    assert!(matches!(
        owner_context.on("authority", Arc::new(NoopHandler), EventOptions::default()),
        Err(MetaError::CapacityExhausted {
            resource: "event listeners"
        })
    ));

    let foreign_context = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("foreign", "1")),
                context: Arc::clone(&foreign_context),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    assert!(
        !foreign_context
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .off(owner_listener)
    );
    assert!(!runtime.root().off(owner_listener));

    owner.dispose().await;
    assert!(matches!(
        owner_context
            .dispatch("authority", DispatchMode::Emit, Value::Null)
            .await,
        Err(MetaError::StaleContext { .. })
    ));

    let staged_context = Arc::new(Mutex::new(None));
    let staged_listener = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("staged-off", "1")),
                context: staged_context,
                listener: staged_listener,
                remove_while_staged: true,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .root()
            .dispatch("authority", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
}

#[derive(Debug)]
struct HangingHandler(Arc<Notify>);

#[async_trait]
impl EventHandler for HangingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[derive(Debug)]
struct HangingFactory(PluginDescriptor, Arc<Notify>);

#[async_trait]
impl PluginFactory for HangingFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.on(
            "hang",
            Arc::new(HangingHandler(Arc::clone(&self.1))),
            EventOptions::default(),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct PanickingHandler;

#[async_trait]
impl EventHandler for PanickingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        panic!("event panic evidence");
    }
}

#[derive(Debug)]
struct CountingHandler(Arc<AtomicUsize>);

#[async_trait]
impl EventHandler for CountingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct OnceEventFactory {
    descriptor: PluginDescriptor,
    completed: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for OnceEventFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.on(
            "parallel-once",
            Arc::new(CountingHandler(Arc::clone(&self.completed))),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn concurrent_parallel_dispatches_claim_a_once_listener_exactly_once() {
    let runtime = Runtime::default();
    let completed = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            Arc::new(OnceEventFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("parallel-once", "1")),
                completed: Arc::clone(&completed),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let first = runtime.root();
    let second = runtime.root();
    let (first, second) = tokio::join!(
        first.dispatch("parallel-once", DispatchMode::Parallel, Value::Null),
        second.dispatch("parallel-once", DispatchMode::Parallel, Value::Null),
    );

    assert_eq!(first.unwrap().invoked + second.unwrap().invoked, 1);
    assert_eq!(completed.load(Ordering::Acquire), 1);
}

#[derive(Debug)]
struct EventFactory {
    descriptor: PluginDescriptor,
    event: &'static str,
    handler: Arc<dyn EventHandler>,
}

#[async_trait]
impl PluginFactory for EventFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.on(
            self.event,
            Arc::clone(&self.handler),
            EventOptions::default(),
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn panicking_event_handlers_become_errors_without_cancelling_parallel_siblings() {
    let runtime = Runtime::default();
    let completed = Arc::new(AtomicUsize::new(0));
    for (name, handler) in [
        (
            "panicking-event",
            Arc::new(PanickingHandler) as Arc<dyn EventHandler>,
        ),
        (
            "parallel-sibling",
            Arc::new(CountingHandler(Arc::clone(&completed))) as Arc<dyn EventHandler>,
        ),
    ] {
        runtime
            .root()
            .apply(
                Arc::new(EventFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(name, "1")),
                    event: "panic",
                    handler,
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }

    let error = runtime
        .root()
        .dispatch("panic", DispatchMode::Parallel, Value::Null)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("panicked"), "{error}");
    assert_eq!(completed.load(Ordering::Acquire), 1);
    assert!(runtime.snapshot().terminal.is_none());
}

#[derive(Debug)]
struct DelayedHandler(std::time::Duration);

#[async_trait]
impl EventHandler for DelayedHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        tokio::time::sleep(self.0).await;
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct OnceDelayedFactory {
    descriptor: PluginDescriptor,
    delay: std::time::Duration,
}

#[async_trait]
impl PluginFactory for OnceDelayedFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.on(
            "once-timeout",
            Arc::new(DelayedHandler(self.delay)),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )?;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_once_listener_is_still_consumed_by_its_single_attempt() {
    let runtime = Runtime::new(RuntimeLimits {
        event_callback_timeout: std::time::Duration::from_millis(20),
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(OnceDelayedFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("once-timeout", "1")),
                delay: std::time::Duration::from_millis(30),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    assert_eq!(
        runtime
            .root()
            .dispatch("once-timeout", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::Timeout("event dispatch")
    );
    assert_eq!(
        runtime
            .root()
            .dispatch("once-timeout", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn one_event_deadline_bounds_the_complete_serial_dispatch() {
    let runtime = Runtime::new(RuntimeLimits {
        event_callback_timeout: std::time::Duration::from_millis(20),
        ..RuntimeLimits::default()
    })
    .unwrap();
    for name in ["first-delayed-event", "second-delayed-event"] {
        runtime
            .root()
            .apply(
                Arc::new(EventFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(name, "1")),
                    event: "deadline",
                    handler: Arc::new(DelayedHandler(std::time::Duration::from_millis(15))),
                }),
                Value::Null,
            )
            .await
            .unwrap();
    }

    let error = runtime
        .root()
        .dispatch("deadline", DispatchMode::Emit, Value::Null)
        .await
        .unwrap_err();
    assert_eq!(error, MetaError::Timeout("event dispatch"));
}

#[tokio::test]
async fn event_callback_deadline_bounds_the_handler() {
    let runtime = Runtime::new(RuntimeLimits {
        event_callback_timeout: std::time::Duration::from_millis(20),
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "timeout-listener",
                    "1",
                )),
                context: Arc::new(Mutex::new(None)),
                listener: Arc::new(Mutex::new(None)),
                remove_while_staged: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(HangingFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("hanging", "1")),
                Arc::clone(&entered),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let result = runtime
        .root()
        .dispatch("hang", DispatchMode::Emit, Value::Null)
        .await;
    assert_eq!(result.unwrap_err(), MetaError::Timeout("event dispatch"));
}

#[derive(Debug)]
struct BlockingHandler {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl EventHandler for BlockingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(EventOutcome::Continue((*value).clone()))
    }
}

#[derive(Debug)]
struct CountingEndpoint(Arc<AtomicUsize>);

#[async_trait]
impl ServiceEndpoint for CountingEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel) -> Result<()> {
        self.0.fetch_add(1, Ordering::AcqRel);
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SharedCallbackFactory {
    descriptor: PluginDescriptor,
    handler: Arc<dyn EventHandler>,
    endpoint: Arc<dyn ServiceEndpoint>,
}

#[async_trait]
impl PluginFactory for SharedCallbackFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.on("block", Arc::clone(&self.handler), EventOptions::default())?;
        context.provide("echo", "test.echo", V1, Arc::clone(&self.endpoint))
    }
}

#[tokio::test]
async fn service_callbacks_do_not_wait_for_same_generation_event_handlers() {
    let runtime = Runtime::new(RuntimeLimits {
        service_call_timeout: std::time::Duration::from_secs(1),
        event_callback_timeout: std::time::Duration::from_secs(1),
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            Arc::new(SharedCallbackFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("shared-lock", "1"))
                    .providing(Provision::new("echo", "test.echo", V1)),
                handler: Arc::new(BlockingHandler {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }),
                endpoint: Arc::new(CountingEndpoint(Arc::clone(&calls))),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("queue-client", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let dispatch_root = runtime.root();
    let dispatch = tokio::spawn(async move {
        dispatch_root
            .dispatch("block", DispatchMode::Emit, Value::Null)
            .await
    });
    entered.notified().await;
    let service = slot.lock().unwrap().clone().unwrap();
    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(b"queued".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"queued");
    assert_eq!(calls.load(Ordering::Acquire), 1);
    release.notify_one();
    dispatch.await.unwrap().unwrap();
}

#[derive(Debug)]
struct TraceLeaf(Arc<Mutex<Option<CallId>>>);

#[async_trait]
impl ServiceEndpoint for TraceLeaf {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        *self.0.lock().expect("trace capture poisoned") = invocation.parent_call_id();
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct TraceBridge {
    enclosing: Arc<Mutex<Option<CallId>>>,
}

#[async_trait]
impl ServiceEndpoint for TraceBridge {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        *self.enclosing.lock().expect("trace capture poisoned") = Some(invocation.call_id());
        while let Some(frame) = channel.recv().await {
            let response = invocation
                .provider_context()
                .service("leaf")?
                .open()?
                .unary(frame)
                .await?;
            channel.send(response).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn nested_service_trace_names_the_immediate_enclosing_call() {
    let runtime = Runtime::default();
    let leaf_parent = Arc::new(Mutex::new(None));
    let leaf = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("trace-leaf", "1"))
                    .providing(Provision::new("leaf", "test.leaf", V1)),
                endpoint: Arc::new(TraceLeaf(Arc::clone(&leaf_parent))),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(leaf.snapshot().state, FiberState::Active));
    let enclosing = Arc::new(Mutex::new(None));
    let bridge = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("trace-bridge", "1"))
                    .requiring(Requirement::new("leaf", "test.leaf", V1))
                    .providing(Provision::new("bridge", "test.bridge", V1)),
                endpoint: Arc::new(TraceBridge {
                    enclosing: Arc::clone(&enclosing),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(bridge.snapshot().state, FiberState::Active));
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("trace-client", "1"))
                    .requiring(Requirement::new("bridge", "test.bridge", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    let service = slot.lock().unwrap().clone().unwrap();
    service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(b"trace".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        *leaf_parent.lock().expect("trace capture poisoned"),
        *enclosing.lock().expect("trace capture poisoned")
    );
}

#[derive(Debug)]
struct DrainEndpoint {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for DrainEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[derive(Debug)]
struct DrainFactory {
    descriptor: PluginDescriptor,
    endpoint: Arc<dyn ServiceEndpoint>,
    cleaned: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for DrainFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.provide("echo", "test.echo", V1, Arc::clone(&self.endpoint))?;
        let cleaned = Arc::clone(&self.cleaned);
        context.defer(
            "drain cleanup",
            Box::new(move || {
                async move {
                    cleaned.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn retirement_never_cleans_resources_before_admitted_callbacks_drain() {
    let runtime = Runtime::new(RuntimeLimits {
        transition_timeout: std::time::Duration::from_millis(5),
        service_call_timeout: std::time::Duration::from_secs(1),
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cleaned = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(DrainFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("drain", "1"))
                    .providing(Provision::new("echo", "test.echo", V1)),
                endpoint: Arc::new(DrainEndpoint {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }),
                cleaned: Arc::clone(&cleaned),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("drain-client", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let call = slot.lock().unwrap().clone().unwrap().open().unwrap();
    entered.notified().await;
    let mut disposal = tokio::spawn(async move { provider.dispose().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut disposal)
            .await
            .is_err()
    );
    assert_eq!(cleaned.load(Ordering::Acquire), 0);
    release.notify_one();
    assert!(disposal.await.unwrap().is_clean());
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
    drop(call);
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

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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
async fn shutdown_deadline_reports_every_blocked_root_and_terminalizes_admission() {
    let runtime = Runtime::new(RuntimeLimits {
        transition_timeout: std::time::Duration::from_millis(10),
        service_call_timeout: std::time::Duration::from_millis(10),
        event_callback_timeout: std::time::Duration::from_millis(10),
        shutdown_timeout: std::time::Duration::from_millis(20),
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
    let report = shutdown.await.unwrap();

    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert_eq!(
        report.failures[0].label,
        format!("fiber {} shutdown", fiber.id().0)
    );
    assert_eq!(
        report.failures[0].error,
        MetaError::Timeout("runtime shutdown").to_string()
    );
    assert!(runtime.snapshot().terminal.is_some());
    release.notify_one();
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

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        panic!("activation panic evidence")
    }
}

#[derive(Debug)]
struct HangingActivationFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for HangingActivationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn activation_deadline_turns_a_hanging_plugin_into_a_failed_fiber() {
    let runtime = Runtime::new(RuntimeLimits {
        transition_timeout: std::time::Duration::from_millis(10),
        service_call_timeout: std::time::Duration::from_millis(10),
        event_callback_timeout: std::time::Duration::from_millis(10),
        shutdown_timeout: std::time::Duration::from_millis(20),
        ..RuntimeLimits::default()
    })
    .unwrap();
    let applying = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                Arc::new(HangingActivationFactory(PluginDescriptor::new(
                    FactoryIdentity::builtin("hanging-activation", "1"),
                ))),
                Value::Null,
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(11)).await;
    let fiber = applying.await.unwrap().unwrap();

    assert_eq!(
        fiber.snapshot().state,
        FiberState::Failed(MetaError::Timeout("plugin activation").to_string())
    );
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
struct PanicCleanupSerializationFactory {
    descriptor: PluginDescriptor,
    cleanup_entered: Arc<Notify>,
    cleanup_release: Arc<Notify>,
    panic_once: AtomicBool,
}

#[async_trait]
impl PluginFactory for PanicCleanupSerializationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, config: Value) -> Result<()> {
        if config == 1 && !self.panic_once.swap(true, Ordering::AcqRel) {
            let entered = Arc::clone(&self.cleanup_entered);
            let release = Arc::clone(&self.cleanup_release);
            context.defer(
                "panic cleanup serialization",
                Box::new(move || {
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    }
                    .boxed()
                }),
            )?;
            panic!("serialized panic cleanup evidence");
        }
        Ok(())
    }
}

#[tokio::test]
async fn activation_panic_cleanup_remains_inside_the_transition_transaction() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "panic-dependency",
                    "1",
                ))
                .providing(Provision::new(
                    "panic-dependency",
                    "test.dependency",
                    V1,
                )),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let consumer = runtime
        .root()
        .apply(
            Arc::new(PanicCleanupSerializationFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "panic-cleanup-consumer",
                    "1",
                ))
                .requiring(Requirement::new(
                    "panic-dependency",
                    "test.dependency",
                    V1,
                )),
                cleanup_entered: Arc::clone(&cleanup_entered),
                cleanup_release: Arc::clone(&cleanup_release),
                panic_once: AtomicBool::new(false),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));

    let reconfiguration = tokio::spawn({
        let consumer = consumer.clone();
        async move { consumer.reconfigure(Value::from(1)).await }
    });
    cleanup_entered.notified().await;
    let mut provider_disposal = tokio::spawn(async move { provider.dispose().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut provider_disposal)
            .await
            .is_err(),
        "dependency reconciliation overlapped panic cleanup"
    );
    cleanup_release.notify_one();
    assert!(provider_disposal.await.unwrap().is_clean());
    let snapshot = reconfiguration.await.unwrap().unwrap();
    assert!(matches!(snapshot.state, FiberState::Failed(_)));
}

#[derive(Debug)]
struct PanickingCleanupFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PanickingCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].label, "panicking cleanup");
    assert!(report.failures[0].error.contains("panicked"));
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

    async fn activate(&self, _: Context, _: Value) -> Result<()> {
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

#[test]
fn intercept_bound_applies_to_the_accumulated_overlay() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_frame_bytes: 8,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let first = runtime.root().intercept("echo", Value::from("a")).unwrap();
    assert_eq!(
        first.intercept("echo", Value::from("a")).unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 8 }
    );
}

#[derive(Debug)]
struct OversizedOutcomeHandler;

#[async_trait]
impl EventHandler for OversizedOutcomeHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Ok(EventOutcome::Continue(Value::String(
            "0123456789".to_owned(),
        )))
    }
}

#[tokio::test]
async fn handler_produced_event_values_obey_the_frame_bound() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_frame_bytes: 8,
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime
        .root()
        .apply(
            Arc::new(EventFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "oversized-event-outcome",
                    "1",
                )),
                event: "oversized-outcome",
                handler: Arc::new(OversizedOutcomeHandler),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    assert_eq!(
        runtime
            .root()
            .dispatch("oversized-outcome", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 8 }
    );
}

#[tokio::test]
async fn dispatch_rejects_an_oversized_input_before_listener_lookup() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_frame_bytes: 8,
        ..RuntimeLimits::default()
    })
    .unwrap();

    assert_eq!(
        runtime
            .root()
            .dispatch(
                "no-listeners",
                DispatchMode::Emit,
                Value::String("0123456789".to_owned()),
            )
            .await
            .unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 8 }
    );
}

#[derive(Debug)]
struct BufferedThenHangingEndpoint;

#[async_trait]
impl ServiceEndpoint for BufferedThenHangingEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel) -> Result<()> {
        let request = channel.recv().await.expect("test request");
        channel.send(request).await?;
        std::future::pending().await
    }
}

#[derive(Debug)]
struct RespondThenFailEndpoint;

#[async_trait]
impl ServiceEndpoint for RespondThenFailEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel) -> Result<()> {
        let request = channel.recv().await.expect("test request");
        channel.send(request).await?;
        Err(MetaError::Service("provider terminal facts".to_owned()))
    }
}

#[derive(Debug)]
struct OversizedResponseEndpoint;

#[async_trait]
impl ServiceEndpoint for OversizedResponseEndpoint {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel) -> Result<()> {
        channel.recv().await.expect("test request");
        channel.send(ServiceFrame::new(vec![0; 5])).await
    }
}

#[tokio::test]
async fn provider_response_frames_obey_the_same_bound_as_caller_requests() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_frame_bytes: 4,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_runtime, service) =
        captured_service_with_runtime(runtime, Arc::new(OversizedResponseEndpoint)).await;

    assert_eq!(
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(Vec::new()))
            .await
            .unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 4 }
    );
}

#[tokio::test]
async fn unary_preserves_a_provider_error_after_the_response() {
    let (_runtime, service) = captured_service(Arc::new(RespondThenFailEndpoint)).await;
    assert_eq!(
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"response".to_vec()))
            .await
            .unwrap_err(),
        MetaError::Service("provider terminal facts".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_service_error_survives_a_full_response_channel() {
    let runtime = Runtime::new(RuntimeLimits {
        channel_capacity: 1,
        service_call_timeout: std::time::Duration::from_millis(20),
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_runtime, service) =
        captured_service_with_runtime(runtime, Arc::new(BufferedThenHangingEndpoint)).await;
    let mut call = service.open().unwrap();
    call.send(ServiceFrame::new(b"buffered".to_vec()))
        .await
        .unwrap();
    call.finish();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    assert_eq!(call.recv().await.unwrap().unwrap().as_bytes(), b"buffered");
    assert_eq!(
        call.recv().await.unwrap().unwrap_err(),
        MetaError::Timeout("service call")
    );
}

#[derive(Debug)]
struct InvocationDebugEndpoint;

#[async_trait]
impl ServiceEndpoint for InvocationDebugEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        while channel.recv().await.is_some() {
            channel
                .send(ServiceFrame::new(format!("{invocation:?}").into_bytes()))
                .await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn invocation_debug_output_does_not_disclose_edge_overlays() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("debug-provider", "1"))
                    .providing(Provision::new("echo", "test.echo", V1)),
                endpoint: Arc::new(InvocationDebugEndpoint),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let slot = Arc::new(Mutex::new(None));
    runtime
        .root()
        .intercept("echo", Value::String("top-secret-overlay".to_owned()))
        .unwrap()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("debug-client", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = slot.lock().unwrap().clone().unwrap();
    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(Vec::new()))
        .await
        .unwrap();
    let debug = std::str::from_utf8(response.as_bytes()).unwrap();
    assert!(!debug.contains("top-secret-overlay"), "{debug}");
}

#[derive(Debug)]
struct FailingCleanupFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for FailingCleanupFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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
    assert_eq!(first.failures[0].label, "forced failure");
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
    let (first, second): (CleanupReport, CleanupReport) =
        tokio::join!(runtime.shutdown(), runtime.shutdown());
    assert_eq!(first, second);
    assert_eq!(first.failures.len(), 1);
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
