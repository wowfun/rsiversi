use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    Context, ContractVersion, DeadlineLimits, ExecutionLimits, FactoryIdentity, FiberState,
    InvocationContext, IsolationId, MAXIMUM_JSON_DEPTH, MetaError, PayloadLimits, PendingReason,
    PendingReport, PluginDescriptor, PluginFactory, ProviderChannel, Provision, Requirement,
    Result, Runtime, RuntimeLimits, ServiceEndpoint, TopologyLimits,
};
use serde_json::{Value, json};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

mod support;

use support::{Echo, EndpointFactory};

const V1: ContractVersion = ContractVersion(1);
const DEEP_CONFIG_CHILD: &str = "RSI_META_DEEP_CONFIG_CHILD";

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
                maximum_frame_bytes: 0,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            payloads: PayloadLimits {
                maximum_frame_bytes: 2,
                maximum_buffered_service_bytes: 1,
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
                maximum_descriptor_bytes: usize::MAX,
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
fn every_accepted_boundary_policy_constructs_without_panicking() {
    let tokio_maximum = tokio::sync::Semaphore::MAX_PERMITS;
    let accepted = [
        RuntimeLimits::default(),
        RuntimeLimits {
            payloads: PayloadLimits {
                maximum_frame_bytes: (u32::MAX as usize).min(tokio_maximum),
                maximum_buffered_service_bytes: tokio_maximum,
                ..PayloadLimits::default()
            },
            execution: ExecutionLimits {
                maximum_concurrent_preparations: tokio_maximum,
                maximum_concurrent_reconciliations: tokio_maximum,
                maximum_concurrent_service_calls: tokio_maximum,
                channel_capacity: tokio_maximum - 1,
                maximum_concurrent_event_dispatches: tokio_maximum,
                maximum_concurrent_event_callbacks: tokio_maximum,
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            deadlines: DeadlineLimits {
                transition: Duration::from_hours(24),
                service_call: Duration::from_hours(24),
                event_dispatch: Duration::from_hours(24),
                shutdown_wait: Duration::from_hours(24),
            },
            ..RuntimeLimits::default()
        },
        RuntimeLimits {
            topology: TopologyLimits {
                maximum_fibers: 1,
                maximum_fiber_depth: 2,
                maximum_services: 2,
                maximum_service_declarations: 1,
                maximum_dependency_edges: 1,
                maximum_requirements_per_fiber: 2,
                maximum_provisions_per_fiber: 2,
                maximum_effects_per_fiber: 2,
                maximum_effects: 1,
                ..TopologyLimits::default()
            },
            payloads: PayloadLimits {
                maximum_identifier_bytes: 16,
                maximum_descriptor_bytes: 8,
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
fn duplicate_descriptor_diagnostic_names_the_factory_and_service() {
    let runtime = Runtime::default();
    let descriptor = PluginDescriptor::new(FactoryIdentity::builtin("duplicate-detail", "7"))
        .requiring(Requirement::new("same", "one", V1))
        .requiring(Requirement::new("same", "two", V1));

    let error = runtime
        .prepare(Arc::new(PassiveFactory(descriptor)), Value::Null)
        .expect_err("duplicate requirements must fail preparation");
    assert_eq!(
        error,
        MetaError::InvalidInput(
            "factory duplicate-detail@7 declares requirement same more than once".to_owned()
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
        maximum_service_declarations,
        maximum_dependency_edges,
        maximum_requirements_per_fiber,
        maximum_provisions_per_fiber,
        maximum_event_listeners,
        maximum_effects_per_fiber,
        maximum_effects,
        maximum_context_entries,
    );
    payload_candidates!(
        maximum_identifier_bytes,
        maximum_descriptor_bytes,
        maximum_frame_bytes,
        maximum_config_bytes,
        maximum_retained_plugin_bytes,
        maximum_context_bytes,
        maximum_buffered_service_bytes,
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
        maximum_concurrent_event_dispatches,
        maximum_concurrent_event_callbacks,
    );
    deadline_candidates!(transition, service_call, event_dispatch, shutdown_wait);

    let mut accepted = 0;
    for limits in candidates {
        let constructed = std::panic::catch_unwind(|| {
            Runtime::new(limits).inspect(|runtime| {
                assert!(runtime.root().owner().is_none());
                let snapshot = runtime.resource_snapshot();
                assert_eq!(snapshot.fibers.current, 0);
                assert_eq!(snapshot.service_calls.current, 0);
                assert_eq!(snapshot.buffered_service_bytes.current, 0);
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
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("depth-boundary", "1"),
            ))),
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
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("too-deep", "1"),
            ))),
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
    let descriptor = || PluginDescriptor::new(FactoryIdentity::builtin("deep-config", "1"));
    match scenario {
        "prepare-input" => assert!(matches!(
            runtime.prepare(Arc::new(PassiveFactory(descriptor())), deep_json_value()),
            Err(MetaError::InvalidConfig(message)) if message.contains("nesting")
        )),
        "validate-output" => assert!(matches!(
            runtime.prepare(Arc::new(DeepConfigFactory(descriptor())), Value::Null),
            Err(MetaError::InvalidConfig(message)) if message.contains("nesting")
        )),
        "drop-apply" => {
            let root = runtime.root();
            drop(root.apply(Arc::new(PassiveFactory(descriptor())), deep_json_value()));
        }
        "drop-reconfigure" => {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let fiber = executor
                .block_on(
                    runtime
                        .root()
                        .apply(Arc::new(PassiveFactory(descriptor())), Value::Null),
                )
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
            &snapshot.service_calls,
            limits.execution.maximum_concurrent_service_calls,
        ),
        (
            &snapshot.buffered_service_bytes,
            limits.payloads.maximum_buffered_service_bytes,
        ),
        (
            &snapshot.reconciliations,
            limits.execution.maximum_concurrent_reconciliations,
        ),
        (&snapshot.scheduler_workers, 1),
        (
            &snapshot.event_dispatches,
            limits.execution.maximum_concurrent_event_dispatches,
        ),
        (
            &snapshot.event_callbacks,
            limits.execution.maximum_concurrent_event_callbacks,
        ),
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
struct PassiveFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for PassiveFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct DeepConfigFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for DeepConfigFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    fn validate_config(&self, _: Value) -> Result<Value> {
        Ok(deep_json_value())
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct UnexpectedFactory;

#[async_trait]
impl PluginFactory for UnexpectedFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        panic!("busy admission must precede descriptor observation")
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
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
    let descriptor =
        || {
            PluginDescriptor::new(FactoryIdentity::builtin("reserved", "1")).providing(
                Provision::new("reserved.service", "test.reserved", ContractVersion(1)),
            )
        };

    let prepared = runtime
        .prepare(Arc::new(PassiveFactory(descriptor())), Value::Null)
        .unwrap();
    let reserved = runtime.resource_snapshot();
    assert_eq!(reserved.fibers.current, 1);
    assert!(reserved.retained_plugin_bytes.current > 0);
    assert_eq!(reserved.service_declarations.current, 1);

    let other = Runtime::default();
    assert!(matches!(
        other.root().apply_prepared(prepared).await,
        Err(MetaError::PreparedForDifferentRuntime)
    ));
    assert_eq!(runtime.resource_snapshot().fibers.current, 0);

    let prepared = runtime
        .prepare(Arc::new(PassiveFactory(descriptor())), Value::Null)
        .unwrap();
    let fiber = runtime.root().apply_prepared(prepared).await.unwrap();
    assert_eq!(runtime.resource_snapshot().fibers.current, 1);
    fiber.dispose().await;
    let released = runtime.resource_snapshot();
    assert_eq!(released.fibers.current, 0);
    assert_eq!(released.retained_plugin_bytes.current, 0);
    assert_eq!(released.service_declarations.current, 0);
    assert_eq!(released.fibers.high_watermark, 1);
}

#[test]
fn plugin_capacity_is_reserved_before_descriptor_observation() {
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
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("fiber", "1"),
            ))),
            Value::Null,
        )
        .unwrap();
    assert!(matches!(
        runtime.prepare(Arc::new(UnexpectedFactory), Value::Null),
        Err(MetaError::CapacityExhausted { resource: "fibers" })
    ));
    drop(proof);

    let payloads = PayloadLimits::default();
    let retained_limit = payloads
        .maximum_descriptor_bytes
        .checked_add(payloads.maximum_config_bytes)
        .unwrap();
    let retained_runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 2,
            maximum_fiber_depth: 2,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_retained_plugin_bytes: retained_limit,
            ..payloads
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let proof = retained_runtime
        .prepare(
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("retained", "1"),
            ))),
            Value::Null,
        )
        .unwrap();
    assert!(matches!(
        retained_runtime.prepare(Arc::new(UnexpectedFactory), Value::Null),
        Err(MetaError::CapacityExhausted {
            resource: "retained plugin bytes"
        })
    ));
    let snapshot = retained_runtime.resource_snapshot();
    assert_eq!(snapshot.fibers.current, 1);
    assert_eq!(snapshot.retained_plugin_bytes.rejected, 1);
    drop(proof);
}

#[test]
fn preparation_validates_descriptor_and_json_shape_before_retention() {
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

    let oversized_identity = PluginDescriptor::new(FactoryIdentity::builtin("123456789", "1"));
    assert!(matches!(
        runtime.prepare(Arc::new(PassiveFactory(oversized_identity)), Value::Null),
        Err(MetaError::InvalidInput(_))
    ));

    let too_many_requirements = PluginDescriptor::new(FactoryIdentity::builtin("bounded", "1"))
        .requiring(Requirement::new("first", "contract", ContractVersion(1)))
        .requiring(Requirement::new("second", "contract", ContractVersion(1)));
    assert!(matches!(
        runtime.prepare(Arc::new(PassiveFactory(too_many_requirements)), Value::Null),
        Err(MetaError::InvalidInput(_))
    ));

    let valid = PluginDescriptor::new(FactoryIdentity::builtin("bounded", "1"));
    assert!(matches!(
        runtime.prepare(Arc::new(PassiveFactory(valid.clone())), json!([[[[null]]]])),
        Err(MetaError::InvalidConfig(_))
    ));
    assert!(matches!(
        runtime.prepare(
            Arc::new(PassiveFactory(valid)),
            Value::Array(vec![Value::Null; 9]),
        ),
        Err(MetaError::InvalidConfig(_))
    ));
    assert_eq!(runtime.resource_snapshot().fibers.current, 0);

    for payloads in [
        PayloadLimits {
            maximum_json_depth: 3,
            maximum_json_nodes: 16,
            ..PayloadLimits::default()
        },
        PayloadLimits {
            maximum_json_depth: 4,
            maximum_json_nodes: 8,
            ..PayloadLimits::default()
        },
    ] {
        let descriptor_shape_runtime = Runtime::new(RuntimeLimits {
            payloads,
            ..RuntimeLimits::default()
        })
        .unwrap();
        let nested_descriptor = PluginDescriptor::new(FactoryIdentity::builtin("shape", "1"))
            .requiring(Requirement::new("service", "contract", ContractVersion(1)));
        assert!(matches!(
            descriptor_shape_runtime
                .prepare(Arc::new(PassiveFactory(nested_descriptor)), Value::Null,),
            Err(MetaError::InvalidInput(_))
        ));
    }

    let descriptor_runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_identifier_bytes: 32,
            maximum_descriptor_bytes: 64,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    assert!(matches!(
        descriptor_runtime.prepare(
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("descriptor", "1")
            ))),
            Value::Null,
        ),
        Err(MetaError::InvalidInput(_))
    ));
    assert_eq!(descriptor_runtime.resource_snapshot().fibers.current, 0);
}

