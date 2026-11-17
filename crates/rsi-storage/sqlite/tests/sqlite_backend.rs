#![cfg(any(unix, windows))]

use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    Address, EncodedValue, KvBackend, Layout, MAXIMUM_STORAGE_DOMAIN_BYTES,
    MAXIMUM_STORAGE_RECORDS, MAXIMUM_STORAGE_VALUE_BYTES, Mutation, RecordObjectSize, RecordPolicy,
    StorageError, StorageFactory, StorageHubContract, UnitDescriptor,
};
use rsi_storage_domain::{DomainFacilityContract, DomainFactory, DomainSpec, Schema};
use rsi_storage_sqlite::SqliteStorageFactory;
use serde_json::{Value, json};
#[cfg(unix)]
use std::fs;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}
struct Harness {
    runtime: Runtime,
    backend: Arc<dyn KvBackend>,
    path: PathBuf,
    _root: tempfile::TempDir,
}
impl Harness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/domains.sqlite3");
        let runtime = Runtime::default();
        runtime
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
        assert_eq!(
            plugin.snapshot().state,
            rsi_meta::FiberState::Active,
            "{:?}",
            plugin.snapshot()
        );
        let backend = runtime
            .root()
            .lookup_local::<StorageHubContract>()
            .unwrap()
            .resolve("sqlite")
            .unwrap();
        Self {
            runtime,
            backend,
            path,
            _root: root,
        }
    }
}
fn spec(version: u32) -> UnitDescriptor {
    UnitDescriptor::new(
        "records".into(),
        version,
        std::collections::BTreeSet::new(),
        BTreeMap::from([("rows".into(), RecordPolicy::Reject)]),
        true,
        Layout::Single,
    )
    .unwrap()
}
fn change(key: &str, value: Option<Value>) -> Mutation {
    Mutation {
        address: Address::record("rows", key).unwrap(),
        value: value.map(|v| EncodedValue::encode(&v).unwrap()),
    }
}
fn decoded(snapshot: rsi_storage::UnitSnapshot) -> BTreeMap<String, Value> {
    snapshot
        .records
        .into_iter()
        .map(|(a, v)| (a.accounting_key(), v.decode().unwrap()))
        .collect()
}
fn assert_accounting(h: &Harness, expected: &BTreeMap<String, Value>) {
    let db = rusqlite::Connection::open(&h.path).unwrap();
    let (count, bytes): (i64, i64) = db
        .query_row(
            "SELECT record_count,record_bytes FROM rsi_storage_domains WHERE domain='records'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(usize::try_from(count).unwrap(), expected.len());
    assert_eq!(
        usize::try_from(bytes).unwrap(),
        serde_json::to_vec(expected).unwrap().len()
    );
}

#[tokio::test]
async fn mutation_accounting_distinguishes_absence_global_null_arbitrary_keys_and_versions() {
    let h = Harness::new().await;
    let opened = h.backend.open(spec(7)).await.unwrap();
    opened
        .unit
        .write(vec![change("absent", None)])
        .await
        .unwrap();
    drop(opened);
    let opened = h.backend.open(spec(7)).await.unwrap();
    assert_eq!(opened.snapshot.version, None);
    opened
        .unit
        .write(vec![
            change("中文/\0", Some(json!("中"))),
            change("", Some(json!([true, null]))),
            Mutation {
                address: Address::global(),
                value: Some(EncodedValue::encode(&Value::Null).unwrap()),
            },
        ])
        .await
        .unwrap();
    let expected = BTreeMap::from([
        ("rows/中文/\0".into(), json!("中")),
        ("rows/".into(), json!([true, null])),
        (String::new(), Value::Null),
    ]);
    assert_accounting(&h, &expected);
    // One backend owns exactly one live handle for a unit.
    assert!(h.backend.open(spec(7)).await.is_err());
    drop(opened);
    assert!(matches!(
        h.backend.open(spec(8)).await,
        Err(StorageError::DomainVersionMismatch {
            stored: 7,
            requested: 8,
            ..
        })
    ));
    let opened = h.backend.open(spec(7)).await.unwrap();
    assert_eq!(decoded(opened.snapshot), expected);
    opened
        .unit
        .write(vec![change("中文/\0", None), change("missing", None)])
        .await
        .unwrap();
    drop(opened.unit);
    let opened = h.backend.open(spec(7)).await.unwrap();
    assert_eq!(opened.snapshot.version, Some(7));
    assert_accounting(&h, &decoded(opened.snapshot));
}

#[tokio::test]
async fn compatible_read_preserves_stamp_and_batch_updates_only_touched_bytes() {
    let h = Harness::new().await;
    let opened = h.backend.open(spec(1)).await.unwrap();
    let exact = br#"{"z":1.2300,"a":"\u4e2d"}"#.to_vec();
    opened
        .unit
        .write(vec![Mutation {
            address: Address::record("rows", "old").unwrap(),
            value: Some(EncodedValue::from_bytes(exact.clone()).unwrap()),
        }])
        .await
        .unwrap();
    drop(opened);
    let next = UnitDescriptor::new(
        "records".into(),
        2,
        [1].into(),
        spec(1).tables().clone(),
        true,
        Layout::Single,
    )
    .unwrap();
    let opened = h.backend.open(next.clone()).await.unwrap();
    assert_eq!(opened.snapshot.version, Some(1));
    opened
        .unit
        .write(vec![change("missing", None)])
        .await
        .unwrap();
    drop(opened);
    let opened = h.backend.open(next.clone()).await.unwrap();
    assert_eq!(opened.snapshot.version, Some(1));
    opened
        .unit
        .write(vec![
            change("new", Some(json!(3))),
            Mutation {
                address: Address::global(),
                value: Some(EncodedValue::encode(&2).unwrap()),
            },
        ])
        .await
        .unwrap();
    drop(opened);
    let opened = h.backend.open(next).await.unwrap();
    assert_eq!(opened.snapshot.version, Some(2));
    assert_eq!(
        opened.snapshot.records[&Address::record("rows", "old").unwrap()].as_bytes(),
        exact
    );
}

#[tokio::test]
async fn raw_backend_batch_uses_final_geometry_and_rejects_before_mutation() {
    let h = Harness::new().await;
    let opened = h.backend.open(spec(1)).await.unwrap();
    opened
        .unit
        .write(
            (0..MAXIMUM_STORAGE_RECORDS)
                .map(|i| change(&format!("key-{i:05}"), Some(Value::Null)))
                .collect(),
        )
        .await
        .unwrap();
    assert!(matches!(
        opened
            .unit
            .write(vec![change("excess", Some(json!(true)))])
            .await,
        Err(StorageError::InvalidInput(_))
    ));
    // This begins with an insert at the ceiling, then removes an old record.
    opened
        .unit
        .write(vec![
            change("replacement", Some(json!(true))),
            change("key-00000", None),
        ])
        .await
        .unwrap();
    opened
        .unit
        .write(vec![change("key-00001", Some(json!("updated")))])
        .await
        .unwrap();
    assert!(matches!(
        opened
            .unit
            .write(vec![
                change("key-00001", Some(json!("should roll back"))),
                change("excess", Some(Value::Null))
            ])
            .await,
        Err(StorageError::InvalidInput(_))
    ));
    drop(opened);
    let opened = h.backend.open(spec(1)).await.unwrap();
    assert_eq!(opened.snapshot.records.len(), MAXIMUM_STORAGE_RECORDS);
    assert!(
        !opened
            .snapshot
            .records
            .contains_key(&Address::record("rows", "key-00000").unwrap())
    );
    assert_eq!(
        opened.snapshot.records[&Address::record("rows", "key-00001").unwrap()]
            .decode::<Value>()
            .unwrap(),
        json!("updated")
    );
    assert_accounting(&h, &decoded(opened.snapshot));
}

#[tokio::test]
async fn every_acknowledged_byte_boundary_reopens_and_raw_growth_is_rejected_before_mutation() {
    let h = Harness::new().await;
    let opened = h.backend.open(spec(u32::MAX)).await.unwrap();
    let mut measured = RecordObjectSize::default();
    let mut final_bytes = 0;
    for index in 0..16 {
        let key = format!("k{index:02}");
        let address = Address::record("rows", &key).unwrap();
        let overhead = address.entry_bytes(0).unwrap() + usize::from(index != 0);
        let bytes = if index < 15 {
            MAXIMUM_STORAGE_VALUE_BYTES
        } else {
            MAXIMUM_STORAGE_DOMAIN_BYTES - 1 - measured.bytes() - overhead
        };
        opened
            .unit
            .write(vec![change(&key, Some(json!("x".repeat(bytes - 2))))])
            .await
            .unwrap();
        measured = measured
            .with_entry(None, address.entry_bytes(bytes).unwrap())
            .unwrap();
        final_bytes = bytes;
    }
    assert_eq!(measured.bytes(), MAXIMUM_STORAGE_DOMAIN_BYTES - 1);
    drop(opened);
    let opened = h.backend.open(spec(u32::MAX)).await.unwrap();
    drop(opened.snapshot);
    opened
        .unit
        .write(vec![change(
            "k15",
            Some(json!("x".repeat(final_bytes - 1))),
        )])
        .await
        .unwrap();
    assert!(matches!(
        opened
            .unit
            .write(vec![change("k15", Some(json!("x".repeat(final_bytes))))])
            .await,
        Err(StorageError::InvalidInput(_))
    ));
    assert!(matches!(
        opened
            .unit
            .write(vec![change("extra", Some(Value::Null))])
            .await,
        Err(StorageError::InvalidInput(_))
    ));
    drop(opened.unit);
    let opened = h.backend.open(spec(u32::MAX)).await.unwrap();
    let size = opened
        .snapshot
        .records
        .iter()
        .fold(RecordObjectSize::default(), |size, (a, v)| {
            size.with_entry(None, a.entry_bytes(v.encoded_len()).unwrap())
                .unwrap()
        });
    assert_eq!(size.bytes(), MAXIMUM_STORAGE_DOMAIN_BYTES);
    drop(opened.snapshot);
    opened
        .unit
        .write(vec![change("k00", None), change("k15", Some(Value::Null))])
        .await
        .unwrap();
    drop(opened.unit);
    let opened = h.backend.open(spec(u32::MAX)).await.unwrap();
    assert_eq!(opened.snapshot.records.len(), 15);
    assert_eq!(
        opened.snapshot.records[&Address::record("rows", "k15").unwrap()].as_bytes(),
        b"null"
    );
}

#[tokio::test]
async fn noncompact_or_oversized_durable_blobs_are_rejected_on_open() {
    for bytes in [
        b"{\"x\": 1}".to_vec(),
        vec![b' '; MAXIMUM_STORAGE_VALUE_BYTES + 1],
    ] {
        let h = Harness::new().await;
        let opened = h.backend.open(spec(1)).await.unwrap();
        opened
            .unit
            .write(vec![change("bad", Some(Value::Null))])
            .await
            .unwrap();
        drop(opened);
        let db = rusqlite::Connection::open(&h.path).unwrap();
        db.execute("UPDATE rsi_storage_records SET value=?1", [&bytes])
            .unwrap();
        let size = RecordObjectSize::default()
            .with_entry(
                None,
                Address::record("rows", "bad")
                    .unwrap()
                    .entry_bytes(bytes.len())
                    .unwrap(),
            )
            .unwrap();
        db.execute(
            "UPDATE rsi_storage_domains SET record_bytes=?1",
            [i64::try_from(size.bytes()).unwrap()],
        )
        .unwrap();
        assert!(matches!(
            h.backend.open(spec(1)).await,
            Err(StorageError::Corrupt(_))
        ));
        let kept: Vec<u8> = db
            .query_row("SELECT value FROM rsi_storage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, bytes);
    }
}

#[tokio::test]
async fn sqlite_round_trip_and_version_mismatch_are_visible_at_domain_seam() {
    let h = Harness::new().await;
    h.runtime
        .root()
        .apply(linked("domain", Arc::new(DomainFactory)), Value::Null)
        .await
        .unwrap();
    let facility = h
        .runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let spec = DomainSpec {
        id: "records".into(),
        backend: "sqlite".into(),
        maximum_records: 10,
        maximum_bytes: 1024,
        ..DomainSpec::default()
    };
    let schema = Schema::default().table::<Vec<u32>>("rows").unwrap();
    let domain = facility.open(spec.clone(), schema.clone()).await.unwrap();
    domain
        .table::<Vec<u32>>("rows")
        .unwrap()
        .put("a", vec![1, 2, 3])
        .await
        .unwrap();
    domain.close().await;
    let domain = facility.open(spec.clone(), schema.clone()).await.unwrap();
    assert_eq!(
        *domain
            .table::<Vec<u32>>("rows")
            .unwrap()
            .get("a")
            .unwrap()
            .unwrap(),
        vec![1, 2, 3]
    );
    domain.close().await;
    assert!(matches!(
        facility
            .open(DomainSpec { version: 2, ..spec }, schema)
            .await,
        Err(StorageError::DomainVersionMismatch { .. })
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for name in [
            "domains.sqlite3",
            "domains.sqlite3-wal",
            "domains.sqlite3-shm",
        ] {
            assert_eq!(
                fs::metadata(h.path.parent().unwrap().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[tokio::test]
#[cfg(unix)]
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
