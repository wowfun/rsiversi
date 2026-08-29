use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    ActivationPlan, ContractVersion, DeadlineLimits, ExecutionLimits, FactoryIdentity, FiberState,
    InvocationContext, IsolationId, MAXIMUM_JSON_DEPTH, MetaError, PayloadLimits, PendingReason,
    PluginFactory, PreparedActivation, ProviderChannel, Requirement, Result, Runtime,
    RuntimeLimits, ServiceEndpoint, TopologyLimits,
};
use serde_json::{Value, json};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[path = "support/resolver.rs"]
mod resolver;
mod support;
use resolver::resolved;

use support::{Echo, EndpointFactory, FactorySpec};

const V1: ContractVersion = ContractVersion(1);
const DEEP_CONFIG_CHILD: &str = "RSI_META_DEEP_CONFIG_CHILD";

#[test]
fn configuration_numbers_preserve_exact_decimal_text_in_package_builds() {
    let value: Value = serde_json::from_str("1.0000000000000001").unwrap();
    assert_eq!(value.to_string(), "1.0000000000000001");
}

fn minimum_retained_plugin_limit(limits: &RuntimeLimits) -> usize {
    let payloads = &limits.payloads;
    let maximum_requirement_bytes = limits.topology.maximum_requirements_per_fiber
        * (payloads.maximum_identifier_bytes * 2 + std::mem::size_of::<ContractVersion>());
    let maximum_attempt_bytes = payloads.maximum_config_bytes
        + payloads.maximum_prepared_state_bytes
        + maximum_requirement_bytes;
    payloads.maximum_identifier_bytes * 2
        + payloads.maximum_config_bytes * 2
        + maximum_attempt_bytes * 2
}