#[test]
fn descriptor_config_and_retained_byte_budgets_accept_the_exact_boundary() {
    let descriptor = PluginDescriptor::new(FactoryIdentity::builtin("exact", "1"));
    let descriptor_bytes = serde_json::to_vec(&descriptor).unwrap().len();
    let config_bytes = serde_json::to_vec(&json!("x")).unwrap().len();
    let retained_bytes = descriptor_bytes.checked_add(config_bytes).unwrap();
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_identifier_bytes: "exact".len(),
            maximum_descriptor_bytes: descriptor_bytes,
            maximum_config_bytes: config_bytes,
            maximum_retained_plugin_bytes: retained_bytes,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    let proof = runtime
        .prepare(Arc::new(PassiveFactory(descriptor.clone())), json!("x"))
        .unwrap();
    assert_eq!(
        runtime.resource_snapshot().retained_plugin_bytes.current,
        retained_bytes
    );
    drop(proof);
    assert!(matches!(
        runtime.prepare(Arc::new(PassiveFactory(descriptor)), json!("xx")),
        Err(MetaError::InvalidConfig(_))
    ));
    assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);
}

#[test]
fn prepared_declaration_capacity_is_global_and_released_with_the_proof() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 2,
            maximum_fiber_depth: 2,
            maximum_services: 1,
            maximum_service_declarations: 1,
            maximum_requirements_per_fiber: 1,
            maximum_provisions_per_fiber: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let descriptor = |name: &str| {
        PluginDescriptor::new(FactoryIdentity::builtin(name, "1")).providing(Provision::new(
            name,
            "contract",
            ContractVersion(1),
        ))
    };
    let first = runtime
        .prepare(Arc::new(PassiveFactory(descriptor("first"))), Value::Null)
        .unwrap();
    assert!(matches!(
        runtime.prepare(Arc::new(PassiveFactory(descriptor("second"))), Value::Null,),
        Err(MetaError::CapacityExhausted {
            resource: "service declarations"
        })
    ));
    assert_eq!(runtime.resource_snapshot().service_declarations.rejected, 1);
    drop(first);
    let replacement = runtime
        .prepare(Arc::new(PassiveFactory(descriptor("third"))), Value::Null)
        .unwrap();
    drop(replacement);
    assert_eq!(runtime.resource_snapshot().service_declarations.current, 0);
}

