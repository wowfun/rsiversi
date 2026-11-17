#![cfg(any(unix, windows))]

use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    Address, EncodedValue, Layout, Mutation, RecordPolicy, StorageFactory, StorageHubContract,
    UnitDescriptor,
};
use rsi_storage_sqlite::{SQLITE_STORAGE_FORMAT, SqliteStorageFactory};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    format: u32,
    database_sha256: Vec<u8>,
    schema: Vec<(String, String, String)>,
    values: BTreeMap<String, Value>,
}

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "persistence", UpdateMode::Replayable, factory)
}

fn schema(path: &Path) -> Vec<(String, String, String)> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, String>(2)?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
#[ignore = "explicit new RSI_STORAGE_CAPTURE directory only"]
async fn capture_current_writer() {
    let root =
        PathBuf::from(std::env::var_os("RSI_STORAGE_CAPTURE").expect("new capture directory"));
    std::fs::create_dir(&root).expect("capture never replaces retained files");
    let private = tempfile::tempdir().unwrap();
    let path = private.path().join("private/domains.sqlite3");
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
    assert_eq!(
        plugin.snapshot().state,
        rsi_meta::FiberState::Active,
        "{:?}",
        plugin.snapshot()
    );
    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("sqlite").unwrap();
    let values: BTreeMap<String, Value> = BTreeMap::from([
        ("scalar".into(), json!(true)),
        (
            "dynamic".into(),
            json!({"z": 1.25, "a": [null, false, 18_446_744_073_709_551_615_u64], "text": "中 🦀\n\"\\"}),
        ),
        ("null".into(), Value::Null),
    ]);
    let opened = backend.open(descriptor()).await.unwrap();
    for (key, value) in &values {
        opened
            .unit
            .write(vec![mutation(key, Some(value))])
            .await
            .unwrap();
    }
    opened
        .unit
        .write(vec![mutation("removed", Some(&json!(42)))])
        .await
        .unwrap();
    opened
        .unit
        .write(vec![mutation("removed", None)])
        .await
        .unwrap();
    drop(opened);
    drop(backend);
    assert!(plugin.dispose().await.is_clean());
    drop(registry);
    assert!(hub.dispose().await.is_clean());
    let bytes = std::fs::read(&path).unwrap();
    let baseline = Baseline {
        format: SQLITE_STORAGE_FORMAT,
        database_sha256: Sha256::digest(&bytes).to_vec(),
        schema: schema(&path),
        values,
    };
    let mut output = flate2::GzBuilder::new().mtime(0).write(
        std::fs::File::create(root.join("database.gz")).unwrap(),
        flate2::Compression::best(),
    );
    output.write_all(&bytes).unwrap();
    output.finish().unwrap();
    std::fs::write(
        root.join("baseline.json"),
        serde_json::to_vec_pretty(&baseline).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn retained_writer_corpus() {
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/persistence");
    let entries = std::fs::read_dir(home)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!entries.is_empty());
    let mut current = false;
    for entry in entries {
        let root = entry.path();
        let baseline: Baseline =
            serde_json::from_slice(&std::fs::read(root.join("baseline.json")).unwrap()).unwrap();
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(root.join("database.gz")).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(Sha256::digest(&bytes).as_slice(), baseline.database_sha256);
        let private = tempfile::tempdir().unwrap();
        rsi_storage::create_private_directories(private.path()).unwrap();
        let path = private.path().join("domains.sqlite3");
        write_private_fixture(&path, &bytes);
        let runtime = Runtime::default();
        let hub = runtime
            .root()
            .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
            .await
            .unwrap();
        let result = runtime
            .root()
            .apply(
                linked("sqlite", Arc::new(SqliteStorageFactory)),
                json!({"name":"sqlite","path":path}),
            )
            .await;
        if baseline.format == SQLITE_STORAGE_FORMAT {
            current = true;
            let plugin = result.unwrap();
            assert_eq!(
                plugin.snapshot().state,
                rsi_meta::FiberState::Active,
                "{:?}",
                plugin.snapshot()
            );
            let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
            let backend = registry.resolve("sqlite").unwrap();
            assert_eq!(
                backend
                    .open(descriptor())
                    .await
                    .unwrap()
                    .snapshot
                    .records
                    .into_iter()
                    .map(|(address, value)| (
                        address.key().to_owned(),
                        value.decode::<Value>().unwrap()
                    ))
                    .collect::<BTreeMap<_, _>>(),
                baseline.values
            );
            assert_eq!(schema(&path), baseline.schema);
            let db = rusqlite::Connection::open_with_flags(
                &path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            for (key, value) in &baseline.values {
                let encoded: Vec<u8> = db
                    .query_row(
                        "SELECT value FROM rsi_storage_records WHERE domain='corpus' AND key=?1",
                        [key],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(encoded, serde_json::to_vec(value).unwrap());
            }
            drop(db);
            drop(backend);
            drop(registry);
            assert!(plugin.dispose().await.is_clean());
        } else {
            let plugin = result.expect("valid configuration reaches backend activation");
            assert!(
                matches!(plugin.snapshot().state,
                rsi_meta::FiberState::Failed(message) if message.contains("incompatible schema")),
                "old layout must fail at schema admission, not an unrelated boundary"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "refusal changed durable payload"
            );
            assert!(plugin.dispose().await.is_clean());
        }
        assert!(hub.dispose().await.is_clean());
    }
    assert!(
        current,
        "current SQLite format requires a real-writer corpus"
    );
}

fn descriptor() -> UnitDescriptor {
    UnitDescriptor::new(
        "corpus".into(),
        1,
        std::collections::BTreeSet::new(),
        BTreeMap::from([("rows".into(), RecordPolicy::Reject)]),
        false,
        Layout::Single,
    )
    .unwrap()
}
fn mutation(key: &str, value: Option<&Value>) -> Mutation {
    Mutation {
        address: Address::record("rows", key).unwrap(),
        value: value.map(|value| EncodedValue::encode(value).unwrap()),
    }
}

fn write_private_fixture(path: &Path, bytes: &[u8]) {
    // The production boundary also requires native ownership on Windows.
    #[cfg(windows)]
    {
        let directory = rsi_files_native_fs::set_private_directory_permissions_no_follow(
            path.parent().unwrap(),
        )
        .unwrap();
        let mut file = rsi_files_native_fs::create_relative_private_file_no_follow(
            &directory,
            Path::new("domains.sqlite3"),
        )
        .unwrap();
        file.write_all(bytes).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}
