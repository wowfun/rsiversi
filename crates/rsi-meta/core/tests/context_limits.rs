use async_trait::async_trait;
use rsi_meta::{
    ContractVersion, InvocationContext, IsolationId, MetaError, PayloadLimits, ProviderChannel,
    Result, Runtime, RuntimeLimits, ServiceEndpoint,
};
use serde_json::Value;
use std::process::Command;
use std::sync::Arc;

#[derive(Debug)]
struct NoopEndpoint;

#[async_trait]
impl ServiceEndpoint for NoopEndpoint {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        Ok(())
    }
}

#[test]
fn context_byte_budget_includes_retained_service_keys() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_context_bytes: 10,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    assert!(matches!(
        runtime.root().isolate("abc", IsolationId(1)),
        Err(MetaError::PayloadTooLarge { maximum: 10 })
    ));
    assert!(matches!(
        runtime.root().intercept("abcdefghij", Value::Null),
        Err(MetaError::PayloadTooLarge { maximum: 10 })
    ));
}

#[test]
fn context_byte_budget_includes_new_intercept_list_delimiters() {
    for maximum in [10, 11] {
        let runtime = Runtime::new(RuntimeLimits {
            payloads: PayloadLimits {
                maximum_context_bytes: maximum,
                ..PayloadLimits::default()
            },
            ..RuntimeLimits::default()
        })
        .unwrap();

        assert_eq!(
            runtime
                .root()
                .intercept("first", Value::from("a"))
                .unwrap_err(),
            MetaError::PayloadTooLarge { maximum }
        );
    }

    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_context_bytes: 12,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    runtime.root().intercept("first", Value::from("a")).unwrap();
}

#[test]
fn context_byte_budget_counts_json_escaped_service_keys() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_context_bytes: 9,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    assert_eq!(
        runtime.root().intercept("\n", Value::Null).unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 9 }
    );
}

#[test]
fn rejected_deep_intercept_values_are_destroyed_without_recursing() {
    const CHILD: &str = "RSI_META_DEEP_INTERCEPT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let runtime = Runtime::default();
        let mut value = Value::Null;
        for _ in 0..100_000 {
            value = Value::Array(vec![value]);
        }
        assert!(matches!(
            runtime.root().intercept("service", value),
            Err(MetaError::InvalidInput(message)) if message.contains("nesting")
        ));
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .env(CHILD, "1")
        .args([
            "--exact",
            "rejected_deep_intercept_values_are_destroyed_without_recursing",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "deep rejected Value crashed the process:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn service_apis_validate_identifiers_before_resolving_context_ownership() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_identifier_bytes: 3,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let root = runtime.root();

    assert!(matches!(
        root.service("long"),
        Err(MetaError::InvalidInput(_))
    ));
    assert!(matches!(
        root.provide("long", "id", ContractVersion(1), Arc::new(NoopEndpoint)),
        Err(MetaError::InvalidInput(_))
    ));
    assert!(matches!(
        root.provide("svc", "long", ContractVersion(1), Arc::new(NoopEndpoint)),
        Err(MetaError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn completed_shutdown_rejects_context_structural_mutations_before_validation() {
    let runtime = Runtime::default();
    let root = runtime.root();
    assert!(runtime.shutdown().await.is_complete());
    let resources = runtime.resource_snapshot();
    let revision = runtime.snapshot().revision;
    let oversized_key = "x".repeat(PayloadLimits::default().maximum_identifier_bytes + 1);

    assert!(matches!(
        root.clone().isolate("echo", IsolationId(1)),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert!(matches!(
        root.clone().isolate_fresh("echo"),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert!(matches!(
        root.clone().intercept("echo", Value::Null),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert!(matches!(
        root.isolate_fresh(oversized_key),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert_eq!(runtime.resource_snapshot(), resources);
    assert_eq!(runtime.snapshot().revision, revision);
}

#[path = "context_limits/contract_invariants.rs"]
mod contract_invariants;
