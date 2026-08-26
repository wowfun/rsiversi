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
        topology!(maximum_dependency_edges),
        topology!(maximum_requirements_per_fiber),
        topology!(maximum_event_listeners),
        topology!(maximum_effects_per_fiber),
        topology!(maximum_effects),
        topology!(maximum_effect_transactions_per_fiber),
        topology!(maximum_effect_transactions),
        topology!(maximum_context_entries),
        topology!(maximum_capability_entries),
        topology!(maximum_capabilities_per_message),
        topology!(maximum_queued_capability_references),
        payloads!(maximum_identifier_bytes),
        payloads!(maximum_prepared_state_bytes),
        payloads!(maximum_message_bytes),
        payloads!(maximum_config_bytes),
        payloads!(maximum_retained_plugin_bytes),
        payloads!(maximum_context_bytes),
        payloads!(maximum_buffered_message_bytes),
        payloads!(maximum_json_depth),
        payloads!(maximum_json_nodes),
        payloads!(maximum_diagnostic_entries),
        payloads!(maximum_diagnostic_bytes),
        execution!(maximum_concurrent_preparations),
        execution!(maximum_concurrent_reconciliations),
        execution!(maximum_concurrent_service_calls),
        execution!(channel_capacity),
        execution!(maximum_pending_message_sends),
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
    let duplicate = FactorySpec::new(FactoryIdentity::builtin("duplicate", "1"))
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
                FactorySpec::new(FactoryIdentity::builtin("consumer", "1"))
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
                Arc::new(PassiveFactory(FactorySpec::new(FactoryIdentity::builtin(
                    "over-capacity",
                    "1"
                )))),
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
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("provider", "1"),
                "slot",
                "actual",
                V1,
                Arc::new(Echo),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(provider.snapshot().state, FiberState::Active));
    let consumer = mismatch_runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                FactorySpec::new(FactoryIdentity::builtin("mismatch", "1"))
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
struct ExpandingConfigFactory(FactorySpec);

#[async_trait]
impl PluginFactory for ExpandingConfigFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, _: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::String("x".repeat(64))))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct PanickingConfigFactory(FactorySpec);

#[async_trait]
impl PluginFactory for PanickingConfigFactory {
    fn identity(&self) -> FactoryIdentity {
        self.0.identity()
    }

    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if config.is_null() {
            Ok(PreparedActivation::new(config.clone()))
        } else {
            panic!("configuration preparation panic")
        }
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
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
    let spec = || FactorySpec::new(FactoryIdentity::builtin("bounded-configuration", "1"));

    assert!(matches!(
        runtime.prepare(
            Arc::new(PassiveFactory(spec())),
            Value::String("x".repeat(64)),
        ),
        Err(MetaError::InvalidConfig(_))
    ));
    assert!(matches!(
        runtime.prepare(Arc::new(ExpandingConfigFactory(spec())), Value::Null,),
        Err(MetaError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn preparation_panics_have_one_error_classification() {
    let runtime = Runtime::default();
    let factory = Arc::new(PanickingConfigFactory(FactorySpec::new(
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
    spec: FactorySpec,
    services: Vec<(&'static str, &'static str, ContractVersion)>,
    effects: usize,
}

#[async_trait]
impl PluginFactory for QuotaFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        for (key, contract, version) in &self.services {
            context.provide(*key, *contract, *version, Arc::new(Echo))?;
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
                spec: FactorySpec::new(FactoryIdentity::builtin("service-quota", "1")),
                services: vec![("first", "test.first", V1), ("second", "test.second", V1)],
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
                spec: FactorySpec::new(FactoryIdentity::builtin("effect-quota", "1")),
                services: Vec::new(),
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
    spec: FactorySpec,
    preparations: Arc<AtomicUsize>,
    activated_with: Arc<Mutex<Option<Value>>>,
}

#[async_trait]
impl PluginFactory for NormalizingFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.preparations.fetch_add(1, Ordering::AcqRel);
        Ok(PreparedActivation::new(Value::from(
            config.as_u64().unwrap() + 1,
        )))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let config = Arc::clone(plan.config());
        *self.activated_with.lock().expect("activation poisoned") = Some((*config).clone());
        Ok(())
    }
}

#[derive(Debug)]
struct OneShotIdentityFactory {
    spec: FactorySpec,
    calls: AtomicUsize,
}

#[async_trait]
impl PluginFactory for OneShotIdentityFactory {
    fn identity(&self) -> FactoryIdentity {
        assert_eq!(
            self.calls.fetch_add(1, Ordering::AcqRel),
            0,
            "identity was called more than once"
        );
        self.spec.identity()
    }

    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn factory_identity_is_captured_once_across_preparation_and_activation() {
    let runtime = Runtime::default();
    let factory = Arc::new(OneShotIdentityFactory {
        spec: FactorySpec::new(FactoryIdentity::builtin("one-shot-identity", "1")),
        calls: AtomicUsize::new(0),
    });
    let prepared = runtime.prepare(factory.clone(), Value::Null).unwrap();
    let fiber = runtime.root().apply_prepared(prepared).await.unwrap();
    assert!(matches!(fiber.snapshot().state, FiberState::Active));
    fiber.reconfigure(Value::from(1)).await.unwrap();
    assert_eq!(factory.calls.load(Ordering::Acquire), 1);
    assert_eq!(runtime.snapshot().fibers.len(), 1);
}

#[tokio::test]
async fn prepared_application_runs_a_stateful_normalizer_exactly_once() {
    let runtime = Runtime::default();
    let preparations = Arc::new(AtomicUsize::new(0));
    let activated_with = Arc::new(Mutex::new(None));
    let prepared = runtime
        .prepare(
            Arc::new(NormalizingFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("normalizer", "1")),
                preparations: Arc::clone(&preparations),
                activated_with: Arc::clone(&activated_with),
            }),
            Value::from(1),
        )
        .unwrap();
    runtime.root().apply_prepared(prepared).await.unwrap();
    assert_eq!(preparations.load(Ordering::Acquire), 1);
    assert_eq!(
        *activated_with.lock().expect("activation poisoned"),
        Some(Value::from(2))
    );
}
