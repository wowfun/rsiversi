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

#[tokio::test]
async fn cursor_survives_deletion_of_highest_orders_and_registry_restart() {
    let temporary = tempfile::tempdir().unwrap();
    for name in ["first", "second", "third", "after_restart", "after_empty"] {
        fs::create_dir(temporary.path().join(name)).unwrap();
    }
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("json", Arc::new(JsonStorageFactory)),
            json!({"name":"json","path":temporary.path().join("registry.json")}),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(linked("domains", Arc::new(DomainFactory)), Value::Null)
        .await
        .unwrap();
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
async fn registration_is_canonical_durable_and_delete_never_touches_files() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_path = temporary.path().join("work");
    let second_workspace_path = temporary.path().join("work-two");
    fs::create_dir(&workspace_path).unwrap();
    fs::create_dir(&second_workspace_path).unwrap();
    let user_file = workspace_path.join("keep.txt");
    fs::write(&user_file, b"keep").unwrap();
    let runtime = Runtime::default();
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
}
#[async_trait::async_trait]
impl rsi_storage_domain::Domain for FaultDomain {
    fn spec(&self) -> &rsi_storage_domain::DomainSpec {
        &self.spec
    }
    async fn snapshot(&self) -> std::collections::BTreeMap<String, Value> {
        self.values.lock().await.clone()
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
        self.values.lock().await.insert(key.into(), value);
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<bool, rsi_storage::StorageError> {
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
    let domain = Arc::new(FaultDomain {
        spec: rsi_storage_domain::DomainSpec {
            id: "rsi.workspace".into(),
            backend: "test".into(),
            version: 3,
            maximum_records: 16_385,
            maximum_bytes: 128 * 1024 * 1024,
        },
        values: tokio::sync::Mutex::new(std::collections::BTreeMap::default()),
        fail_registration: std::sync::atomic::AtomicBool::new(true),
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
    assert!(
        registry
            .get_or_create(&temporary.path().join("failed"))
            .await
            .is_err()
    );
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
    drop(registry);
    assert!(fiber.dispose().await.is_clean());
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
    assert_exhaustion_and_divergence(&runtime, &domain, temporary.path(), &kept).await;
    assert!(runtime.shutdown().await.is_clean());
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
async fn version_two_rejection_preserves_the_entire_backend_byte_for_byte() {
    use sha2::{Digest, Sha256};
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    let path = directory.to_str().unwrap();
    let id = hex::encode(Sha256::digest(path.as_bytes()));
    let file = temporary.path().join("registry.json");
    let original = serde_json::to_vec_pretty(&json!({"format":1,"domains":{
        "rsi.workspace":{"version":2,"records":{id.clone():{"order":9,"record":{"id":id,"path":path}}}},
        "another.domain":{"version":1,"records":{"keep":{"value":"unchanged"}}}
    }})).unwrap();
    fs::write(&file, &original).unwrap();
    let runtime = Runtime::default();
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
        panic!("v2 must fail activation")
    };
    assert!(error.contains("version 2, expected 3"), "{error}");
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