#[test]
fn grouped_limits_validate_primitive_bounds_without_coupling_shutdown_wait() {
    Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            transition: Duration::from_secs(2),
            shutdown_wait: Duration::from_secs(1),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .expect("shutdown waiting is independent from an admitted transition");

    let invalid = [
        RuntimeLimits {
            topology: TopologyLimits {
                maximum_fibers: 0,
                ..TopologyLimits::default()
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            payloads: PayloadLimits {
                maximum_message_bytes: 0,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            payloads: PayloadLimits {
                maximum_message_bytes: 2,
                maximum_buffered_message_bytes: 1,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            execution: ExecutionLimits {
                channel_capacity: tokio::sync::Semaphore::MAX_PERMITS,
                ..ExecutionLimits::default()
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            deadlines: DeadlineLimits {
                service_call: Duration::MAX,
                ..DeadlineLimits::default()
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            payloads: PayloadLimits {
                maximum_identifier_bytes: 1,
                maximum_prepared_state_bytes: usize::MAX,
                maximum_config_bytes: 1,
                maximum_retained_plugin_bytes: usize::MAX,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        },
    ];

    for limits in invalid {
        let constructed = std::panic::catch_unwind(|| Runtime::new(limits));
        assert!(matches!(constructed, Ok(Err(MetaError::InvalidInput(_)))));
    }
}

#[test]
fn retained_plugin_budget_covers_desired_and_normalized_attempt_configurations() {
    let limits = |maximum_retained_plugin_bytes| RuntimeLimits {
        topology: TopologyLimits {
            maximum_dependency_edges: 1,
            maximum_requirements_per_fiber: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_identifier_bytes: 1,
            maximum_prepared_state_bytes: 3,
            maximum_config_bytes: 4,
            maximum_retained_plugin_bytes,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    };

    let exact = minimum_retained_plugin_limit(&limits(usize::MAX));
    Runtime::new(limits(exact))
        .expect("identity, desired, normalized, state, and requirement reservations fit exactly");
    assert_eq!(
        Runtime::new(limits(exact - 1)).unwrap_err(),
        MetaError::InvalidInput(
            "a maximum plugin or Message payload exceeds its aggregate Runtime budget".to_owned()
        )
    );
}

#[test]
fn every_accepted_boundary_policy_constructs_without_panicking() {
    let tokio_maximum = tokio::sync::Semaphore::MAX_PERMITS;
    let accepted = [
        RuntimeLimits::default(),
        RuntimeLimits {
            payloads: PayloadLimits {
                maximum_message_bytes: (u32::MAX as usize).min(tokio_maximum),
                maximum_buffered_message_bytes: tokio_maximum,
                ..PayloadLimits::default()
            },
            execution: ExecutionLimits {
                maximum_concurrent_preparations: tokio_maximum,
                maximum_concurrent_reconciliations: tokio_maximum,
                maximum_concurrent_service_calls: tokio_maximum,
                channel_capacity: tokio_maximum - 1,
                maximum_pending_message_sends: usize::MAX,
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            deadlines: DeadlineLimits {
                transition: Duration::from_hours(24),
                service_call: Duration::from_hours(24),
                shutdown_wait: Duration::from_hours(24),
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            topology: TopologyLimits {
                maximum_fibers: 1,
                maximum_fiber_depth: 2,
                maximum_services: 2,
                maximum_dependency_edges: 2,
                maximum_requirements_per_fiber: 2,
                maximum_effects_per_fiber: 2,
                maximum_effects: 1,
                ..TopologyLimits::default()
            },
            payloads: PayloadLimits {
                maximum_identifier_bytes: 16,
                maximum_prepared_state_bytes: 8,
                maximum_json_depth: 8,
                maximum_json_nodes: 1,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        },
    ];

    for limits in accepted {
        let constructed = std::panic::catch_unwind(|| {
            let runtime = Runtime::new(limits)?;
            assert!(runtime.root().owner().is_none());
            assert_eq!(runtime.resource_snapshot().fibers.current, 0);
            Result::Ok(runtime)
        });
        assert!(matches!(constructed, Ok(Ok(_))));
    }
}

#[test]
fn duplicate_prepared_requirement_diagnostic_names_the_service() {
    let runtime = Runtime::default();
    let spec = FactorySpec::new(FactoryIdentity::linked("duplicate-detail", "7"))
        .requiring(Requirement::new("same", "one", V1))
        .requiring(Requirement::new("same", "two", V1));

    let error = runtime
        .prepare(crate::resolved(Arc::new(PassiveFactory(spec))), Value::Null)
        .expect_err("duplicate requirements must fail preparation");
    assert_eq!(
        error,
        MetaError::InvalidInput(
            "prepared activation requires service same more than once".to_owned()
        )
    );
}

#[test]
fn every_one_field_boundary_candidate_is_rejected_or_constructs_without_panicking() {
    let tokio_maximum = tokio::sync::Semaphore::MAX_PERMITS;
    let mut candidates = Vec::new();
    macro_rules! topology_candidates {
        ($($field:ident),+ $(,)?) => {$({
            for value in [1, usize::MAX] {
                let mut limits = RuntimeLimits::default();
                limits.topology.$field = value;
                candidates.push(limits);
            }
        })+};
    }
    macro_rules! payload_candidates {
        ($($field:ident),+ $(,)?) => {$({
            for value in [1, usize::MAX] {
                let mut limits = RuntimeLimits::default();
                limits.payloads.$field = value;
                candidates.push(limits);
            }
        })+};
    }
    macro_rules! execution_candidates {
        ($($field:ident),+ $(,)?) => {$({
            for value in [1, tokio_maximum, tokio_maximum.saturating_add(1)] {
                let mut limits = RuntimeLimits::default();
                limits.execution.$field = value;
                candidates.push(limits);
            }
        })+};
    }
    macro_rules! deadline_candidates {
        ($($field:ident),+ $(,)?) => {$({
            for value in [
                Duration::from_nanos(1),
                Duration::from_hours(24),
                Duration::from_hours(24).saturating_add(Duration::from_nanos(1)),
                Duration::MAX,
            ] {
                let mut limits = RuntimeLimits::default();
                limits.deadlines.$field = value;
                candidates.push(limits);
            }
        })+};
    }

    topology_candidates!(
        maximum_fibers,
        maximum_fiber_depth,
        maximum_services,
        maximum_dependency_edges,
        maximum_requirements_per_fiber,
        maximum_event_listeners,
        maximum_effects_per_fiber,
        maximum_effects,
        maximum_effect_transactions_per_fiber,
        maximum_effect_transactions,
        maximum_context_entries,
        maximum_capability_entries,
        maximum_capabilities_per_message,
        maximum_queued_capability_references,
    );
    payload_candidates!(
        maximum_identifier_bytes,
        maximum_prepared_state_bytes,
        maximum_message_bytes,
        maximum_config_bytes,
        maximum_retained_plugin_bytes,
        maximum_context_bytes,
        maximum_buffered_message_bytes,
        maximum_json_depth,
        maximum_json_nodes,
        maximum_diagnostic_entries,
        maximum_diagnostic_bytes,
    );
    execution_candidates!(
        maximum_concurrent_preparations,
        maximum_concurrent_reconciliations,
        maximum_concurrent_service_calls,
        channel_capacity,
        maximum_pending_message_sends,
    );
    deadline_candidates!(transition, service_call, shutdown_wait);

    let mut accepted = 0;
    for limits in candidates {
        let constructed = std::panic::catch_unwind(|| {
            Runtime::new(limits).inspect(|runtime| {
                assert!(runtime.root().owner().is_none());
                let snapshot = runtime.resource_snapshot();
                assert_eq!(snapshot.fibers.current, 0);
                assert_eq!(snapshot.service_calls.current, 0);
                assert_eq!(snapshot.buffered_message_bytes.current, 0);
            })
        });
        let constructed = constructed.expect("a boundary policy panicked during validation");
        accepted += usize::from(constructed.is_ok());
    }
    assert!(accepted > 0);
}

#[test]
fn json_depth_hard_ceiling_is_exact_and_precedes_recursive_encoding() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_json_depth: MAXIMUM_JSON_DEPTH,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let mut boundary = Value::Null;
    for _ in 1..MAXIMUM_JSON_DEPTH {
        boundary = Value::Array(vec![boundary]);
    }
    runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("depth-boundary", "1"),
            )))),
            boundary,
        )
        .expect("the exact implementation-safe JSON depth must be accepted");

    assert!(matches!(
        Runtime::new(RuntimeLimits {
            payloads: PayloadLimits {
                maximum_json_depth: MAXIMUM_JSON_DEPTH + 1,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        }),
        Err(MetaError::InvalidInput(_))
    ));

    let mut too_deep = Value::Null;
    for _ in 0..MAXIMUM_JSON_DEPTH {
        too_deep = Value::Array(vec![too_deep]);
    }
    assert!(matches!(
        runtime.prepare(
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("too-deep", "1"),
            )))),
            too_deep,
        ),
        Err(MetaError::InvalidConfig(_))
    ));
}

#[test]
fn deep_owned_config_boundaries_reject_or_drop_without_recursing() {
    if let Some(scenario) = std::env::var_os(DEEP_CONFIG_CHILD) {
        run_deep_config_scenario(scenario.to_str().expect("scenario is valid UTF-8"));
        return;
    }

    for scenario in [
        "prepare-input",
        "validate-output",
        "drop-apply",
        "drop-reconfigure",
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .env(DEEP_CONFIG_CHILD, scenario)
            .args([
                "--exact",
                "deep_owned_config_boundaries_reject_or_drop_without_recursing",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "deep owned config scenario {scenario} crashed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

fn run_deep_config_scenario(scenario: &str) {
    let runtime = Runtime::default();
    let spec = || FactorySpec::new(FactoryIdentity::linked("deep-config", "1"));
    match scenario {
        "prepare-input" => assert!(matches!(
            runtime.prepare(crate::resolved(Arc::new(PassiveFactory(spec()))), deep_json_value()),
            Err(MetaError::InvalidConfig(message)) if message.contains("nesting")
        )),
        "validate-output" => assert!(matches!(
            runtime.prepare(crate::resolved(Arc::new(DeepConfigFactory)), Value::Null),
            Err(MetaError::InvalidConfig(message)) if message.contains("nesting")
        )),
        "drop-apply" => {
            let root = runtime.root();
            drop(root.apply(
                crate::resolved(Arc::new(PassiveFactory(spec()))),
                deep_json_value(),
            ));
        }
        "drop-reconfigure" => {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let fiber = executor
                .block_on(runtime.root().apply(
                    crate::resolved(Arc::new(PassiveFactory(spec()))),
                    Value::Null,
                ))
                .unwrap();
            drop(fiber.reconfigure(deep_json_value()));
            executor.block_on(async {
                assert!(fiber.dispose().await.is_clean());
                assert!(runtime.shutdown().await.is_complete());
            });
        }
        _ => panic!("unknown deep config scenario {scenario}"),
    }
}

fn deep_json_value() -> Value {
    (0..100_000).fold(Value::Null, |value, _| Value::Array(vec![value]))
}

#[test]
fn resource_snapshot_exposes_every_global_budget_with_its_validated_limit() {
    let limits = RuntimeLimits::default();
    let runtime = Runtime::new(limits.clone()).unwrap();
    let snapshot = runtime.resource_snapshot();
    let expected = [
        (&snapshot.listeners, limits.topology.maximum_event_listeners),
        (
            &snapshot.effect_transactions,
            limits.topology.maximum_effect_transactions,
        ),
        (
            &snapshot.service_calls,
            limits.execution.maximum_concurrent_service_calls,
        ),
        (
            &snapshot.buffered_message_bytes,
            limits.payloads.maximum_buffered_message_bytes,
        ),
        (
            &snapshot.reconciliations,
            limits.execution.maximum_concurrent_reconciliations,
        ),
        (&snapshot.scheduler_workers, 1),
        (&snapshot.cleanup_runs, limits.topology.maximum_fibers),
    ];
    for (usage, limit) in expected {
        assert_eq!(usage.current, 0);
        assert_eq!(usage.limit, limit);
        assert_eq!(usage.high_watermark, 0);
        assert_eq!(usage.rejected, 0);
    }
}

#[derive(Debug)]
struct PassiveFactory(FactorySpec);

#[async_trait]
impl PluginFactory for PassiveFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct DeepConfigFactory;

#[async_trait]
impl PluginFactory for DeepConfigFactory {
    fn prepare(&self, _: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(deep_json_value()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct UnexpectedFactory;

#[async_trait]
impl PluginFactory for UnexpectedFactory {
    fn prepare(&self, _: &Value) -> Result<PreparedActivation> {
        panic!("busy admission must precede preparation")
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        panic!("busy admission must precede activation")
    }
}

#[tokio::test]
async fn prepared_proofs_are_runtime_bound_and_reserve_until_drop_or_disposal() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 1,
            maximum_fiber_depth: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let factory = || FactorySpec::new(FactoryIdentity::linked("reserved", "1"));

    let prepared = runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(factory()))),
            Value::Null,
        )
        .unwrap();
    let reserved = runtime.resource_snapshot();
    assert_eq!(reserved.fibers.current, 1);
    assert!(reserved.retained_plugin_bytes.current > 0);

    let other = Runtime::default();
    assert!(matches!(
        other.root().apply_prepared(prepared).await,
        Err(MetaError::PreparedForDifferentRuntime)
    ));
    assert_eq!(runtime.resource_snapshot().fibers.current, 0);

    let prepared = runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(factory()))),
            Value::Null,
        )
        .unwrap();
    let fiber = runtime.root().apply_prepared(prepared).await.unwrap();
    assert_eq!(runtime.resource_snapshot().fibers.current, 1);
    fiber.dispose().await;
    let released = runtime.resource_snapshot();
    assert_eq!(released.fibers.current, 0);
    assert_eq!(released.retained_plugin_bytes.current, 0);
    assert_eq!(released.fibers.high_watermark, 1);
}

#[test]
fn plugin_capacity_is_reserved_before_identity_observation() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 1,
            maximum_fiber_depth: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let proof = runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("fiber", "1"),
            )))),
            Value::Null,
        )
        .unwrap();
    assert!(matches!(
        runtime.prepare(crate::resolved(Arc::new(UnexpectedFactory)), Value::Null),
        Err(MetaError::CapacityExhausted { resource: "fibers" })
    ));
    drop(proof);

    let mut retained_limits = RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 3,
            maximum_fiber_depth: 2,
            maximum_dependency_edges: 1,
            maximum_requirements_per_fiber: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_identifier_bytes: 16,
            maximum_prepared_state_bytes: 1,
            maximum_config_bytes: 128,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    };
    retained_limits.payloads.maximum_retained_plugin_bytes =
        minimum_retained_plugin_limit(&retained_limits);
    let maximum_config_bytes = retained_limits.payloads.maximum_config_bytes;
    let retained_runtime = Runtime::new(retained_limits).unwrap();
    let proof = retained_runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("1234567890123456", "1234567890123456"),
            )))),
            Value::String("x".repeat(maximum_config_bytes - 2)),
        )
        .unwrap();
    let second_proof = retained_runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("abcdefghijklmnop", "abcdefghijklmnop"),
            )))),
            Value::String("y".repeat(maximum_config_bytes - 2)),
        )
        .unwrap();
    assert!(matches!(
        retained_runtime.prepare(crate::resolved(Arc::new(UnexpectedFactory)), Value::Null),
        Err(MetaError::CapacityExhausted {
            resource: "retained plugin bytes"
        })
    ));
    let snapshot = retained_runtime.resource_snapshot();
    assert_eq!(snapshot.fibers.current, 2);
    assert_eq!(snapshot.retained_plugin_bytes.rejected, 1);
    drop(proof);
    drop(second_proof);
}

