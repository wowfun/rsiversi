use super::*;

#[tokio::test]
async fn runtime_rejects_every_zero_limit() {
    let zero = std::time::Duration::ZERO;
    macro_rules! topology {
        ($field:ident) => {
            RuntimeLimits {
                topology: TopologyLimits {
                    $field: 0,
                    ..TopologyLimits::default()
                },
                ..RuntimeLimits::default()
            }
        };
    }
    macro_rules! payloads {
        ($field:ident) => {
            RuntimeLimits {
                payloads: PayloadLimits {
                    $field: 0,
                    ..PayloadLimits::default()
                },
                ..RuntimeLimits::default()
            }
        };
    }
    macro_rules! execution {
        ($field:ident) => {
            RuntimeLimits {
                execution: ExecutionLimits {
                    $field: 0,
                    ..ExecutionLimits::default()
                },
                ..RuntimeLimits::default()
            }
        };
    }
    macro_rules! deadline {
        ($field:ident) => {
            RuntimeLimits {
                deadlines: DeadlineLimits {
                    $field: zero,
                    ..DeadlineLimits::default()
                },
                ..RuntimeLimits::default()
            }
        };
    }
    let invalid_limits = [
        topology!(maximum_fibers),
        topology!(maximum_fiber_depth),
        topology!(maximum_services),
        topology!(maximum_service_declarations),
        topology!(maximum_dependency_edges),
        topology!(maximum_requirements_per_fiber),
        topology!(maximum_provisions_per_fiber),
        topology!(maximum_event_listeners),
        topology!(maximum_effects_per_fiber),
        topology!(maximum_effects),
        topology!(maximum_context_entries),
        payloads!(maximum_identifier_bytes),
        payloads!(maximum_descriptor_bytes),
        payloads!(maximum_frame_bytes),
        payloads!(maximum_config_bytes),
        payloads!(maximum_retained_plugin_bytes),
        payloads!(maximum_context_bytes),
        payloads!(maximum_buffered_service_bytes),
        payloads!(maximum_json_depth),
        payloads!(maximum_json_nodes),
        payloads!(maximum_diagnostic_entries),
        payloads!(maximum_diagnostic_bytes),
        execution!(maximum_concurrent_preparations),
        execution!(maximum_concurrent_reconciliations),
        execution!(maximum_concurrent_service_calls),
        execution!(channel_capacity),
        execution!(maximum_concurrent_event_dispatches),
        execution!(maximum_concurrent_event_callbacks),
        deadline!(transition),
        deadline!(service_call),
        deadline!(event_dispatch),
        deadline!(shutdown_wait),
    ];
    for limits in invalid_limits {
        assert!(matches!(
            Runtime::new(limits),
            Err(MetaError::InvalidInput(_))
        ));
    }
}

#[tokio::test]
async fn limits_duplicates_contract_mismatches_and_wait_cancellation_fail_closed() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 1,
            maximum_fiber_depth: 1,
            ..TopologyLimits::default()
        },
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
        FiberState::Pending(ref report)
            if report.reasons.iter().any(|reason| matches!(
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

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
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

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn plugin_config_is_bounded_before_and_after_normalization() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_config_bytes: 32,
            ..PayloadLimits::default()
        },
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

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
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
        topology: TopologyLimits {
            maximum_services: 1,
            ..TopologyLimits::default()
        },
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
        topology: TopologyLimits {
            maximum_effects_per_fiber: 1,
            ..TopologyLimits::default()
        },
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

    async fn activate(&self, _: Context, config: Arc<Value>) -> Result<()> {
        *self.activated_with.lock().expect("activation poisoned") = Some((*config).clone());
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

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
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
