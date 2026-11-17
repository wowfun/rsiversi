#![cfg(unix)]

use rsi_meta::FiberState;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{StorageFactory, StorageHubContract};
use rsi_storage_domain::{DomainFacilityContract, DomainFactory, DomainSpec, Schema};
use rsi_storage_json::JsonStorageFactory;
use serde_json::{Value, json};
#[cfg(unix)]
use std::fs;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One backend restart scenario follows committed and refused records across generations."
)]
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
        ..DomainSpec::default()
    };
    let domain = facility.open(spec.clone(), schema()).await.unwrap();
    domain
        .table::<Value>("rows")
        .unwrap()
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
    domain.close().await;
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
        .open(spec, schema())
        .await
        .unwrap();
    assert_eq!(
        *reopened
            .table::<Value>("rows")
            .unwrap()
            .get("record")
            .unwrap()
            .unwrap(),
        json!({"path":"/tmp/a"})
    );

    reopened.close().await;
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
    fs::create_dir(&path).unwrap();
    for sequence in 0..512 {
        symlink(
            &victim,
            path.join(format!(
                ".{}.json.{}.{sequence}.tmp",
                hash(b"workspace"),
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
        .open(
            DomainSpec {
                id: "workspace".into(),
                backend: "json".into(),
                version: 1,
                maximum_records: 10,
                maximum_bytes: 1024 * 1024,
                ..DomainSpec::default()
            },
            schema(),
        )
        .await
        .unwrap();

    let _result = domain
        .table::<Value>("rows")
        .unwrap()
        .put("record", json!({"value":1}))
        .await;
    assert_eq!(fs::read(&victim).unwrap(), b"keep me");

    domain.close().await;
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

    assert!(
        rsi_storage::UnitDescriptor::new(
            "domain".into(),
            0,
            std::collections::BTreeSet::new(),
            [("rows".into(), rsi_storage::RecordPolicy::Reject)].into(),
            false,
            rsi_storage::Layout::Single
        )
        .is_err()
    );
    assert!(
        !path.exists(),
        "invalid input must not create durable state"
    );

    drop(backend);
    drop(registry);
    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

fn schema() -> Schema {
    Schema::default().table::<Value>("rows").unwrap()
}
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "The corrupt-record scenario compares preserve-and-skip with single-layout refusal."
)]
async fn typed_disposable_record_is_preserved_and_skip_is_not_available_for_single_layout() {
    use rsi_storage::{
        Address, EncodedValue, Layout, Mutation, RecordPolicy, StorageError, UnitDescriptor,
    };
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("storage");
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("json", Arc::new(JsonStorageFactory)),
            json!({"name":"json","path":path}),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(linked("domain", Arc::new(DomainFactory)), Value::Null)
        .await
        .unwrap();
    let backend = runtime
        .root()
        .lookup_local::<StorageHubContract>()
        .unwrap()
        .resolve("json")
        .unwrap();
    let descriptor = UnitDescriptor::new(
        "unit".into(),
        1,
        std::collections::BTreeSet::new(),
        [("rows".into(), RecordPolicy::BackupAndSkip)].into(),
        true,
        Layout::PerRecord,
    )
    .unwrap();
    let opened = backend.open(descriptor).await.unwrap();
    opened
        .unit
        .write(vec![Mutation {
            address: Address::record("rows", "bad/中文\0").unwrap(),
            value: Some(EncodedValue::encode(&"wrong type").unwrap()),
        }])
        .await
        .unwrap();
    opened
        .unit
        .write(vec![Mutation {
            address: Address::global(),
            value: Some(EncodedValue::encode(&Value::Null).unwrap()),
        }])
        .await
        .unwrap();
    drop(opened);
    let original = path
        .join(hash(b"unit"))
        .join(hash(b"rows"))
        .join(format!("{}.json", hash("bad/中文\0".as_bytes())));
    let bytes = fs::read(&original).unwrap();
    let facility = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let spec = DomainSpec {
        id: "unit".into(),
        backend: "json".into(),
        layout: Layout::PerRecord,
        ..DomainSpec::default()
    };
    let schema = Schema::default()
        .table_with_policy::<u32>("rows", RecordPolicy::BackupAndSkip)
        .unwrap()
        .global::<Value>()
        .unwrap();
    let domain = facility.open(spec.clone(), schema.clone()).await.unwrap();
    assert!(
        domain
            .table::<u32>("rows")
            .unwrap()
            .get("bad/中文\0")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        *domain.global::<Value>().unwrap().get().unwrap().unwrap(),
        Value::Null
    );
    let reports = domain.recoveries();
    assert_eq!(reports.len(), 1);
    assert_eq!(fs::read(&reports[0].backup).unwrap(), bytes);
    assert!(!original.exists());
    assert_eq!(reports[0].key.as_deref(), Some("bad/中文\0"));
    let table = domain.table::<u32>("rows").unwrap();
    assert!(matches!(
        domain
            .batch(vec![
                table.put_change("a", 1).unwrap(),
                table.put_change("b", 2).unwrap()
            ])
            .await,
        Err(StorageError::Unsupported(_))
    ));
    assert!(table.get("a").unwrap().is_none());
    table.put("", 3).await.unwrap();
    table.put("../中文\0", 4).await.unwrap();
    table
        .delete_many(["", "../中文\0", "missing"])
        .await
        .unwrap();
    domain.close().await;
    let reopened = facility.open(spec.clone(), schema.clone()).await.unwrap();
    assert_eq!(
        reopened
            .table::<u32>("rows")
            .unwrap()
            .snapshot()
            .unwrap()
            .len(),
        0
    );
    reopened.close().await;
    assert!(matches!(
        facility
            .open(
                DomainSpec {
                    id: "single".into(),
                    layout: Layout::Single,
                    ..spec
                },
                schema
            )
            .await,
        Err(StorageError::Unsupported(_))
    ));
}