#[test]
fn preparation_validates_identity_requirements_and_json_shape_before_retention() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_requirements_per_fiber: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_identifier_bytes: 8,
            maximum_json_depth: 4,
            maximum_json_nodes: 8,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    let oversized_identity = FactorySpec::new(FactoryIdentity::linked("123456789", "1"));
    let resolved_identity = oversized_identity.identity();
    assert!(matches!(
        runtime.prepare(
            rsi_meta::ResolvedFactory::new(
                resolved_identity,
                rsi_meta::UpdateMode::Replayable,
                Arc::new(PassiveFactory(oversized_identity)),
            ),
            Value::Null,
        ),
        Err(MetaError::InvalidInput(_))
    ));

    let too_many_requirements = FactorySpec::new(FactoryIdentity::linked("bounded", "1"))
        .requiring(Requirement::new("first", "contract", ContractVersion(1)))
        .requiring(Requirement::new("second", "contract", ContractVersion(1)));
    assert!(matches!(
        runtime.prepare(
            crate::resolved(Arc::new(PassiveFactory(too_many_requirements))),
            Value::Null
        ),
        Err(MetaError::InvalidInput(_))
    ));

    let valid = FactorySpec::new(FactoryIdentity::linked("bounded", "1"));
    assert!(matches!(
        runtime.prepare(
            crate::resolved(Arc::new(PassiveFactory(valid.clone()))),
            json!([[[[null]]]])
        ),
        Err(MetaError::InvalidConfig(_))
    ));
    assert!(matches!(
        runtime.prepare(
            crate::resolved(Arc::new(PassiveFactory(valid))),
            Value::Array(vec![Value::Null; 9]),
        ),
        Err(MetaError::InvalidConfig(_))
    ));
    assert_eq!(runtime.resource_snapshot().fibers.current, 0);
}

