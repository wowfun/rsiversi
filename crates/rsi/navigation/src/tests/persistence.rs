use super::*;
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;

fn schemas() -> serde_json::Value {
    use rsi_persistence_testkit::structural;
    serde_json::json!({
        "": structural::<Revision>(),
        "metadata": structural::<StoredMetadata>(),
        "orders": structural::<Vec<SessionId>>(),
    })
}

#[tokio::test]
#[ignore = "requires a new RSI_NAVIGATION_CAPTURE directory"]
async fn capture_current_writer() {
    let root =
        PathBuf::from(std::env::var_os("RSI_NAVIGATION_CAPTURE").expect("new capture directory"));
    std::fs::create_dir(&root).expect("retained corpus is never replaced");
    let mut owner = owner().await;
    let domain = TestDomain::new(owner.domain.spec().clone()).await;
    Arc::get_mut(&mut owner).unwrap().domain = domain.domain.clone();
    let mut files = BTreeMap::new();
    for (name, session, metadata) in [
        (
            "title",
            "one",
            SessionMetadata {
                title: Some("中 🦀 quoted \" and slash \\".into()),
                ..Default::default()
            },
        ),
        (
            "pinned",
            "one",
            SessionMetadata {
                title: Some("pinned".into()),
                pinned: true,
                ..Default::default()
            },
        ),
        (
            "archived",
            "two",
            SessionMetadata {
                archived: true,
                ..Default::default()
            },
        ),
        ("removed", "one", SessionMetadata::default()),
    ] {
        let published = owner
            .write_metadata(&SessionId::new(session).unwrap(), &metadata)
            .await
            .unwrap();
        let bytes = snapshot_bytes(&domain);
        let file = format!("{name}.json");
        files.insert(file.clone(), hex::encode(Sha256::digest(&bytes)));
        std::fs::write(root.join(file), bytes).unwrap();
        drop(published);
    }
    let change = owner
        .document()
        .project_order(
            &WorkspaceId::from_bytes([0xab; 32]),
            vec![SessionId::new("two").unwrap()],
        )
        .unwrap();
    owner.publish(change).await.unwrap();
    let bytes = snapshot_bytes(&domain);
    files.insert("order.json".into(), hex::encode(Sha256::digest(&bytes)));
    std::fs::write(root.join("order.json"), bytes).unwrap();
    let schema = serde_json::to_vec_pretty(&schemas()).unwrap();
    std::fs::write(root.join("schema.json"), &schema).unwrap();
    std::fs::write(
        root.join("baseline.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"version":NAVIGATION_STORAGE_VERSION,"files":files,"schema_sha256":hex::encode(Sha256::digest(&schema))}),
        )
        .unwrap(),
    )
    .unwrap();
    owner.close().await;
}

fn snapshot_bytes(domain: &TestDomain) -> Vec<u8> {
    let units = domain.backend.state.units.lock().unwrap();
    let rows: Vec<_> = units[&domain.domain.spec().id]
        .records
        .iter()
        .map(|(address, value)| (address.table(), address.key(), value))
        .collect();
    serde_json::to_vec(&rows).unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Retained versions are checked through the same backend admission and read/refusal path."
)]
async fn retained_payload_bytes() {
    use rsi_storage::{Address, EncodedValue, StorageError, UnitSnapshot};
    use rsi_storage_testkit::MemoryBackend;
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/persistence");
    let mut current = false;
    for entry in std::fs::read_dir(home).unwrap() {
        let root = entry.unwrap().path();
        let baseline: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("baseline.json")).unwrap()).unwrap();
        let version = u32::try_from(baseline["version"].as_u64().unwrap()).unwrap();
        current |= version == NAVIGATION_STORAGE_VERSION;
        if version == NAVIGATION_STORAGE_VERSION {
            let stored = std::fs::read(root.join("schema.json")).unwrap();
            assert_eq!(
                hex::encode(Sha256::digest(&stored)),
                baseline["schema_sha256"].as_str().unwrap()
            );
            assert_eq!(
                stored,
                serde_json::to_vec_pretty(&schemas()).unwrap(),
                "same-version schema or field order changed"
            );
        }
        for (file, hash) in baseline["files"].as_object().unwrap() {
            let bytes = std::fs::read(root.join(file)).unwrap();
            assert_eq!(hex::encode(Sha256::digest(&bytes)), hash.as_str().unwrap());
            let backend = Arc::new(MemoryBackend::default());
            let mut records = BTreeMap::new();
            if version == NAVIGATION_STORAGE_VERSION {
                let rows: Vec<(String, String, Box<serde_json::value::RawValue>)> =
                    serde_json::from_slice(&bytes).unwrap();
                for (table, key, value) in rows {
                    let address = if table.is_empty() {
                        assert!(key.is_empty());
                        Address::global()
                    } else {
                        Address::record(&table, &key).unwrap()
                    };
                    let value = EncodedValue::copy_raw(&value).unwrap();
                    let schema = rsi_persistence_testkit::validation_schema(&schemas()[&table]);
                    let validator = jsonschema::options()
                        .with_draft(jsonschema::Draft::Draft7)
                        .build(&schema)
                        .unwrap();
                    let payload: serde_json::Value = value.decode().unwrap();
                    assert!(
                        validator.is_valid(&payload),
                        "writer payload violates its serialization schema"
                    );
                    let encoded = match table.as_str() {
                        "" => EncodedValue::encode(&value.decode::<Revision>().unwrap()),
                        "metadata" => {
                            EncodedValue::encode(&value.decode::<StoredMetadata>().unwrap())
                        }
                        "orders" => {
                            EncodedValue::encode(&value.decode::<Vec<SessionId>>().unwrap())
                        }
                        _ => panic!("undeclared table"),
                    }
                    .unwrap();
                    assert_eq!(
                        encoded.as_bytes(),
                        value.as_bytes(),
                        "retained payload encoding changed"
                    );
                    assert!(records.insert(address, value).is_none());
                }
            } else {
                records.insert(
                    Address::global(),
                    EncodedValue::from_bytes(bytes.clone()).unwrap(),
                );
            }
            let spec = domain_spec("fixture".into());
            backend.state.units.lock().unwrap().insert(
                spec.id.clone(),
                UnitSnapshot {
                    version: Some(version),
                    records,
                    recoveries: vec![],
                },
            );
            let opened = TestDomain::open(spec, backend.clone()).await;
            if version == NAVIGATION_STORAGE_VERSION {
                let fixture = opened.unwrap();
                load_document(&fixture.domain).unwrap();
                assert_eq!(snapshot_bytes(&fixture), bytes);
                fixture.domain.close().await;
            } else {
                assert!(matches!(
                    opened,
                    Err(StorageError::DomainVersionMismatch { .. })
                ));
            }
            assert_eq!(
                backend
                    .state
                    .writes
                    .load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            assert_eq!(std::fs::read(root.join(file)).unwrap(), bytes);
        }
    }
    assert!(
        current,
        "current navigation version needs an actual writer corpus"
    );
}
