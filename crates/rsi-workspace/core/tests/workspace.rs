#[path = "../../../../fixtures/rsi/workspace-access/resolver.rs"]
mod access;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::StorageFactory;
use rsi_storage_domain::DomainFactory;
use rsi_storage_json::JsonStorageFactory;
use rsi_workspace::WorkspaceFactory;
use rsi_workspace_protocol::{WorkspaceRegistryContract, WorkspaceStatus};
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

async fn storage_runtime(path: &std::path::Path) -> Runtime {
    let runtime = Runtime::default();
    provide_access(&runtime).await;
    for (id, factory, config) in [
        (
            "storage",
            Arc::new(StorageFactory) as Arc<dyn rsi_meta::PluginFactory>,
            Value::Null,
        ),
        (
            "json",
            Arc::new(JsonStorageFactory),
            json!({"name":"json","path":path}),
        ),
        ("domains", Arc::new(DomainFactory), Value::Null),
    ] {
        runtime
            .root()
            .apply(linked(id, factory), config)
            .await
            .unwrap();
    }
    runtime
}

async fn assert_membership(
    registry: &dyn rsi_workspace_protocol::WorkspaceRegistry,
    expected: &[rsi_workspace_protocol::WorkspaceRecord],
) {
    let seed = registry.order_seed().await.unwrap();
    seed.validate().unwrap();
    let rsi_workspace_protocol::WorkspaceOrderSeed::Available { records } = seed else {
        panic!("membership fits")
    };
    assert_eq!(
        records
            .iter()
            .map(|record| &record.id)
            .collect::<std::collections::BTreeSet<_>>(),
        expected.iter().map(|record| &record.id).collect()
    );
}

