use rsi_inspector::{PageRequest, RuntimeRequest};

#[test]
fn requests_reject_noncanonical_cursors_and_out_of_range_pages() {
    for cursor in ["", "00", "01", "-1", "+1", "18446744073709551616"] {
        let request = RuntimeRequest {
            after: Some(cursor.into()),
            ..Default::default()
        };
        assert!(request.validate().is_err(), "{cursor}");
    }
    assert!(
        RuntimeRequest {
            after: Some(u64::MAX.to_string()),
            ..Default::default()
        }
        .validate()
        .is_ok()
    );
    assert!(
        PageRequest {
            offset: 0,
            limit: 0
        }
        .validate()
        .is_err()
    );
    assert!(
        PageRequest {
            offset: 0,
            limit: 129
        }
        .validate()
        .is_err()
    );
    assert!(serde_json::from_str::<RuntimeRequest>(r#"{"config":"secret"}"#).is_err());
}

use rsi_api::ApiRegistry;
use rsi_api_protocol::{ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_inspector::{FactoryDeclaration, InspectorApi, InspectorSource, NativeObservation};
use rsi_meta::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct Source(AtomicUsize);
impl InspectorSource for Source {
    fn runtime(&self, _: InspectionRequest) -> rsi_api_protocol::Result<RuntimeInspection> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let service = || InspectedService::Local {
            key: LocalContractKey::new("fixture.local"),
            isolation: LocalIsolationId(u64::MAX),
        };
        let owner = || InspectedOwner {
            fiber: FiberId(u64::MAX - 1),
            generation: FiberGeneration(u64::MAX),
        };
        Ok(RuntimeInspection {
            revision: u64::MAX,
            shutting_down: false,
            terminal: false,
            total_fibers: 1,
            next_after: Some(FiberId(u64::MAX)),
            resources: None,
            fibers: vec![InspectedFiber {
                id: FiberId(u64::MAX),
                generation: FiberGeneration(u64::MAX),
                factory: FactoryIdentity::native("fixture.inspect", "a".repeat(64)),
                update_mode: UpdateMode::Replayable,
                state: InspectedFiberState::Failed,
                parent: Some(owner()),
                order: Some(vec![u64::MAX]),
                dependencies: InspectedCollection {
                    total: 1,
                    items: vec![InspectedDependency {
                        service: service(),
                        provider: Some(InspectedProvider {
                            owner: owner(),
                            supply_token: u64::MAX,
                        }),
                    }],
                },
                supplies: InspectedCollection {
                    total: 1,
                    items: vec![InspectedSupply {
                        service: service(),
                        supply_token: u64::MAX,
                        generation_published: true,
                    }],
                },
                effects: InspectedCollection {
                    total: 1,
                    items: vec![InspectedEffect {
                        id: u64::MAX,
                        open: false,
                        cleanup: InspectedCleanupState::Complete,
                        cleanup_failures: Some(1),
                        queued_entries: 0,
                    }],
                },
                retained_effect_entries: 0,
                retained_effect_transactions: 1,
                cleanup_phase: Some(CleanupPhase::RunningEffects),
                listeners: 0,
                children: 0,
            }],
        })
    }
    fn profile(
        &self,
    ) -> rsi_api_protocol::Result<(
        rsi_meta_profile::ProfileStatus,
        rsi_meta_profile::ProfileSnapshot,
    )> {
        Err(ApiError::Unavailable)
    }
    fn factories(&self) -> &[FactoryDeclaration] {
        &[]
    }
    fn native(&self) -> rsi_api_protocol::Result<NativeObservation> {
        Err(ApiError::Unavailable)
    }
}

#[tokio::test]
async fn wire_preserves_large_identities_and_validates_before_source_then_withdraws() {
    let registry = ApiRegistry::new(Execution::native(tokio::runtime::Handle::current()));
    let source = Arc::new(Source::default());
    let api = InspectorApi::register(&registry, source.clone()).unwrap();
    let operations = registry.operations();
    assert_eq!(operations.len(), 4);
    let runtime = operations
        .iter()
        .find(|operation| operation.id.name() == "runtime")
        .unwrap();
    for raw in [
        r#"{"after":"01"}"#,
        r#"{"maximum_fibers":65}"#,
        r#"{"maximum_items":0}"#,
        r#"{"secret":"config"}"#,
    ] {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let bytes = ByteBudget::new(1024).unwrap().encode(&value, 1024).unwrap();
        assert!(
            registry
                .admit(&runtime.id, CallOrigin::Local)
                .unwrap()
                .invoke(bytes)
                .await
                .is_err()
        );
    }
    assert_eq!(source.0.load(Ordering::SeqCst), 0);
    let bytes = ByteBudget::new(1024)
        .unwrap()
        .encode(&RuntimeRequest::default(), 1024)
        .unwrap();
    let ApiOutput::Reply(reply) = registry
        .admit(&runtime.id, CallOrigin::Local)
        .unwrap()
        .invoke(bytes)
        .await
        .unwrap()
    else {
        panic!("finite reply required");
    };
    let document: serde_json::Value = serde_json::from_slice(reply.json.as_bytes()).unwrap();
    for pointer in [
        "/revision",
        "/next_after",
        "/fibers/0/id",
        "/fibers/0/generation",
        "/fibers/0/order/0",
        "/fibers/0/dependencies/items/0/provider/supply_token",
        "/fibers/0/dependencies/items/0/service/isolation",
        "/fibers/0/supplies/items/0/supply_token",
        "/fibers/0/effects/items/0/id",
    ] {
        assert_eq!(
            document.pointer(pointer).unwrap(),
            &serde_json::json!(u64::MAX.to_string()),
            "{pointer}"
        );
    }
    assert_eq!(document["fibers"][0]["state"], "failed");
    assert_eq!(source.0.load(Ordering::SeqCst), 1);
    drop(reply);
    api.close().await;
    assert!(registry.operations().is_empty());
    assert!(registry.admit(&runtime.id, CallOrigin::Local).is_err());
}