#[derive(Debug)]
struct PreparedStateDropProbe(Arc<AtomicUsize>);

impl Drop for PreparedStateDropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct PreparedStateFactory {
    retained_bytes: usize,
    drops: Arc<AtomicUsize>,
    takes: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for PreparedStateFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::with_state(
            desired.clone(),
            PreparedStateDropProbe(Arc::clone(&self.drops)),
            self.retained_bytes,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> Result<()> {
        assert!(matches!(
            plan.take_state::<String>(),
            Err(MetaError::PreparedStateTypeMismatch { .. })
        ));
        let state = plan
            .take_state::<PreparedStateDropProbe>()
            .expect("wrong-type take must preserve opaque state");
        assert!(matches!(
            plan.take_state::<PreparedStateDropProbe>(),
            Err(MetaError::PreparedStateUnavailable)
        ));
        self.takes.fetch_add(1, Ordering::AcqRel);
        drop(state);
        Ok(())
    }
}

#[tokio::test]
async fn opaque_prepared_state_is_exactly_accounted_moved_and_dropped() {
    const STATE_BYTES: usize = 5;
    let config_bytes = serde_json::to_vec(&json!("x")).unwrap().len();
    let mut limits = RuntimeLimits {
        topology: TopologyLimits {
            maximum_dependency_edges: 1,
            maximum_requirements_per_fiber: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_identifier_bytes: "exact".len(),
            maximum_prepared_state_bytes: STATE_BYTES,
            maximum_config_bytes: config_bytes,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    };
    limits.payloads.maximum_retained_plugin_bytes = minimum_retained_plugin_limit(&limits);
    let runtime = Runtime::new(limits).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let takes = Arc::new(AtomicUsize::new(0));
    let factory = |retained_bytes| {
        rsi_meta::ResolvedFactory::linked(
            "exact",
            "1",
            rsi_meta::UpdateMode::Replayable,
            Arc::new(PreparedStateFactory {
                retained_bytes,
                drops: Arc::clone(&drops),
                takes: Arc::clone(&takes),
            }),
        )
    };

    let proof = runtime.prepare(factory(STATE_BYTES), json!("x")).unwrap();
    assert_eq!(
        runtime.resource_snapshot().retained_plugin_bytes.current,
        "exact".len() + "1".len() + config_bytes * 2 + STATE_BYTES,
    );
    assert_eq!(drops.load(Ordering::Acquire), 0);
    drop(proof);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);

    assert!(matches!(
        runtime.prepare(factory(STATE_BYTES + 1), json!("x")),
        Err(MetaError::PayloadTooLarge {
            maximum: STATE_BYTES
        })
    ));
    assert_eq!(drops.load(Ordering::Acquire), 2);
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);