#[tokio::test]
async fn cursor_survives_deletion_of_highest_orders_and_registry_restart() {
    let temporary = tempfile::tempdir().unwrap();
    for name in ["first", "second", "third", "after_restart", "after_empty"] {
        fs::create_dir(temporary.path().join(name)).unwrap();
    }
    let runtime = storage_runtime(&temporary.path().join("registry.json")).await;
    let config = json!({"backend":"json"});
    let mut fiber = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            config.clone(),
        )
        .await
        .unwrap();
    let mut registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    let first = registry
        .get_or_create(&temporary.path().join("first"))
        .await
        .unwrap();
    let second = registry
        .get_or_create(&temporary.path().join("second"))
        .await
        .unwrap();
    let third = registry
        .get_or_create(&temporary.path().join("third"))
        .await
        .unwrap();
    assert_membership(
        registry.as_ref(),
        &[first.clone(), second.clone(), third.clone()],
    )
    .await;
    let cursor = registry.list(None, 2).await.unwrap().next.unwrap();
    assert!(registry.delete_registration(&second.id).await.unwrap());
    assert!(registry.delete_registration(&third.id).await.unwrap());
    drop(registry);
    assert!(fiber.dispose().await.is_clean());
    fiber = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            config.clone(),
        )
        .await
        .unwrap();
    registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    let after_restart = registry
        .get_or_create(&temporary.path().join("after_restart"))
        .await
        .unwrap();
    assert_eq!(
        registry.list(Some(cursor), 2).await.unwrap().records,
        vec![after_restart.clone()]
    );
    assert!(registry.delete_registration(&first.id).await.unwrap());
    assert!(
        registry
            .delete_registration(&after_restart.id)
            .await
            .unwrap()
    );
    drop(registry);
    assert!(fiber.dispose().await.is_clean());
    runtime
        .root()
        .apply(linked("workspace", Arc::new(WorkspaceFactory)), config)
        .await
        .unwrap();
    registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    let after_empty = registry
        .get_or_create(&temporary.path().join("after_empty"))
        .await
        .unwrap();
    assert_eq!(
        registry.list(Some(cursor), 2).await.unwrap().records,
        vec![after_empty]
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One cold registry lifecycle proves canonicalization and non-destructive deletion.
async fn registration_is_canonical_durable_and_delete_never_touches_files() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_path = temporary.path().join("work");
    let second_workspace_path = temporary.path().join("work-two");
    fs::create_dir(&workspace_path).unwrap();
    fs::create_dir(&second_workspace_path).unwrap();
    let user_file = workspace_path.join("keep.txt");
    fs::write(&user_file, b"keep").unwrap();
    let runtime = Runtime::default();
    provide_access(&runtime).await;
    let hub = runtime
        .root()
        .apply(linked("storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let backend = runtime
        .root()
        .apply(
            linked("storage-json", Arc::new(JsonStorageFactory)),
            json!({"name":"json","path":temporary.path().join("domains.json")}),
        )
        .await
        .unwrap();
    let domains = runtime
        .root()
        .apply(linked("domains", Arc::new(DomainFactory)), Value::Null)
        .await
        .unwrap();
    let workspace_config = json!({"backend":"json"});
    let workspace = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            workspace_config.clone(),
        )
        .await
        .unwrap();
    let registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    let first = registry.get_or_create(&workspace_path).await.unwrap();
    let second = registry
        .get_or_create(&workspace_path.join("."))
        .await
        .unwrap();
    assert_eq!(first, second);
    let third = registry
        .get_or_create(&second_workspace_path)
        .await
        .unwrap();
    assert_eq!(registry.get(&first.id).await.unwrap(), first);
    let cursor = assert_bounded_pages(registry.as_ref(), &first, &third).await;
    assert_eq!(
        registry.status(&first.id).await.unwrap(),
        WorkspaceStatus::Ok
    );
    drop(registry);
    assert!(workspace.dispose().await.is_clean());
    let workspace = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            workspace_config,
        )
        .await
        .unwrap();
    let registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    assert_eq!(
        registry.list(None, 256).await.unwrap().records,
        vec![first.clone(), third.clone()]
    );
    assert!(registry.delete_registration(&first.id).await.unwrap());
    assert_eq!(
        registry.list(Some(cursor), 1).await.unwrap().records,
        vec![third.clone()]
    );
    fs::remove_dir(&second_workspace_path).unwrap();
    assert_eq!(registry.get(&third.id).await.unwrap(), third);
    assert_eq!(
        registry.status(&third.id).await.unwrap(),
        WorkspaceStatus::MissingDirectory
    );
    #[cfg(unix)]
    assert_replaced_directory_is_missing(
        registry.as_ref(),
        &third,
        &workspace_path,
        &second_workspace_path,
    )
    .await;
    assert!(user_file.is_file());
    assert!(workspace_path.is_dir());
    assert_eq!(registry.list(None, 256).await.unwrap().records, vec![third]);

    drop(registry);
    assert!(workspace.dispose().await.is_clean());
    assert!(domains.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[cfg(unix)]
async fn assert_replaced_directory_is_missing(
    registry: &dyn rsi_workspace_protocol::WorkspaceRegistry,
    record: &rsi_workspace_protocol::WorkspaceRecord,
    target: &std::path::Path,
    path: &std::path::Path,
) {
    std::os::unix::fs::symlink(target, path).unwrap();
    assert_eq!(
        registry.status(&record.id).await.unwrap(),
        WorkspaceStatus::MissingDirectory
    );
    fs::remove_file(path).unwrap();
}

async fn assert_bounded_pages(
    registry: &dyn rsi_workspace_protocol::WorkspaceRegistry,
    first: &rsi_workspace_protocol::WorkspaceRecord,
    second: &rsi_workspace_protocol::WorkspaceRecord,
) -> rsi_workspace_protocol::WorkspaceCursor {
    assert!(
        matches!(registry.get_or_create(std::path::Path::new(".")).await,
        Err(rsi_workspace_protocol::WorkspaceError::InvalidInput(error)) if error.contains("absolute"))
    );
    first.validate().unwrap();
    let mut misbound = first.clone();
    misbound.coordinates.clone_from(&second.coordinates);
    assert!(matches!(
        misbound.validate(),
        Err(rsi_workspace_protocol::WorkspaceError::Corrupt(_))
    ));
    let page = registry.list(None, 1).await.unwrap();
    assert_eq!(page.records, vec![first.clone()]);
    let cursor = page.next.expect("second registration remains");
    let next = registry.list(Some(cursor), 1).await.unwrap();
    assert_eq!(next.records, vec![second.clone()]);
    assert!(next.next.is_none());
    assert!(registry.list(None, 0).await.is_err());
    assert!(registry.list(None, 257).await.is_err());
    assert_eq!(
        registry.list(None, 256).await.unwrap().records,
        vec![first.clone(), second.clone()]
    );
    cursor
}

#[derive(Debug)]
struct FaultDomain {
    spec: rsi_storage_domain::DomainSpec,
    values: tokio::sync::Mutex<std::collections::BTreeMap<String, Value>>,
    fail_registration: std::sync::atomic::AtomicBool,
    pause: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
    failure: std::sync::Mutex<Option<rsi_storage::StorageError>>,
}
#[async_trait::async_trait]
impl rsi_storage_domain::Domain for FaultDomain {
    fn ensure_available(&self) -> Result<(), rsi_storage::StorageError> {
        self.failure.lock().unwrap().clone().map_or(Ok(()), Err)
    }
    fn spec(&self) -> &rsi_storage_domain::DomainSpec {
        &self.spec
    }
    async fn snapshot(
        &self,
    ) -> Result<std::collections::BTreeMap<String, Value>, rsi_storage::StorageError> {
        Ok(self.values.lock().await.clone())
    }
    async fn put(&self, key: &str, value: Value) -> Result<(), rsi_storage::StorageError> {
        if key != "allocation"
            && self
                .fail_registration
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(rsi_storage::StorageError::Io(
                "injected registration failure".into(),
            ));
        }
        self.pause_commit().await;
        self.values.lock().await.insert(key.into(), value);
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<bool, rsi_storage::StorageError> {
        self.pause_commit().await;
        Ok(self.values.lock().await.remove(key).is_some())
    }
}
#[derive(Debug)]
struct FaultFacility(Arc<FaultDomain>);
#[async_trait::async_trait]
impl rsi_storage_domain::DomainFacility for FaultFacility {
    async fn open(
        &self,
        spec: rsi_storage_domain::DomainSpec,
    ) -> Result<Arc<dyn rsi_storage_domain::Domain>, rsi_storage::StorageError> {
        assert_eq!(spec, self.0.spec);
        Ok(self.0.clone())
    }
}
#[tokio::test]
async fn failed_registration_leaves_a_reserved_gap_and_invalid_allocation_is_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    for name in ["failed", "kept"] {
        fs::create_dir(temporary.path().join(name)).unwrap();
    }
    let runtime = Runtime::default();
    provide_access(&runtime).await;
    let domain = Arc::new(FaultDomain {
        spec: rsi_storage_domain::DomainSpec {
            id: "rsi.workspace".into(),
            backend: "test".into(),
            version: 4,
            maximum_records: 16_385,
            maximum_bytes: 128 * 1024 * 1024,
        },
        values: tokio::sync::Mutex::new(std::collections::BTreeMap::default()),
        fail_registration: std::sync::atomic::AtomicBool::new(true),
        failure: std::sync::Mutex::new(None),
        pause: std::sync::atomic::AtomicBool::default(),
        entered: tokio::sync::Notify::default(),
        resume: tokio::sync::Notify::default(),
    });
    runtime
        .root()
        .apply(
            linked("domains", Arc::new(FaultFacility(domain.clone()))),
            Value::Null,
        )
        .await
        .unwrap();
    let fiber = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            json!({"backend":"test"}),
        )
        .await
        .unwrap();
    let registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    assert!(matches!(
        registry
            .get_or_create(&temporary.path().join("failed"))
            .await,
        Err(rsi_workspace_protocol::WorkspaceError::Api(
            rsi_api_protocol::ApiError::Unavailable
        ))
    ));
    assert!(registry.list(None, 10).await.unwrap().records.is_empty());
    assert_eq!(
        domain.values.lock().await["allocation"],
        json!({"high_water":1})
    );
    let kept = registry
        .get_or_create(&temporary.path().join("kept"))
        .await
        .unwrap();
    assert_eq!(
        registry
            .list(
                Some(rsi_workspace_protocol::WorkspaceCursor { after_order: 1 }),
                10
            )
            .await
            .unwrap()
            .records,
        vec![kept.clone()]
    );
    assert_storage_errors(registry.as_ref(), &domain, temporary.path(), &kept).await;
    drop(registry);
    assert!(fiber.dispose().await.is_clean());
    assert_invalid_allocation(&runtime, &domain).await;
    assert_exhaustion_and_divergence(&runtime, &domain, temporary.path(), &kept).await;
    assert!(runtime.shutdown().await.is_clean());
}

