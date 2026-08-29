use rsi_meta::{FiberState, ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{StorageFactory, StorageHubContract};
use rsi_storage_domain::{DomainFacilityContract, DomainFactory, DomainSpec};
use rsi_storage_json::JsonStorageFactory;
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn committed_record_survives_backend_reactivation() {
    let temporary = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = temporary.path().join("domains.json");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let backend_config = json!({"name":"json", "path": path});
    let backend = runtime
        .root()
        .apply(
            linked("rsi.storage.json", Arc::new(JsonStorageFactory)),
            backend_config.clone(),
        )
        .await
        .unwrap();
    let domains = runtime
        .root()
        .apply(
            linked("rsi.storage.domain", Arc::new(DomainFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let facility = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let spec = DomainSpec {
        id: "workspace".into(),
        backend: "json".into(),
        version: 1,
        maximum_records: 10,
        maximum_bytes: 1024 * 1024,
    };
    let domain = facility.open(spec.clone()).await.unwrap();
    domain
        .put("record", json!({"path":"/tmp/a"}))
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(temporary.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    drop(domain);
    assert!(domains.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());

    let backend = runtime
        .root()
        .apply(
            linked("rsi.storage.json", Arc::new(JsonStorageFactory)),
            backend_config,
        )
        .await
        .unwrap();
    let domains = runtime
        .root()
        .apply(
            linked("rsi.storage.domain", Arc::new(DomainFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let reopened = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap()
        .open(spec)
        .await
        .unwrap();
    assert_eq!(
        reopened.snapshot().await["record"],
        json!({"path":"/tmp/a"})
    );

    drop(reopened);
    assert!(domains.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
    assert!(
        runtime
            .root()
            .lookup_local::<StorageHubContract>()
            .is_none()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn preplaced_temporary_symlinks_cannot_overwrite_their_target() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("domains.json");
    let victim = temporary.path().join("victim");
    fs::write(&victim, b"keep me").unwrap();
    for sequence in 0..512 {
        symlink(
            &victim,
            temporary.path().join(format!(
                ".domains.json.{}.{sequence}.tmp",
                std::process::id()
            )),
        )
        .unwrap();
    }

    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.storage.json", Arc::new(JsonStorageFactory)),
            json!({"name":"json", "path":path}),
        )
        .await
        .unwrap();
    let domains = runtime
        .root()
        .apply(
            linked("rsi.storage.domain", Arc::new(DomainFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let domain = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap()
        .open(DomainSpec {
            id: "workspace".into(),
            backend: "json".into(),
            version: 1,
            maximum_records: 10,
            maximum_bytes: 1024 * 1024,
        })
        .await
        .unwrap();

    let _result = domain.put("record", json!({"value":1})).await;
    assert_eq!(fs::read(&victim).unwrap(), b"keep me");

    drop(domain);
    assert!(domains.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn configured_document_symlink_is_rejected_before_reading_its_target() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let victim = temporary.path().join("outside.json");
    fs::write(&victim, br#"{"format":1,"domains":{}}"#).unwrap();
    let path = temporary.path().join("domains.json");
    symlink(&victim, &path).unwrap();
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();

    let backend = runtime
        .root()
        .apply(
            linked("rsi.storage.json", Arc::new(JsonStorageFactory)),
            json!({"name":"json", "path":path}),
        )
        .await
        .unwrap();
    assert!(matches!(backend.snapshot().state, FiberState::Failed(_)));
    assert!(backend.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[tokio::test]
async fn direct_backend_rejects_zero_schema_version_before_persistence() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("domains.json");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("rsi.storage.json", Arc::new(JsonStorageFactory)),
            json!({"name":"json", "path":path}),
        )
        .await
        .unwrap();
    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("json").unwrap();

    assert!(backend.put("domain", 0, "key", &json!(1)).await.is_err());
    assert!(
        !path.exists(),
        "invalid input must not create durable state"
    );

    drop(backend);
    drop(registry);
    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}
