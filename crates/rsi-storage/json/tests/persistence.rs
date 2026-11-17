#![cfg(unix)]
use rsi_meta::{FiberState, ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{
    Address, EncodedValue, Layout, Mutation, RecordPolicy, StorageFactory, StorageHubContract,
    UnitDescriptor,
};
use rsi_storage_json::{JSON_STORAGE_FORMAT, JsonStorageFactory};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    format: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sha256: Vec<u8>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    files: BTreeMap<String, Vec<u8>>,
    values: BTreeMap<String, Value>,
}
fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "persistence", UpdateMode::Replayable, factory)
}
fn descriptor(layout: Layout) -> UnitDescriptor {
    UnitDescriptor::new(
        match layout {
            Layout::Single => "corpus-single",
            Layout::PerRecord => "corpus-per-record",
        }
        .into(),
        1,
        std::collections::BTreeSet::new(),
        [("rows".into(), RecordPolicy::Reject)].into(),
        true,
        layout,
    )
    .unwrap()
}
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn collect(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                collect(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    collect(root, root, &mut result);
    result
}
#[tokio::test]
#[ignore = "explicit new RSI_JSON_CAPTURE directory only"]
async fn capture_current_writer() {
    let root = PathBuf::from(std::env::var_os("RSI_JSON_CAPTURE").expect("new capture directory"));
    std::fs::create_dir(&root).expect("capture never replaces retained files");
    let private = tempfile::tempdir().unwrap();
    let path = private.path().join("storage");
    let runtime = Runtime::default();
    let hub = runtime
        .root()
        .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
        .await
        .unwrap();
    let plugin = runtime
        .root()
        .apply(
            linked("json", Arc::new(JsonStorageFactory)),
            json!({"name":"json","path":path}),
        )
        .await
        .unwrap();
    assert_eq!(plugin.snapshot().state, FiberState::Active);
    let registry = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let backend = registry.resolve("json").unwrap();
    let values: BTreeMap<String, Value> = BTreeMap::from([
        ("rows/scalar".into(), json!(true)),
        (
            "rows/dynamic".into(),
            json!({"z":1.25,"a":[null,false,18_446_744_073_709_551_615_u64],"text":"中 🦀\n\"\\"}),
        ),
        ("rows/".into(), Value::Null),
        ("rows/../中文\0".into(), json!(42)),
        (String::new(), Value::Null),
    ]);
    for layout in [Layout::Single, Layout::PerRecord] {
        let opened = backend.open(descriptor(layout)).await.unwrap();
        for (key, value) in &values {
            let address = if key.is_empty() {
                Address::global()
            } else {
                Address::record("rows", key.strip_prefix("rows/").unwrap()).unwrap()
            };
            opened
                .unit
                .write(vec![Mutation {
                    address,
                    value: Some(EncodedValue::encode(value).unwrap()),
                }])
                .await
                .unwrap();
        }
        let address = Address::record("rows", "removed").unwrap();
        opened
            .unit
            .write(vec![Mutation {
                address: address.clone(),
                value: Some(EncodedValue::encode(&42).unwrap()),
            }])
            .await
            .unwrap();
        opened
            .unit
            .write(vec![Mutation {
                address,
                value: None,
            }])
            .await
            .unwrap();
    }
    drop(backend);
    drop(registry);
    assert!(plugin.dispose().await.is_clean());
    assert!(hub.dispose().await.is_clean());
    let documents = files(&path);
    let baseline = Baseline {
        format: JSON_STORAGE_FORMAT,
        sha256: Vec::new(),
        files: documents
            .iter()
            .map(|(name, bytes)| (name.clone(), Sha256::digest(bytes).to_vec()))
            .collect(),
        values,
    };
    for (name, bytes) in documents {
        let destination = root.join("files").join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, bytes).unwrap();
    }
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
        let private = tempfile::tempdir().unwrap();
        let path = private.path().join("storage");
        let mut retained = BTreeMap::new();
        if baseline.format == 1 {
            let bytes = std::fs::read(root.join("document.json")).unwrap();
            assert_eq!(Sha256::digest(&bytes).as_slice(), baseline.sha256);
            std::fs::write(&path, &bytes).unwrap();
            retained.insert(String::new(), bytes);
        } else {
            let documents = files(&root.join("files"));
            assert_eq!(documents.len(), baseline.files.len());
            for (name, bytes) in documents {
                assert_eq!(Sha256::digest(&bytes).as_slice(), baseline.files[&name]);
                let destination = path.join(&name);
                std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
                std::fs::write(destination, &bytes).unwrap();
                retained.insert(name, bytes);
            }
        }
        let runtime = Runtime::default();
        let hub = runtime
            .root()
            .apply(linked("hub", Arc::new(StorageFactory)), Value::Null)
            .await
            .unwrap();
        let plugin = runtime
            .root()
            .apply(
                linked("json", Arc::new(JsonStorageFactory)),
                json!({"name":"json","path":path}),
            )
            .await
            .unwrap();
        if baseline.format == 1 {
            assert!(
                matches!(plugin.snapshot().state,FiberState::Failed(message) if message.contains("requires a directory"))
            );
        } else {
            assert_eq!(plugin.snapshot().state, FiberState::Active);
            let backend = runtime
                .root()
                .lookup_local::<StorageHubContract>()
                .unwrap()
                .resolve("json")
                .unwrap();
            for layout in [Layout::Single, Layout::PerRecord] {
                let result = backend.open(descriptor(layout)).await;
                if baseline.format == JSON_STORAGE_FORMAT {
                    current = true;
                    let opened = result.unwrap();
                    assert_eq!(
                        opened
                            .snapshot
                            .records
                            .into_iter()
                            .map(|(address, value)| (
                                address.accounting_key(),
                                value.decode::<Value>().unwrap()
                            ))
                            .collect::<BTreeMap<_, _>>(),
                        baseline.values
                    );
                } else {
                    assert!(
                        matches!(result,Err(rsi_storage::StorageError::Corrupt(message)) if message.contains("unsupported JSON storage format"))
                    );
                }
            }
        }
        for (name, bytes) in retained {
            let source = if name.is_empty() {
                path.clone()
            } else {
                path.join(name)
            };
            assert_eq!(
                std::fs::read(source).unwrap(),
                bytes,
                "open/read/refusal changed durable bytes"
            );
        }
        assert!(plugin.dispose().await.is_clean());
        assert!(hub.dispose().await.is_clean());
    }
    assert!(
        current,
        "current physical format requires a real-writer corpus"
    );
}