async fn assert_invalid_allocation(runtime: &Runtime, domain: &FaultDomain) {
    for bad in [
        Some(json!({"high_water":0})),
        Some(json!({"high_water":"2"})),
        None,
    ] {
        let mut records = domain.values.lock().await;
        if let Some(bad) = bad {
            records.insert("allocation".into(), bad);
        } else {
            records.remove("allocation");
        }
        drop(records);
        let failed = runtime
            .root()
            .apply(
                linked("workspace", Arc::new(WorkspaceFactory)),
                json!({"backend":"test"}),
            )
            .await
            .unwrap();
        assert!(matches!(
            failed.snapshot().state,
            rsi_meta::FiberState::Failed(_)
        ));
        assert!(
            runtime
                .root()
                .lookup_local::<WorkspaceRegistryContract>()
                .is_none()
        );
        assert!(failed.dispose().await.is_clean());
    }
}

async fn assert_storage_errors(
    registry: &dyn rsi_workspace_protocol::WorkspaceRegistry,
    domain: &FaultDomain,
    root: &std::path::Path,
    kept: &rsi_workspace_protocol::WorkspaceRecord,
) {
    for (error, expected) in [
        (
            rsi_storage::StorageError::RecoveryRequired,
            rsi_api_protocol::ApiError::Unavailable,
        ),
        (
            rsi_storage::StorageError::OutcomeUnknown("lost result".into()),
            rsi_api_protocol::ApiError::OutcomeUnknown,
        ),
    ] {
        *domain.failure.lock().unwrap() = Some(error);
        assert_eq!(
            registry.get(&kept.id).await,
            Err(rsi_workspace_protocol::WorkspaceError::Api(
                expected.clone()
            ))
        );
        assert_eq!(
            registry.list(None, 10).await,
            Err(rsi_workspace_protocol::WorkspaceError::Api(
                expected.clone()
            ))
        );
        assert_eq!(
            registry.get_or_create(&root.join("kept")).await,
            Err(rsi_workspace_protocol::WorkspaceError::Api(
                expected.clone()
            ))
        );
        assert_eq!(
            registry.delete_registration(&kept.id).await,
            Err(rsi_workspace_protocol::WorkspaceError::Api(expected))
        );
    }
    *domain.failure.lock().unwrap() = None;
}