    let fiber = runtime
        .root()
        .apply(factory(STATE_BYTES), json!("x"))
        .await
        .unwrap();
    assert!(matches!(fiber.snapshot().state, FiberState::Active));
    assert_eq!(takes.load(Ordering::Acquire), 1);
    assert_eq!(drops.load(Ordering::Acquire), 3);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[test]
fn prepared_requirement_capacity_is_global_and_released_with_the_proof() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 2,
            maximum_fiber_depth: 2,
            maximum_dependency_edges: 1,
            maximum_requirements_per_fiber: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let factory = |name: &str| {
        FactorySpec::new(FactoryIdentity::linked(name, "1")).requiring(Requirement::new(
            format!("{name}-missing"),
            "contract",
            ContractVersion(1),
        ))
    };
    let first = runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(factory("first")))),
            Value::Null,
        )
        .unwrap();
    assert!(matches!(
        runtime.prepare(
            crate::resolved(Arc::new(PassiveFactory(factory("second")))),
            Value::Null,
        ),
        Err(MetaError::CapacityExhausted {
            resource: "dependency edges"
        })
    ));
    assert_eq!(runtime.resource_snapshot().dependency_edges.rejected, 1);
    drop(first);
    let replacement = runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(factory("third")))),
            Value::Null,
        )
        .unwrap();
    drop(replacement);
    assert_eq!(runtime.resource_snapshot().dependency_edges.current, 0);
}

#[test]
fn context_scope_bounds_entries_and_identifiers() {
    let entry_runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_context_entries: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let scoped = entry_runtime
        .root()
        .isolate("first", IsolationId(1))
        .unwrap()
        .isolate("first", IsolationId(2))
        .unwrap();
    assert!(matches!(
        scoped.isolate("second", IsolationId(3)),
        Err(MetaError::CapacityExhausted {
            resource: "context entries"
        })
    ));

    let shape_runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_identifier_bytes: 8,
            maximum_json_depth: 2,
            maximum_json_nodes: 8,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    assert!(matches!(
        shape_runtime.root().isolate("123456789", IsolationId(1)),
        Err(MetaError::InvalidInput(_))
    ));
}

#[derive(Debug)]
struct NoopEndpoint;

#[async_trait]
impl ServiceEndpoint for NoopEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct OwnedResourcesFactory {
    spec: FactorySpec,
    services: Vec<(&'static str, &'static str, ContractVersion)>,
    effects: usize,
}