#[test]
fn context_scope_bounds_entries_identifiers_and_encoded_overlay_bytes() {
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

    let payload_runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_context_bytes: 12,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let scoped = payload_runtime
        .root()
        .intercept("first", json!("a"))
        .unwrap();
    assert!(matches!(
        scoped.intercept("second", json!("a")),
        Err(MetaError::PayloadTooLarge { maximum: 12 })
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
    assert!(matches!(
        shape_runtime.root().intercept("bounded", json!([[null]])),
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
    descriptor: PluginDescriptor,
    effects: usize,
}

#[async_trait]
impl PluginFactory for OwnedResourcesFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        for provision in &self.descriptor.provides {
            context.provide(
                provision.key.clone(),
                provision.contract.clone(),
                provision.version,
                Arc::new(NoopEndpoint),
            )?;
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
            Arc::new(OwnedResourcesFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("services", "1"))
                    .providing(Provision::new("first", "test.first", ContractVersion(1)))
                    .providing(Provision::new("second", "test.second", ContractVersion(1))),
                effects: 0,
            }),
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
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("effect", "1")),
            effects: 1,
        })
    };
    let active = effect_runtime
        .root()
        .apply(one_effect(), Value::Null)
        .await
        .unwrap();
    assert_eq!(effect_runtime.resource_snapshot().effects.current, 1);
    let rejected = effect_runtime
        .root()
        .apply(one_effect(), Value::Null)
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
struct BlockingValidationFactory {
    descriptor: PluginDescriptor,
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

#[derive(Debug)]
struct CountingReconfigurationFactory {
    descriptor: PluginDescriptor,
    validations: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct BlockingReactivationFactory {
    descriptor: PluginDescriptor,
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
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, _: Context, config: Arc<Value>) -> Result<()> {
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
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        if !config.is_null() {
            self.validations.fetch_add(1, Ordering::AcqRel);
        }
        Ok(config)
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn reconfiguration_staging_capacity_is_reserved_before_plugin_validation() {
    const MAXIMUM_CONFIG_BYTES: usize = 128;
    let descriptor =
        PluginDescriptor::new(FactoryIdentity::builtin("reconfiguration-staging", "1"));
    let descriptor_bytes = serde_json::to_vec(&descriptor).unwrap().len();
    let initial_bytes = serde_json::to_vec(&Value::Null).unwrap().len();
    let proof_config = Value::String("x".repeat(120));
    let proof_config_bytes = serde_json::to_vec(&proof_config).unwrap().len();
    let retained_limit = descriptor_bytes
        .checked_add(initial_bytes)
        .and_then(|bytes| bytes.checked_add(descriptor_bytes))
        .and_then(|bytes| bytes.checked_add(proof_config_bytes))
        .and_then(|bytes| bytes.checked_add(MAXIMUM_CONFIG_BYTES - 1))
        .unwrap();
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_descriptor_bytes: descriptor_bytes,
            maximum_config_bytes: MAXIMUM_CONFIG_BYTES,
            maximum_retained_plugin_bytes: retained_limit,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let validations = Arc::new(AtomicUsize::new(0));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(CountingReconfigurationFactory {
                descriptor: descriptor.clone(),
                validations: Arc::clone(&validations),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let proof = runtime
        .prepare(Arc::new(PassiveFactory(descriptor)), proof_config)
        .unwrap();
    let before = runtime.resource_snapshot().retained_plugin_bytes;
    assert_eq!(before.limit - before.current, MAXIMUM_CONFIG_BYTES - 1);

    assert_eq!(
        fiber.reconfigure(Value::from(1)).await.unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "retained plugin bytes",
        }
    );
    assert_eq!(validations.load(Ordering::Acquire), 0);
    let rejected = runtime.resource_snapshot().retained_plugin_bytes;
    assert_eq!(rejected.current, before.current);
    assert_eq!(rejected.rejected, before.rejected + 1);

    drop(proof);
    let retained_after_proof = runtime.resource_snapshot().retained_plugin_bytes.current;
    assert!(matches!(
        fiber
            .reconfigure(Value::String("y".repeat(MAXIMUM_CONFIG_BYTES)))
            .await,
        Err(MetaError::InvalidConfig(_))
    ));
    assert_eq!(validations.load(Ordering::Acquire), 0);
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

    let provider_descriptor = || {
        PluginDescriptor::new(FactoryIdentity::builtin("retained-config-provider", "1"))
            .providing(Provision::new("dependency", "test.dependency", V1))
    };
    let consumer_descriptor =
        PluginDescriptor::new(FactoryIdentity::builtin("retained-config-consumer", "1"))
            .requiring(Requirement::new("dependency", "test.dependency", V1));
    let old_config = Value::String("x".repeat(96));
    assert_eq!(
        serde_json::to_vec(&old_config).unwrap().len(),
        OLD_CONFIG_BYTES
    );
    assert_eq!(
        serde_json::to_vec(&Value::Null).unwrap().len(),
        NEW_CONFIG_BYTES
    );

    let provider_bytes = serde_json::to_vec(&provider_descriptor()).unwrap().len();
    let consumer_bytes = serde_json::to_vec(&consumer_descriptor).unwrap().len();
    let retained_limit = provider_bytes
        .checked_add(NEW_CONFIG_BYTES)
        .and_then(|bytes| bytes.checked_add(consumer_bytes))
        .and_then(|bytes| bytes.checked_add(OLD_CONFIG_BYTES))
        .and_then(|bytes| bytes.checked_add(MAXIMUM_CONFIG_BYTES))
        .unwrap();
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_descriptor_bytes: provider_bytes.max(consumer_bytes),
            maximum_config_bytes: MAXIMUM_CONFIG_BYTES,
            maximum_retained_plugin_bytes: retained_limit,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let provider_factory = || {
        Arc::new(EndpointFactory {
            descriptor: provider_descriptor(),
            endpoint: Arc::new(Echo),
        })
    };
    let first_provider = runtime
        .root()
        .apply(provider_factory(), Value::Null)
        .await
        .unwrap();
    let (drop_entered_sender, drop_entered) = mpsc::sync_channel(1);
    let (drop_release, drop_release_receiver) = mpsc::sync_channel(1);
    let retained = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(BlockingReactivationFactory {
                descriptor: consumer_descriptor,
                activations: AtomicUsize::new(0),
                drop_entered: drop_entered_sender,
                drop_release: Arc::new(Mutex::new(drop_release_receiver)),
                retained: Arc::clone(&retained),
            }),
            old_config,
        )
        .await
        .unwrap();

    assert!(first_provider.dispose().await.is_clean());
    let second_provider = runtime
        .root()
        .apply(provider_factory(), Value::Null)
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
        retained_before + NEW_CONFIG_BYTES,
        "the old configuration allocation must remain reserved while activation owns it",
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
impl PluginFactory for BlockingValidationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: Value) -> Result<Value> {
        self.entered.send(()).expect("test waiter still exists");
        self.release
            .lock()
            .expect("release receiver poisoned")
            .recv()
            .expect("test releases validation");
        Ok(config)
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
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
            Arc::new(BlockingValidationFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("blocked", "1")),
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
            Value::Null,
        )
    });
    entered_rx.recv().unwrap();

