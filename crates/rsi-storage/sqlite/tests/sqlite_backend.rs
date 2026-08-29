use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    MAXIMUM_STORAGE_RECORDS, MAXIMUM_STORAGE_VALUE_BYTES, StorageError, StorageFactory,
    StorageHubContract,
};
use rsi_storage_domain::{DomainFacilityContract, DomainFactory, DomainSpec};
use rsi_storage_sqlite::SqliteStorageFactory;
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn sqlite_round_trip_and_version_mismatch_are_visible_at_domain_seam() {
    let temporary = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = temporary.path().join("domains.sqlite3");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.storage.sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite", "path":path}),
        )
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
    let form = runtime
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
    let domain = facility
        .open(DomainSpec {
            id: "projection".into(),
            backend: "sqlite".into(),
            version: 1,
            maximum_records: 10,
            maximum_bytes: 1024 * 1024,
        })
        .await
        .unwrap();
    domain.put("a", json!([1, 2, 3])).await.unwrap();
    assert_eq!(domain.snapshot().await["a"], json!([1, 2, 3]));

    let locking = rusqlite::Connection::open(&path).unwrap();
    locking.execute_batch("BEGIN IMMEDIATE").unwrap();
    let blocked_put = tokio::spawn({
        let domain = Arc::clone(&domain);
        async move { domain.put("busy", json!(true)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !blocked_put.is_finished(),
        "a transient SQLite writer lock must wait instead of failing immediately"
    );
    locking.execute_batch("COMMIT").unwrap();
    blocked_put.await.unwrap().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for sidecar in ["domains.sqlite3-wal", "domains.sqlite3-shm"] {
            assert_eq!(
                fs::metadata(temporary.path().join(sidecar))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{sidecar} must be private"
            );
        }
    }
    drop(domain);

    assert!(
        facility
            .open(DomainSpec {
                id: "projection".into(),
                backend: "sqlite".into(),
                version: 2,
                maximum_records: 10,
                maximum_bytes: 1024 * 1024,
            })
            .await
            .is_err()
    );

    drop(facility);
    assert!(form.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[tokio::test]
async fn oversized_durable_blob_is_rejected_at_the_backend_load_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("domains.sqlite3");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("rsi.storage.sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite", "path":path}),
        )
        .await
        .unwrap();
    let injection = rusqlite::Connection::open(&path).unwrap();
    injection
        .execute(
            "INSERT INTO rsi_storage_domains(domain, version) VALUES ('oversized', 1)",
            [],
        )
        .unwrap();
    injection
        .execute(
            "INSERT INTO rsi_storage_records(domain, key, value)
             VALUES ('oversized', 'record', zeroblob(?1))",
            [i64::try_from(MAXIMUM_STORAGE_VALUE_BYTES + 1).unwrap()],
        )
        .unwrap();

    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    assert!(matches!(
        backend.load("oversized").await,
        Err(rsi_storage::StorageError::Corrupt(message)) if message.contains("oversized")
    ));

    drop(backend);
    drop(registry);
    drop(injection);
    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[tokio::test]
async fn raw_backend_put_enforces_the_global_record_bound_transactionally() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("domains.sqlite3");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("rsi.storage.sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite", "path":path}),
        )
        .await
        .unwrap();
    let injection = rusqlite::Connection::open(&path).unwrap();
    injection
        .execute(
            "INSERT INTO rsi_storage_domains(domain, version) VALUES ('ceiling', 1)",
            [],
        )
        .unwrap();
    injection
        .execute(
            "WITH RECURSIVE records(index_value) AS (
               SELECT 0
               UNION ALL
               SELECT index_value + 1 FROM records WHERE index_value + 1 < ?1
             )
             INSERT INTO rsi_storage_records(domain, key, value)
             SELECT 'ceiling', printf('key-%05d', index_value), x'6e756c6c' FROM records",
            [i64::try_from(MAXIMUM_STORAGE_RECORDS).unwrap()],
        )
        .unwrap();

    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    assert!(matches!(
        backend.put("ceiling", 1, "one-too-many", &json!(true)).await,
        Err(StorageError::InvalidInput(message)) if message.contains("record bound")
    ));
    backend
        .put("ceiling", 1, "key-00000", &json!("updated"))
        .await
        .expect("updates remain valid at the record ceiling");
    let count: i64 = injection
        .query_row(
            "SELECT count(*) FROM rsi_storage_records WHERE domain = 'ceiling'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usize::try_from(count).unwrap(), MAXIMUM_STORAGE_RECORDS);

    drop(backend);
    drop(registry);
    drop(injection);
    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[tokio::test]
async fn an_existing_lookalike_schema_is_rejected_before_backend_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("domains.sqlite3");
    let incompatible = rusqlite::Connection::open(&path).unwrap();
    incompatible
        .execute_batch(
            "CREATE TABLE rsi_storage_domains (
               domain TEXT PRIMARY KEY NOT NULL,
               version INTEGER NOT NULL CHECK(version > 0)
             ) STRICT;
             CREATE TABLE rsi_storage_records (
               domain TEXT NOT NULL,
               key TEXT NOT NULL,
               value BLOB NOT NULL,
               PRIMARY KEY(domain, key),
               FOREIGN KEY(domain) REFERENCES rsi_storage_domains(domain) ON DELETE CASCADE
             ) STRICT;",
        )
        .unwrap();
    drop(incompatible);

    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("rsi.storage.sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite", "path":path}),
        )
        .await
        .unwrap();

    assert!(
        matches!(plugin.snapshot().state, rsi_meta::FiberState::Failed(message) if message.contains("incompatible schema")),
        "lookalike tables without the current count invariant must not be published"
    );
    let unchanged = rusqlite::Connection::open(&path).unwrap();
    let trigger_count: i64 = unchanged
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'trigger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        trigger_count, 0,
        "opening an incompatible schema must not repair or mutate it"
    );
    drop(unchanged);
    assert!(
        runtime
            .root()
            .lookup_local::<StorageHubContract>()
            .unwrap()
            .resolve("sqlite")
            .is_err()
    );

    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn preplaced_database_symlink_is_rejected_without_chmodding_its_target() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let temporary = tempfile::tempdir().unwrap();
    let victim = temporary.path().join("victim");
    fs::write(&victim, b"not a database").unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
    let path = temporary.path().join("domains.sqlite3");
    symlink(&victim, &path).unwrap();

    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("rsi.storage", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("rsi.storage.sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite", "path":path}),
        )
        .await
        .unwrap();

    assert!(
        matches!(plugin.snapshot().state, rsi_meta::FiberState::Failed(_)),
        "a database symlink must leave a failed activation fiber"
    );
    assert_eq!(
        fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
        0o644,
        "rejecting a database symlink must not chmod its target"
    );
    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}