async fn assert_exhaustion_and_divergence(
    runtime: &Runtime,
    domain: &FaultDomain,
    root: &std::path::Path,
    kept: &rsi_workspace_protocol::WorkspaceRecord,
) {
    domain
        .values
        .lock()
        .await
        .insert("allocation".into(), json!({"high_water":u64::MAX}));
    let fiber = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            json!({"backend":"test"}),
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
    let registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    let before = domain.values.lock().await.clone();
    assert!(matches!(registry.get_or_create(&root.join("failed")).await,
        Err(rsi_workspace_protocol::WorkspaceError::InvalidInput(error)) if error.contains("exhausted")));
    assert_eq!(*domain.values.lock().await, before);
    assert_eq!(
        registry.list(None, 10).await.unwrap().records,
        vec![kept.clone()]
    );
    // A provider divergence must not be reported as successful deletion.
    domain.values.lock().await.remove(kept.id.as_str());
    assert!(matches!(
        registry.delete_registration(&kept.id).await,
        Err(rsi_workspace_protocol::WorkspaceError::Corrupt(_))
    ));
    assert_eq!(registry.get(&kept.id).await.unwrap(), *kept);
}

#[async_trait::async_trait]
impl rsi_meta::PluginFactory for FaultFacility {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<rsi_storage_domain::DomainFacilityContract>(Arc::new(Self(
                self.0.clone(),
            )))?;
        plan.defer(
            "withdraw test domain",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn version_three_rejection_preserves_the_entire_backend_byte_for_byte() {
    use sha2::{Digest, Sha256};
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    let path = directory.to_str().unwrap();
    let id = hex::encode(Sha256::digest(path.as_bytes()));
    let file = temporary.path().join("registry.json");
    let original = serde_json::to_vec_pretty(&json!({"format":1,"domains":{
        "rsi.workspace":{"version":3,"records":{id.clone():{"order":9,"record":{"id":id,"path":path}}}},
        "another.domain":{"version":1,"records":{"keep":{"value":"unchanged"}}}
    }})).unwrap();
    fs::write(&file, &original).unwrap();
    let runtime = Runtime::default();
    provide_access(&runtime).await;
    runtime
        .root()
        .apply(linked("storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("json", Arc::new(JsonStorageFactory)),
            json!({"name":"json","path":file}),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(linked("domains", Arc::new(DomainFactory)), Value::Null)
        .await
        .unwrap();
    let fiber = runtime
        .root()
        .apply(
            linked("workspace", Arc::new(WorkspaceFactory)),
            json!({"backend":"json"}),
        )
        .await
        .unwrap();
    let rsi_meta::FiberState::Failed(error) = fiber.snapshot().state else {
        panic!("v3 must fail activation")
    };
    assert!(error.contains("version 3, expected 4"), "{error}");
    assert!(
        runtime
            .root()
            .lookup_local::<WorkspaceRegistryContract>()
            .is_none()
    );
    assert_eq!(fs::read(&file).unwrap(), original);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(fs::read(&file).unwrap(), original);
}

#[tokio::test]
async fn cold_registry_preserves_equal_paths_on_distinct_machines_without_local_remote_probes() {
    use rsi_workspace_protocol::{
        ExecutionCoordinates, ExecutionLocation, WorkspaceId, WorkspaceOrderSeed, WorkspaceRecord,
    };
    let temporary = tempfile::tempdir().unwrap();
    let canonical = temporary.path().canonicalize().unwrap();
    let path = canonical.to_str().unwrap();
    let records = [
        ExecutionLocation::Local,
        ExecutionLocation::Ssh {
            target: serde_json::from_value(json!("a".repeat(32))).unwrap(),
        },
        ExecutionLocation::Ssh {
            target: serde_json::from_value(json!("b".repeat(32))).unwrap(),
        },
    ]
    .map(|location| WorkspaceRecord::new(ExecutionCoordinates::new(location, path).unwrap()));
    let mut durable = serde_json::Map::new();
    durable.insert("allocation".into(), json!({"high_water":3}));
    for (index, record) in records.iter().enumerate() {
        durable.insert(
            record.id.to_string(),
            json!({"order":index+1,"record":record}),
        );
    }
    let file = temporary.path().join("registry.json");
    fs::write(
        &file,
        serde_json::to_vec(
            &json!({"format":1,"domains":{"rsi.workspace":{"version":4,"records":durable}}}),
        )
        .unwrap(),
    )
    .unwrap();
    let runtime = Runtime::default();
    provide_access(&runtime).await;
    for (id, factory, config) in [
        (
            "storage",
            Arc::new(StorageFactory) as Arc<dyn rsi_meta::PluginFactory>,
            Value::Null,
        ),
        (
            "json",
            Arc::new(JsonStorageFactory),
            json!({"name":"json","path":file}),
        ),
        ("domains", Arc::new(DomainFactory), Value::Null),
        (
            "workspace",
            Arc::new(WorkspaceFactory),
            json!({"backend":"json"}),
        ),
    ] {
        runtime
            .root()
            .apply(linked(id, factory), config)
            .await
            .unwrap();
    }
    let registry = runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    assert_eq!(registry.list(None, 64).await.unwrap().records, records);
    let WorkspaceOrderSeed::Available { records: seed } = registry.order_seed().await.unwrap()
    else {
        panic!("three fit")
    };
    assert_eq!(seed.len(), 3);
    assert_eq!(
        WorkspaceId::from_canonical_path(&canonical).unwrap(),
        records[0].id
    );
    assert_eq!(
        registry.status(&records[0].id).await.unwrap(),
        WorkspaceStatus::Ok
    );
    for record in &records[1..] {
        assert_eq!(registry.get(&record.id).await.unwrap(), *record);
        assert!(
            matches!(
                registry.status(&record.id).await,
                Err(rsi_workspace_protocol::WorkspaceError::Api(
                    rsi_api_protocol::ApiError::Unavailable
                ))
            ),
            "native registry must not test the matching Local directory for SSH status"
        );
    }
    drop(registry);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One durable snapshot proves filtering, offline status and revocation through the same view.
async fn scoped_registry_filters_before_paging_and_seed_budget_and_rechecks_revocation() {
    use rsi_api_protocol::{ApiError, AuthenticatedDevice, CallOrigin, DeviceId};
    use rsi_workspace_protocol::{
        ExecutionCoordinates, ExecutionLocation, WorkspaceError, WorkspaceIngressContract,
        WorkspaceOrderSeed, WorkspaceRecord,
    };
    use std::sync::atomic::Ordering;
    let temporary = tempfile::tempdir().unwrap();
    let local_path = temporary.path().canonicalize().unwrap();
    let local = WorkspaceRecord::new(
        ExecutionCoordinates::new(ExecutionLocation::Local, local_path.to_str().unwrap()).unwrap(),
    );
    let remote_location = ExecutionLocation::Ssh {
        target: serde_json::from_value(json!("a".repeat(32))).unwrap(),
    };
    let remote = WorkspaceRecord::new(
        ExecutionCoordinates::new(remote_location.clone(), local_path.to_str().unwrap()).unwrap(),
    );
    // Hidden entries exceed the full manual-order seed budget, before the visible row.
    let mut values = std::collections::BTreeMap::new();
    values.insert("allocation".into(), json!({"high_water":1027}));
    for index in 0..1025 {
        let record = WorkspaceRecord::new(
            ExecutionCoordinates::new(remote_location.clone(), format!("/private/remote/{index}"))
                .unwrap(),
        );
        values.insert(
            record.id.to_string(),
            json!({"order":index+1,"record":record}),
        );
    }
    values.insert(remote.id.to_string(), json!({"order":1026,"record":remote}));
    values.insert(local.id.to_string(), json!({"order":1027,"record":local}));
    let domain = Arc::new(FaultDomain {
        spec: rsi_storage_domain::DomainSpec {
            id: "rsi.workspace".into(),
            backend: "test".into(),
            version: 4,
            maximum_records: 16_385,
            maximum_bytes: 128 * 1024 * 1024,
        },
        values: tokio::sync::Mutex::new(values),
        fail_registration: std::sync::atomic::AtomicBool::new(false),
        failure: std::sync::Mutex::new(None),
        pause: std::sync::atomic::AtomicBool::default(),
        entered: tokio::sync::Notify::default(),
        resume: tokio::sync::Notify::default(),
    });
    let resolver = Arc::new(access::Resolver::default());
    let runtime = Runtime::default();
    for (id, factory, config) in [
        (
            "execution",
            Arc::new(access::Factory(resolver.clone())) as Arc<dyn rsi_meta::PluginFactory>,
            Value::Null,
        ),
        (
            "domains",
            Arc::new(FaultFacility(domain.clone())),
            Value::Null,
        ),
        (
            "workspace",
            Arc::new(WorkspaceFactory),
            json!({"backend":"test"}),
        ),
    ] {
        runtime
            .root()
            .apply(linked(id, factory), config)
            .await
            .unwrap();
    }
    let revoked = tokio_util::sync::CancellationToken::new();
    let scoped = runtime
        .root()
        .lookup_local::<WorkspaceIngressContract>()
        .unwrap()
        .scoped(CallOrigin::Device(AuthenticatedDevice {
            id: DeviceId::from_bytes([7; 16]),
            revoked: revoked.clone(),
        }));
    let page = scoped.list(None, 1).await.unwrap();
    page.validate(None, 1).unwrap();
    assert_eq!(page.records.as_slice(), std::slice::from_ref(&local));
    assert_eq!(page.next, None);
    assert_eq!(
        scoped.order_seed().await.unwrap(),
        WorkspaceOrderSeed::Available {
            records: vec![local.clone()]
        }
    );
    for result in [
        scoped.get(&remote.id).await.map(|_| ()),
        scoped.status(&remote.id).await.map(|_| ()),
        scoped.delete_registration(&remote.id).await.map(|_| ()),
        scoped
            .register_at(&remote_location, &local_path)
            .await
            .map(|_| ()),
    ] {
        assert_eq!(result, Err(WorkspaceError::Api(ApiError::Unauthorized)));
    }
    resolver.ssh_use.store(true, Ordering::SeqCst);
    assert_eq!(scoped.get(&remote.id).await.unwrap(), remote);
    assert!(
        matches!(
            scoped.status(&remote.id).await,
            Err(WorkspaceError::Api(ApiError::Unavailable))
        ),
        "offline remote path must not use existing Local directory"
    );
    assert!(matches!(
        scoped.order_seed().await.unwrap(),
        WorkspaceOrderSeed::TooLarge { .. }
    ));
    let first = scoped.list(None, 1).await.unwrap();
    let second = scoped.list(first.next, 1).await.unwrap();
    assert_ne!(first.records[0].id, second.records[0].id);
    resolver.ssh_use.store(false, Ordering::SeqCst);
    assert_eq!(
        scoped.get(&remote.id).await,
        Err(WorkspaceError::Api(ApiError::Unauthorized))
    );
    assert_eq!(scoped.list(first.next, 1).await.unwrap().records, [local]);
    revoked.cancel();
    assert_eq!(
        scoped.list(None, 1).await,
        Err(WorkspaceError::Api(ApiError::Unauthorized))
    );
    assert_eq!(
        domain.values.lock().await.len(),
        1028,
        "denied mutations perform zero domain writes"
    );
    assert!(local_path.is_dir());
    assert!(runtime.shutdown().await.is_clean());
}

async fn provide_access(runtime: &Runtime) {
    runtime
        .root()
        .apply(
            linked(
                "execution",
                Arc::new(access::Factory(Arc::new(access::Resolver::default()))),
            ),
            Value::Null,
        )
        .await
        .unwrap();
}

impl FaultDomain {
    async fn pause_commit(&self) {
        if self.pause.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume.notified().await;
        }
    }
}

#[tokio::test]
async fn retirement_drains_abandoned_registration_and_deletion_before_withdrawal() {
    for delete in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Runtime::default();
        provide_access(&runtime).await;
        let domain = Arc::new(FaultDomain {
            spec: rsi_storage_domain::DomainSpec {
                id: "rsi.workspace".into(),
                backend: "test".into(),
                version: 4,
                maximum_records: 16_385,
                maximum_bytes: 128 * 1024 * 1024,
            },
            values: tokio::sync::Mutex::default(),
            fail_registration: std::sync::atomic::AtomicBool::default(),
            failure: std::sync::Mutex::default(),
            pause: std::sync::atomic::AtomicBool::default(),
            entered: tokio::sync::Notify::default(),
            resume: tokio::sync::Notify::default(),
        });
        runtime
            .root()
            .apply(
                linked("domains", Arc::new(FaultFacility(domain.clone()))),
                Value::Null,
            )
            .await
            .unwrap();
        let fiber = runtime
            .root()
            .apply(
                linked("workspace", Arc::new(WorkspaceFactory)),
                json!({"backend":"test"}),
            )
            .await
            .unwrap();
        let registry = runtime
            .root()
            .lookup_local::<WorkspaceRegistryContract>()
            .unwrap();
        let record = if delete {
            Some(registry.get_or_create(directory.path()).await.unwrap())
        } else {
            None
        };
        domain
            .pause
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let worker = {
            let registry = registry.clone();
            let path = directory.path().to_owned();
            tokio::spawn(async move {
                if let Some(record) = record {
                    registry.delete_registration(&record.id).await.map(|_| ())
                } else {
                    registry.get_or_create(&path).await.map(|_| ())
                }
            })
        };
        domain.entered.notified().await;
        worker.abort();
        let _ = worker.await;
        let closing = tokio::spawn(async move { fiber.dispose().await });
        // Retirement must reach the fence while the durable call is held open.
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert!(
            !closing.is_finished(),
            "withdrawal raced the accepted commit"
        );
        assert!(matches!(
            registry.list(None, 1).await,
            Err(rsi_workspace_protocol::WorkspaceError::Api(
                rsi_api_protocol::ApiError::ShuttingDown
            ))
        ));
        domain.resume.notify_one();
        assert!(closing.await.unwrap().is_clean());
        assert!(
            runtime
                .root()
                .lookup_local::<WorkspaceRegistryContract>()
                .is_none()
        );
        assert_eq!(domain.values.lock().await.len(), if delete { 1 } else { 2 });
        assert!(runtime.shutdown().await.is_clean());
    }
}