    assert!(matches!(
        runtime.prepare(Arc::new(UnexpectedFactory), Value::Null,),
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
                Arc::new(BlockingValidationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin("blocked", "1")),
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
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
        .apply(Arc::new(UnexpectedFactory), Value::Null)
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
                Arc::new(BlockingValidationFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin("cancelled", "1")),
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
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
        runtime.prepare(Arc::new(UnexpectedFactory), Value::Null),
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
            Arc::new(PassiveFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("replacement", "1"),
            ))),
            Value::Null,
        )
        .unwrap();
    drop(replacement);
}

#[tokio::test]
async fn pending_reports_bound_cycle_samples_before_snapshot_cloning() {
    const FIBERS: usize = 16;
    const MAXIMUM_ENTRIES: usize = 3;
    const MAXIMUM_BYTES: usize = 20;
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: FIBERS,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_diagnostic_entries: MAXIMUM_ENTRIES,
            maximum_diagnostic_bytes: MAXIMUM_BYTES,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let mut fibers = Vec::with_capacity(FIBERS);
    for index in 0..FIBERS {
        let provided = format!("service-{index:02}");
        let required = format!("service-{:02}", (index + 1) % FIBERS);
        fibers.push(
            runtime
                .root()
                .apply(
                    Arc::new(PassiveFactory(
                        PluginDescriptor::new(FactoryIdentity::builtin(
                            format!("cycle-{index:02}"),
                            "1",
                        ))
                        .requiring(Requirement::new(required, "cycle", ContractVersion(1)))
                        .providing(Provision::new(
                            provided,
                            "cycle",
                            ContractVersion(1),
                        )),
                    )),
                    Value::Null,
                )
                .await
                .unwrap(),
        );
    }

    let snapshot = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = fibers[FIBERS - 1].snapshot();
            if matches!(
                &snapshot.state,
                FiberState::Pending(report)
                    if report.reasons.iter().any(|reason| matches!(
                        reason,
                        PendingReason::DependencyCycle { .. }
                    ))
            ) {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded dependency-cycle diagnostics never converged");
    let FiberState::Pending(report) = &snapshot.state else {
        panic!("cycle participant left pending state");
    };
    assert_bounded_cycle_report(report, MAXIMUM_ENTRIES, MAXIMUM_BYTES);
    let serialized = serde_json::to_value(&snapshot.state).unwrap();
    assert_eq!(serialized["pending"]["total_reasons"], 2);
    assert_eq!(serialized["pending"]["truncated"], true);
    assert!(serialized["pending"]["reasons"].is_array());
    for _ in 0..64 {
        assert_eq!(fibers[FIBERS - 1].snapshot(), snapshot);
    }
}

#[tokio::test]
async fn pending_report_omits_a_dependency_cycle_when_no_sample_entry_fits() {
    const MISSING_REQUIREMENTS: usize = 254;
    let runtime = Runtime::default();
    let mut first_descriptor = PluginDescriptor::new(FactoryIdentity::builtin("cycle-full", "1"));
    for index in 0..MISSING_REQUIREMENTS {
        first_descriptor =
            first_descriptor.requiring(Requirement::new(format!("missing-{index}"), "cycle", V1));
    }
    first_descriptor = first_descriptor
        .requiring(Requirement::new("b", "cycle", V1))
        .providing(Provision::new("a", "cycle", V1));
    let first = runtime
        .root()
        .apply(Arc::new(PassiveFactory(first_descriptor)), Value::Null)
        .await
        .unwrap();
    let _second = runtime
        .root()
        .apply(
            Arc::new(PassiveFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("cycle-peer", "1"))
                    .requiring(Requirement::new("a", "cycle", V1))
                    .providing(Provision::new("b", "cycle", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();

    let report = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let FiberState::Pending(report) = first.snapshot().state
                && report.total_reasons == MISSING_REQUIREMENTS + 2
            {
                break report;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cycle evidence did not converge after the diagnostic prefix filled");

    assert_eq!(report.total_reasons, MISSING_REQUIREMENTS + 2);
    assert_eq!(report.reasons.len(), MISSING_REQUIREMENTS + 1);
    assert!(report.truncated);
    assert!(report.reasons.iter().all(|reason| !matches!(
        reason,
        PendingReason::DependencyCycle { services } if services.is_empty()
    )));
}

fn assert_bounded_cycle_report(
    report: &PendingReport,
    maximum_entries: usize,
    maximum_bytes: usize,
) {
    assert_eq!(report.total_reasons, 2);
    assert!(report.reasons.len() <= maximum_entries);
    assert!(report.truncated);
    let nested_services = report
        .reasons
        .iter()
        .map(|reason| match reason {
            PendingReason::DependencyCycle { services } => services.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert_eq!(nested_services, 1);
    assert!(report.reasons.len() + nested_services <= maximum_entries);
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
            } => service
                .as_str()
                .len()
                .saturating_add(expected.as_str().len())
                .saturating_add(actual.as_str().len()),
            PendingReason::DependencyCycle { services } => {
                assert!(services.len() <= maximum_entries);
                services.iter().map(|service| service.as_str().len()).sum()
            }
        })
        .sum::<usize>();
    assert!(retained_bytes <= maximum_bytes);
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
            Arc::new(PassiveFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("bounded-pending", "1"))
                    .requiring(Requirement::new("abcd", "test", ContractVersion(1)))
                    .requiring(Requirement::new("efgh", "test", ContractVersion(1)))
                    .requiring(Requirement::new("i", "test", ContractVersion(1))),
            )),
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