#[async_trait]
impl PluginFactory for OwnedResourcesFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let context = plan.context().clone();
        for (key, contract, version) in &self.services {
            context.provide(*key, *contract, *version, Arc::new(NoopEndpoint))?;
        }
        for index in 0..self.effects {
            context.defer(
                format!("effect-{index}"),
                Box::new(|| async { Ok(()) }.boxed()),
            )?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn staged_service_and_effect_budgets_release_after_rollback_and_disposal() {
    let service_runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_services: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let service_fiber = service_runtime
        .root()
        .apply(
            crate::resolved(Arc::new(OwnedResourcesFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("services", "1")),
                services: vec![
                    ("first", "test.first", ContractVersion(1)),
                    ("second", "test.second", ContractVersion(1)),
                ],
                effects: 0,
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        service_fiber.snapshot().state,
        FiberState::Failed(_)
    ));
    let services = service_runtime.resource_snapshot().services;
    assert_eq!(services.current, 0);
    assert_eq!(services.high_watermark, 1);
    assert_eq!(services.rejected, 1);
    service_fiber.dispose().await;

    let effect_runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_effects_per_fiber: 1,
            maximum_effects: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let one_effect = || {
        Arc::new(OwnedResourcesFactory {
            spec: FactorySpec::new(FactoryIdentity::linked("effect", "1")),
            services: Vec::new(),
            effects: 1,
        })
    };
    let active = effect_runtime
        .root()
        .apply(resolved(one_effect()), Value::Null)
        .await
        .unwrap();
    assert_eq!(effect_runtime.resource_snapshot().effects.current, 1);
    let rejected = effect_runtime
        .root()
        .apply(resolved(one_effect()), Value::Null)
        .await
        .unwrap();
    assert!(matches!(rejected.snapshot().state, FiberState::Failed(_)));
    let effects = effect_runtime.resource_snapshot().effects;
    assert_eq!(effects.current, 1);
    assert_eq!(effects.high_watermark, 1);
    assert_eq!(effects.rejected, 1);

    active.dispose().await;
    assert_eq!(effect_runtime.resource_snapshot().effects.current, 0);
    rejected.dispose().await;
}

#[derive(Debug)]
struct BlockingPreparationFactory {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

#[derive(Debug)]
struct CountingReconfigurationFactory {
    preparations: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct BlockingReactivationFactory {
    spec: FactorySpec,
    activations: AtomicUsize,
    drop_entered: mpsc::SyncSender<()>,
    drop_release: Arc<Mutex<mpsc::Receiver<()>>>,
    retained: Arc<Mutex<Option<std::sync::Weak<Value>>>>,
}

#[derive(Debug)]
struct BlockingConfigDrop {
    _config: Arc<Value>,
    entered: mpsc::SyncSender<()>,
    release: Arc<Mutex<mpsc::Receiver<()>>>,
}

impl Drop for BlockingConfigDrop {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let _ = self.release.lock().expect("drop release poisoned").recv();
    }
}

#[async_trait]
impl PluginFactory for BlockingReactivationFactory {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let config = Arc::clone(plan.config());
        if self.activations.fetch_add(1, Ordering::AcqRel) == 1 {
            *self.retained.lock().expect("retained config poisoned") =
                Some(Arc::downgrade(&config));
            let _drop = BlockingConfigDrop {
                _config: config,
                entered: self.drop_entered.clone(),
                release: Arc::clone(&self.drop_release),
            };
            futures_util::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for CountingReconfigurationFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        if !config.is_null() {
            self.preparations.fetch_add(1, Ordering::AcqRel);
        }
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn reconfiguration_staging_capacity_is_reserved_before_plugin_preparation() {
    const MAXIMUM_CONFIG_BYTES: usize = 128;
    let spec = FactorySpec::new(FactoryIdentity::linked("reconfiguration-staging", "1"));
    let proof_config = Value::String("x".repeat(MAXIMUM_CONFIG_BYTES - 2));
    let mut limits = RuntimeLimits {
        topology: TopologyLimits {
            maximum_dependency_edges: 1,
            maximum_requirements_per_fiber: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_identifier_bytes: 32,
            maximum_prepared_state_bytes: 1,
            maximum_config_bytes: MAXIMUM_CONFIG_BYTES,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    };
    limits.payloads.maximum_retained_plugin_bytes = minimum_retained_plugin_limit(&limits);
    let runtime = Runtime::new(limits).unwrap();
    let preparations = Arc::new(AtomicUsize::new(0));
    let resolve = |factory: Arc<dyn PluginFactory>| {
        rsi_meta::ResolvedFactory::new(spec.identity(), rsi_meta::UpdateMode::Replayable, factory)
    };
    let fiber = runtime
        .root()
        .apply(
            resolve(Arc::new(CountingReconfigurationFactory {
                preparations: Arc::clone(&preparations),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    let proof = runtime
        .prepare(
            resolve(Arc::new(PassiveFactory(spec.clone()))),
            proof_config.clone(),
        )
        .unwrap();
    let second_proof = runtime
        .prepare(
            resolve(Arc::new(PassiveFactory(spec.clone()))),
            proof_config,
        )
        .unwrap();
    let before = runtime.resource_snapshot().retained_plugin_bytes;
    assert!(before.limit - before.current < MAXIMUM_CONFIG_BYTES);

    assert_eq!(
        fiber.reconfigure(Value::from(1)).await.unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "retained plugin bytes",
        }
    );
    assert_eq!(preparations.load(Ordering::Acquire), 0);
    let rejected = runtime.resource_snapshot().retained_plugin_bytes;
    assert_eq!(rejected.current, before.current);
    assert_eq!(rejected.rejected, before.rejected + 1);

    drop(proof);
    drop(second_proof);
    let retained_after_proof = runtime.resource_snapshot().retained_plugin_bytes.current;
    assert!(matches!(
        fiber
            .reconfigure(Value::String("y".repeat(MAXIMUM_CONFIG_BYTES)))
            .await,
        Err(MetaError::InvalidConfig(_))
    ));
    assert_eq!(preparations.load(Ordering::Acquire), 0);
    assert_eq!(
        runtime.resource_snapshot().retained_plugin_bytes.current,
        retained_after_proof,
        "failed normalization retained its staging reservation",
    );
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // One public scenario spans reactivation, replacement, and cleanup.
async fn reconfiguration_retains_old_config_capacity_while_activation_owns_it() {
    const MAXIMUM_CONFIG_BYTES: usize = 128;
    const OLD_CONFIG_BYTES: usize = 98;
    const NEW_CONFIG_BYTES: usize = 4;

    let provider_identity = || FactoryIdentity::linked("retained-config-provider", "1");
    let consumer_spec = FactorySpec::new(FactoryIdentity::linked("retained-config-consumer", "1"))
        .requiring(Requirement::new("dependency", "test.dependency", V1));
    let requirement_bytes =
        "dependency".len() + "test.dependency".len() + std::mem::size_of::<ContractVersion>();
    let old_config = Value::String("x".repeat(96));
    assert_eq!(
        serde_json::to_vec(&old_config).unwrap().len(),
        OLD_CONFIG_BYTES
    );
    assert_eq!(
        serde_json::to_vec(&Value::Null).unwrap().len(),
        NEW_CONFIG_BYTES
    );

    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_config_bytes: MAXIMUM_CONFIG_BYTES,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let provider_factory = || {
        Arc::new(EndpointFactory::new(
            provider_identity(),
            "dependency",
            "test.dependency",
            V1,
            Arc::new(Echo),
        ))
    };
    let first_provider = runtime
        .root()
        .apply(resolved(provider_factory()), Value::Null)
        .await
        .unwrap();
    let (drop_entered_sender, drop_entered) = mpsc::sync_channel(1);
    let (drop_release, drop_release_receiver) = mpsc::sync_channel(1);
    let retained = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(BlockingReactivationFactory {
                spec: consumer_spec,
                activations: AtomicUsize::new(0),
                drop_entered: drop_entered_sender,
                drop_release: Arc::new(Mutex::new(drop_release_receiver)),
                retained: Arc::clone(&retained),
            })),
            old_config,
        )
        .await
        .unwrap();

    assert!(first_provider.dispose().await.is_clean());
    let second_provider = runtime
        .root()
        .apply(resolved(provider_factory()), Value::Null)
        .await
        .unwrap();
    let retained_before = runtime.resource_snapshot().retained_plugin_bytes.current;

    let reconfiguration = tokio::spawn({
        let consumer = consumer.clone();
        async move { consumer.reconfigure(Value::Null).await }
    });
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || drop_entered.recv()),
    )
    .await
    .expect("reconfiguration did not cancel the old activation")
    .unwrap()
    .expect("blocking activation drop sender disappeared");
    let retained_during = runtime.resource_snapshot().retained_plugin_bytes.current;
    let old_config = retained
        .lock()
        .expect("retained config poisoned")
        .as_ref()
        .expect("blocking activation did not retain its configuration")
        .upgrade()
        .expect("the blocking activation released its configuration early");
    assert_eq!(old_config.as_str().map(str::len), Some(96));
    assert_eq!(
        retained_during,
        retained_before - OLD_CONFIG_BYTES + 2 * NEW_CONFIG_BYTES + requirement_bytes,
        "the one-time identity and old normalized attempt coexist after the superseded raw desired is released",
    );
    drop(old_config);

    drop_release
        .send(())
        .expect("blocking activation still owns its drop receiver");
    tokio::time::timeout(Duration::from_secs(2), reconfiguration)
        .await
        .expect("reconfiguration did not converge after activation was released")
        .unwrap()
        .unwrap();
    assert!(consumer.dispose().await.is_clean());
    assert!(second_provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);
}

#[async_trait]
impl PluginFactory for BlockingPreparationFactory {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        self.entered.send(()).expect("test waiter still exists");
        self.release
            .lock()
            .expect("release receiver poisoned")
            .recv()
            .expect("test releases preparation");
        Ok(PreparedActivation::new(config.clone()))
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Ok(())
    }
}

#[test]
fn preparation_admission_is_fail_fast_and_reports_rejections() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_preparations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let blocked_runtime = runtime.clone();
    let blocked = std::thread::spawn(move || {
        blocked_runtime.prepare(
            crate::resolved(Arc::new(BlockingPreparationFactory {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            })),
            Value::Null,
        )
    });
    entered_rx.recv().unwrap();

    assert!(matches!(
        runtime.prepare(crate::resolved(Arc::new(UnexpectedFactory)), Value::Null,),
        Err(MetaError::Busy {
            operation: "plugin preparation"
        })
    ));
    let usage = runtime.resource_snapshot().preparations;
    assert_eq!(usage.current, 1);
    assert_eq!(usage.high_watermark, 1);
    assert_eq!(usage.rejected, 1);

    release_tx.send(()).unwrap();
    drop(blocked.join().unwrap().unwrap());
    assert_eq!(runtime.resource_snapshot().preparations.current, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_apply_rejects_busy_before_starting_another_normalizer() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_preparations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let first = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                crate::resolved(Arc::new(BlockingPreparationFactory {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                })),
                Value::Null,
            )
            .await
        }
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    let error = runtime
        .root()
        .apply(crate::resolved(Arc::new(UnexpectedFactory)), Value::Null)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        MetaError::Busy {
            operation: "plugin preparation"
        }
    );
    release_tx.send(()).unwrap();
    first.await.unwrap().unwrap().dispose().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_apply_waiter_does_not_release_preparation_while_blocking_work_runs() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_preparations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let first = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                crate::resolved(Arc::new(BlockingPreparationFactory {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                })),
                Value::Null,
            )
            .await
        }
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    first.abort();
    assert!(matches!(
        runtime.prepare(crate::resolved(Arc::new(UnexpectedFactory)), Value::Null),
        Err(MetaError::Busy {
            operation: "plugin preparation"
        })
    ));

    release_tx.send(()).unwrap();
    assert!(first.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = runtime.resource_snapshot();
            if snapshot.preparations.current == 0
                && snapshot.fibers.current == 0
                && snapshot.retained_plugin_bytes.current == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocking preparation releases its Runtime-owned admission");
    let released = runtime.resource_snapshot();
    assert_eq!(released.fibers.current, 0);
    assert_eq!(released.retained_plugin_bytes.current, 0);
    let replacement = runtime
        .prepare(
            crate::resolved(Arc::new(PassiveFactory(FactorySpec::new(
                FactoryIdentity::linked("replacement", "1"),
            )))),
            Value::Null,
        )
        .unwrap();
    drop(replacement);
}

