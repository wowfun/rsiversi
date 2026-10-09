use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    MAXIMUM_STORAGE_RECORDS, MAXIMUM_STORAGE_VALUE_BYTES, StorageError, StorageFactory,
    StorageHubContract,
};
use rsi_storage_domain::{DomainFacilityContract, DomainFactory, DomainSpec};
use rsi_storage_sqlite::SqliteStorageFactory;
use serde_json::{Value, json};
#[cfg(unix)]
use std::fs;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}
#[tokio::test]
async fn mutation_accounting_distinguishes_missing_domain_key_and_wrong_version() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("mutations.sqlite3");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite","path":path}),
        )
        .await
        .unwrap();
    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    backend.delete("records", 7, "absent").await.unwrap();
    assert!(backend.load("records").await.unwrap().is_none());
    backend
        .put("records", 7, "first", &json!("中文"))
        .await
        .unwrap();
    backend
        .put("records", 7, "second", &json!({"n":1}))
        .await
        .unwrap();
    backend
        .put("records", 7, "first", &json!([true, null]))
        .await
        .unwrap();
    backend.delete("records", 7, "absent").await.unwrap();
    for key in ["absent", "first"] {
        assert!(matches!(
            backend.delete("records", 8, key).await,
            Err(StorageError::Corrupt(_))
        ));
        assert!(matches!(
            backend.put("records", 8, key, &Value::Null).await,
            Err(StorageError::Corrupt(_))
        ));
    }
    let inspect = rusqlite::Connection::open(&path).unwrap();
    let expected = std::collections::BTreeMap::from([
        ("first".to_owned(), json!([true, null])),
        ("second".to_owned(), json!({"n":1})),
    ]);
    let assert_accounting =
        |expected: &std::collections::BTreeMap<String, Value>| {
            let (count, bytes) = inspect.query_row(
            "SELECT record_count, record_bytes FROM rsi_storage_domains WHERE domain='records'",
            [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        ).unwrap();
            assert_eq!(usize::try_from(count).unwrap(), expected.len());
            assert_eq!(
                usize::try_from(bytes).unwrap(),
                serde_json::to_vec(expected).unwrap().len()
            );
        };
    assert_accounting(&expected);
    assert_eq!(
        backend.load("records").await.unwrap().unwrap().records,
        expected
    );
    backend.delete("records", 7, "first").await.unwrap();
    let expected = std::collections::BTreeMap::from([("second".to_owned(), json!({"n":1}))]);
    assert_accounting(&expected);
    backend.delete("records", 7, "second").await.unwrap();
    backend.delete("records", 7, "second").await.unwrap();
    assert_accounting(&std::collections::BTreeMap::new());
    drop(inspect);
    drop(backend);
    assert!(plugin.dispose().await.is_clean());
    let reopened = runtime
        .root()
        .apply(
            linked("sqlite-reopened", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite","path":path}),
        )
        .await
        .unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    let empty = backend.load("records").await.unwrap().unwrap();
    assert_eq!(empty.version, 7);
    assert!(empty.records.is_empty());
    drop(backend);
    drop(registry);
    assert!(reopened.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[tokio::test]
async fn typed_raw_put_reopens_while_noncompact_offline_blobs_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("domains.sqlite3");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite","path":path}),
        )
        .await
        .unwrap();
    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    let values = std::collections::BTreeMap::from([
        ("structured".to_owned(), serde_json::from_str::<Value>(
            r#" { "z": 1.2300, "a": [ -0, 18446744073709551616, 1e+09 ], "text": "\u4e2d\n" } "#,
        ).unwrap()),
        ("whitespace".to_owned(), json!(" \n\t中文 ")),
        ("null".to_owned(), Value::Null),
        ("scalar".to_owned(), json!(true)),
    ]);
    for (key, value) in &values {
        backend.put("typed", 1, key, value).await.unwrap();
    }
    assert_eq!(
        backend.load("typed").await.unwrap().unwrap().records,
        values
    );

    let injection = rusqlite::Connection::open(&path).unwrap();
    let pretty = serde_json::to_vec_pretty(&values["structured"]).unwrap();
    injection
        .execute(
            "INSERT INTO rsi_storage_domains(domain,version) VALUES ('offline',1)",
            [],
        )
        .unwrap();
    injection
        .execute(
            "INSERT INTO rsi_storage_records(domain,key,value) VALUES ('offline','record',?1)",
            [&pretty],
        )
        .unwrap();
    let size = rsi_storage::RecordObjectSize::default()
        .with_entry(
            None,
            rsi_storage::encoded_entry_bytes("record", pretty.len()).unwrap(),
        )
        .unwrap();
    injection
        .execute(
            "UPDATE rsi_storage_domains SET record_bytes=?1 WHERE domain='offline'",
            [i64::try_from(size.bytes()).unwrap()],
        )
        .unwrap();
    assert!(matches!(backend.load("offline").await,
        Err(StorageError::Corrupt(message)) if message.contains("not compact JSON")));
    drop(injection);
    drop(backend);
    assert!(plugin.dispose().await.is_clean());

    let reopened = runtime
        .root()
        .apply(
            linked("sqlite-reopened", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite","path":path}),
        )
        .await
        .unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    assert_eq!(
        backend.load("typed").await.unwrap().unwrap().records,
        values
    );
    assert!(matches!(backend.load("offline").await,
        Err(StorageError::Corrupt(message)) if message.contains("not compact JSON")));
    drop(backend);
    drop(registry);
    assert!(reopened.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
}

#[tokio::test]
async fn every_acknowledged_byte_boundary_reopens_and_raw_growth_is_rejected_before_mutation() {
    use rsi_storage::{MAXIMUM_STORAGE_DOMAIN_BYTES, RecordObjectSize, encoded_entry_bytes};
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("boundary.sqlite3");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let backend_fiber = runtime
        .root()
        .apply(
            linked("sqlite", Arc::new(SqliteStorageFactory)),
            json!({"name":"sqlite","path":path}),
        )
        .await
        .unwrap();
    let form = runtime
        .root()
        .apply(linked("domain", Arc::new(DomainFactory)), Value::Null)
        .await
        .unwrap();
    let facility = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let spec = DomainSpec {
        id: "boundary".into(),
        backend: "sqlite".into(),
        version: u32::MAX,
        maximum_records: 16,
        maximum_bytes: MAXIMUM_STORAGE_DOMAIN_BYTES,
    };
    let domain = facility.open(spec.clone()).await.unwrap();
    let mut measured = RecordObjectSize::default();
    let mut final_value_bytes = 0;
    for index in 0..16 {
        let key = format!("k{index:02}");
        let overhead = encoded_entry_bytes(&key, 0).unwrap() + usize::from(index != 0);
        let value_bytes = if index < 15 {
            MAXIMUM_STORAGE_VALUE_BYTES
        } else {
            MAXIMUM_STORAGE_DOMAIN_BYTES - 1 - measured.bytes() - overhead
        };
        domain
            .put(&key, Value::String("x".repeat(value_bytes - 2)))
            .await
            .unwrap();
        measured = measured
            .with_entry(None, encoded_entry_bytes(&key, value_bytes).unwrap())
            .unwrap();
        final_value_bytes = value_bytes;
    }
    assert_eq!(measured.bytes(), MAXIMUM_STORAGE_DOMAIN_BYTES - 1);
    drop(domain);
    let domain = facility.open(spec.clone()).await.unwrap();
    domain
        .put("k15", Value::String("x".repeat(final_value_bytes - 1)))
        .await
        .unwrap();
    drop(domain);
    let domain = facility.open(spec).await.unwrap();
    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    assert!(matches!(
        backend
            .put(
                "boundary",
                u32::MAX,
                "k15",
                &Value::String("x".repeat(final_value_bytes))
            )
            .await,
        Err(StorageError::InvalidInput(_))
    ));
    assert!(matches!(
        backend.put("boundary", u32::MAX, "new", &Value::Null).await,
        Err(StorageError::InvalidInput(_))
    ));
    let loaded = backend.load("boundary").await.unwrap().unwrap();
    assert_eq!(loaded.version, u32::MAX);
    assert_eq!(
        serde_json::to_vec(&loaded.records).unwrap().len(),
        MAXIMUM_STORAGE_DOMAIN_BYTES
    );
    drop(loaded);
    domain.delete("k00").await.unwrap();
    domain.put("k15", Value::Null).await.unwrap();
    drop(domain);
    let loaded = backend.load("boundary").await.unwrap().unwrap();
    assert_eq!(loaded.records.len(), 15);
    assert_eq!(loaded.records["k15"], Value::Null);
    drop(loaded);
    drop((backend, registry, facility));
    assert!(form.dispose().await.is_clean());
    assert!(backend_fiber.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
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
    assert_eq!(domain.snapshot().await.unwrap()["a"], json!([1, 2, 3]));

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
    let mut size = rsi_storage::RecordObjectSize::default();
    for index in 0..MAXIMUM_STORAGE_RECORDS {
        size = size
            .with_entry(
                None,
                rsi_storage::encoded_entry_bytes(&format!("key-{index:05}"), 4).unwrap(),
            )
            .unwrap();
    }
    injection
        .execute(
            "UPDATE rsi_storage_domains SET record_bytes=?1 WHERE domain='ceiling'",
            [i64::try_from(size.bytes()).unwrap()],
        )
        .unwrap();
    assert!(matches!(
        backend.put("ceiling", 1, "one-too-many", &json!(true)).await,
        Err(StorageError::InvalidInput(message))
            if message.contains("`ceiling`") && message.contains("record bound")
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
