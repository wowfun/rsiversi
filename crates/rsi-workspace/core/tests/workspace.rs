use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::StorageFactory;
use rsi_storage_domain::DomainFactory;
use rsi_storage_json::JsonStorageFactory;
use rsi_workspace::{WorkspaceFactory, WorkspaceRegistryContract, WorkspaceStatus};
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
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
    assert_eq!(registry.list().await, vec![first.clone(), third.clone()]);
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
    assert_eq!(registry.list().await, vec![first.clone(), third.clone()]);
    assert!(registry.delete_registration(&first.id).await.unwrap());
    assert!(user_file.is_file());
    assert!(workspace_path.is_dir());
    assert_eq!(registry.list().await, vec![third]);

    drop(registry);
    assert!(workspace.dispose().await.is_clean());
    assert!(domains.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}
