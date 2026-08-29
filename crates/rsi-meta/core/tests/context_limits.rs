use async_trait::async_trait;
use rsi_meta::{
    ContractVersion, InvocationContext, IsolationId, LocalIsolationId, MetaError, PayloadLimits,
    ProviderChannel, Result, Runtime, RuntimeLimits, ServiceEndpoint,
};
use std::any::TypeId;
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
fn context_byte_budget_includes_retained_service_isolation_keys() {
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
    assert!(matches!(
        root.clone()
            .isolate_local_type(TypeId::of::<NoopEndpoint>(), "long", LocalIsolationId(1)),
        Err(MetaError::InvalidInput(_))
    ));
    assert!(matches!(
        root.isolate_event_type(TypeId::of::<NoopEndpoint>(), "long", LocalIsolationId(1)),
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
        root.isolate_fresh(oversized_key),
        Err(MetaError::RuntimeShuttingDown)
    ));
    assert_eq!(runtime.resource_snapshot(), resources);
    assert_eq!(runtime.snapshot().revision, revision);
}