#[tokio::test]
async fn pending_reports_bound_missing_service_samples_before_snapshot_cloning() {
    const REQUIREMENTS: usize = 16;
    const MAXIMUM_ENTRIES: usize = 3;
    const MAXIMUM_BYTES: usize = 20;
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_entries: MAXIMUM_ENTRIES,
            maximum_diagnostic_bytes: MAXIMUM_BYTES,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let mut spec = FactorySpec::new(FactoryIdentity::linked("bounded-missing", "1"));
    for index in 0..REQUIREMENTS {
        spec = spec.requiring(Requirement::new(
            format!("service-{index:02}"),
            "missing",
            ContractVersion(1),
        ));
    }
    let fiber = runtime
        .root()
        .apply(crate::resolved(Arc::new(PassiveFactory(spec))), Value::Null)
        .await
        .unwrap();
    let snapshot = fiber.snapshot();
    let FiberState::Pending(report) = &snapshot.state else {
        panic!("missing requirements unexpectedly activated");
    };
    assert_eq!(report.total_reasons, REQUIREMENTS);
    assert!(report.reasons.len() <= MAXIMUM_ENTRIES);
    assert!(report.truncated);
    let retained_bytes = report
        .reasons
        .iter()
        .map(|reason| match reason {
            PendingReason::MissingService { service, .. } => service.as_str().len(),
            PendingReason::ContractMismatch {
                service,
                expected,
                actual,
                ..
            } => service.as_str().len() + expected.as_str().len() + actual.as_str().len(),
            PendingReason::MissingLocal { contract, .. } => contract.as_str().len(),
        })
        .sum::<usize>();
    assert!(retained_bytes <= MAXIMUM_BYTES);
    let serialized = serde_json::to_value(&snapshot.state).unwrap();
    assert_eq!(serialized["pending"]["total_reasons"], REQUIREMENTS);
    assert_eq!(serialized["pending"]["truncated"], true);
    assert!(serialized["pending"]["reasons"].is_array());
    for _ in 0..64 {
        assert_eq!(fiber.snapshot(), snapshot);
    }
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn pending_report_omits_a_reason_when_no_sample_entry_fits() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_entries: 1,
            maximum_diagnostic_bytes: 3,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let fiber = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(PassiveFactory(
                FactorySpec::new(FactoryIdentity::linked("omitted-missing", "1"))
                    .requiring(Requirement::new("long", "missing", V1)),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let FiberState::Pending(report) = fiber.snapshot().state else {
        panic!("missing requirement unexpectedly activated");
    };
    assert_eq!(report.total_reasons, 1);
    assert!(report.reasons.is_empty());
    assert!(report.truncated);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn pending_reports_retain_only_a_bounded_reason_prefix() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_entries: 2,
            maximum_diagnostic_bytes: 5,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let fiber = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(PassiveFactory(
                FactorySpec::new(FactoryIdentity::linked("bounded-pending", "1"))
                    .requiring(Requirement::new("abcd", "test", ContractVersion(1)))
                    .requiring(Requirement::new("efgh", "test", ContractVersion(1)))
                    .requiring(Requirement::new("i", "test", ContractVersion(1))),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let FiberState::Pending(report) = fiber.snapshot().state else {
        panic!("missing services unexpectedly activated the Fiber");
    };

    assert_eq!(report.total_reasons, 3);
    assert_eq!(report.reasons.len(), 1);
    assert!(report.truncated);
    assert!(matches!(
        &report.reasons[0],
        PendingReason::MissingService { service, .. } if service.as_str() == "abcd"
    ));
}

#[path = "resource_limits/contract_invariants.rs"]
mod contract_invariants;
