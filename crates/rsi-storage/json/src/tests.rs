use super::*;
use serde_json::json;

fn config(path: PathBuf) -> JsonStorageConfig {
    JsonStorageConfig {
        name: "test".into(),
        path,
        maximum_document_bytes: 1024 * 1024,
    }
}
#[cfg(unix)]
fn spec(layout: Layout, version: u32) -> UnitDescriptor {
    UnitDescriptor::new(
        "unit".into(),
        version,
        std::collections::BTreeSet::new(),
        BTreeMap::from([("rows".into(), RecordPolicy::Reject)]),
        true,
        layout,
    )
    .unwrap()
}
#[cfg(unix)]
fn change(key: &str, value: Option<serde_json::Value>) -> Mutation {
    Mutation {
        address: Address::record("rows", key).unwrap(),
        value: value.map(|v| EncodedValue::encode(&v).unwrap()),
    }
}
#[test]
fn configuration_is_validated_before_native_admission() {
    for path in [PathBuf::from("relative"), PathBuf::new()] {
        assert!(matches!(
            config(path).validate(),
            Err(StorageError::InvalidInput(_))
        ));
    }
    let root = tempfile::tempdir().unwrap();
    let mut options = config(root.path().join("missing"));
    options.maximum_document_bytes = DEFAULT_MAXIMUM_DOCUMENT_BYTES + 1;
    assert!(matches!(
        options.validate(),
        Err(StorageError::InvalidInput(_))
    ));
    assert!(!options.path.exists());
}
#[test]
fn byte_limit_and_underlying_invalid_input_are_distinct() {
    struct Reject;
    impl Write for Reject {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(matches!(
        io::encode_document(&json!("long"), &mut BoundedWriter::new(Vec::new(), 1)),
        Err(StorageError::InvalidInput(_))
    ));
    assert!(matches!(
        io::encode_document(&json!("long"), &mut BoundedWriter::new(Reject, 1024)),
        Err(StorageError::Io(_))
    ));
}

#[cfg(unix)]
mod native {
    use super::*;
    fn assert_single_accounting(unit: &JsonUnit) {
        let state = unit.state.lock().unwrap();
        let UnitState::Single { records, size, .. } = &*state else {
            unreachable!()
        };
        let logical: BTreeMap<_, _> = records
            .iter()
            .map(|(key, value)| (key.accounting_key(), value))
            .collect();
        assert_eq!(size.records(), logical.len());
        assert_eq!(size.bytes(), serde_json::to_vec(&logical).unwrap().len());
    }

    #[tokio::test]
    async fn single_batch_rollback_preserves_values_and_incremental_accounting() {
        let directory = tempfile::tempdir().unwrap();
        let options = config(directory.path().join("storage"));
        let backend = JsonBackend::open(options.clone()).unwrap();
        let unit = owned(&backend, spec(Layout::Single, 1)).await;
        unit.write(vec![
            change("a", Some(json!("original"))),
            change("中\"", Some(json!([1, 2]))),
        ])
        .await
        .unwrap();
        assert_single_accounting(&unit);
        let rejected = unit
            .publish(
                vec![
                    change("a", None),
                    change("中\"", Some(json!(true))),
                    change("new", Some(json!("temporary"))),
                ],
                |_, _, _| Err(StorageError::Io("injected before publication".into())),
            )
            .await;
        assert!(matches!(rejected, Err(StorageError::Io(_))));
        assert_single_accounting(&unit);
        unit.write(vec![
            change("b", Some(json!({"escaped":"\n🦀"}))),
            change("missing", None),
        ])
        .await
        .unwrap();
        assert_single_accounting(&unit);
        drop(unit);
        let reopened = JsonBackend::open(options)
            .unwrap()
            .open(spec(Layout::Single, 1))
            .await
            .unwrap();
        assert_eq!(reopened.snapshot.records.len(), 3);
        assert_eq!(
            reopened.snapshot.records[&Address::record("rows", "a").unwrap()]
                .decode::<String>()
                .unwrap(),
            "original"
        );
        assert_eq!(
            reopened.snapshot.records[&Address::record("rows", "中\"").unwrap()]
                .decode::<Vec<u8>>()
                .unwrap(),
            [1, 2]
        );
        reopened
            .unit
            .write(vec![
                change("a", None),
                change("中\"", None),
                change("b", None),
            ])
            .await
            .unwrap();
        drop(reopened);
        let unit = owned(&backend, spec(Layout::Single, 1)).await;
        assert_single_accounting(&unit);
        unit.write(vec![change("next", Some(json!(false)))])
            .await
            .unwrap();
        assert_single_accounting(&unit);
    }
    #[tokio::test]
    async fn writes_reject_replaced_root_symlinks_in_both_layouts() {
        for layout in [Layout::Single, Layout::PerRecord] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("storage");
            let foreign = directory.path().join("foreign");
            std::fs::create_dir(&foreign).unwrap();
            let backend = JsonBackend::open(config(root.clone())).unwrap();
            let opened = backend.open(spec(layout, 1)).await.unwrap();
            opened
                .unit
                .write(vec![change("a", Some(json!(1)))])
                .await
                .unwrap();
            std::fs::rename(&root, directory.path().join("original")).unwrap();
            std::os::unix::fs::symlink(&foreign, &root).unwrap();
            assert!(
                opened
                    .unit
                    .write(vec![change("b", Some(json!(2)))])
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read_dir(foreign).unwrap().count(), 0);
        }
    }
    #[tokio::test]
    async fn deleting_records_rejects_a_replaced_table_parent_without_touching_its_target() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("storage");
        let backend = JsonBackend::open(config(root.clone())).unwrap();
        let opened = backend.open(spec(Layout::PerRecord, 1)).await.unwrap();
        opened
            .unit
            .write(vec![change("a", Some(json!(1)))])
            .await
            .unwrap();
        let table = root.join(hash(b"unit")).join(hash(b"rows"));
        let foreign = directory.path().join("foreign");
        std::fs::rename(&table, &foreign).unwrap();
        let record = foreign.join(format!("{}.json", hash(b"a")));
        let before = std::fs::read(&record).unwrap();
        std::os::unix::fs::symlink(&foreign, &table).unwrap();
        assert!(opened.unit.write(vec![change("a", None)]).await.is_err());
        assert_eq!(std::fs::read(record).unwrap(), before);
        assert_eq!(std::fs::read_dir(foreign).unwrap().count(), 1);
    }
    fn unit(backend: &JsonBackend, descriptor: UnitDescriptor, state: UnitState) -> JsonUnit {
        JsonUnit {
            owner: Arc::new(UnitOwner {
                backend: backend.clone(),
                _lease: backend.units.reserve(descriptor.name()).unwrap(),
                descriptor,
            }),
            state: Arc::new(Mutex::new(state)),
        }
    }
    async fn owned(backend: &JsonBackend, descriptor: UnitDescriptor) -> JsonUnit {
        let opened = backend.open(descriptor.clone()).await.unwrap();
        // Reconstruct the private handle only to inject persistence faults; load and
        // all public conformance use the normal backend entry point.
        drop(opened);
        let owner = UnitOwner {
            backend: backend.clone(),
            _lease: backend.units.reserve(descriptor.name()).unwrap(),
            descriptor: descriptor.clone(),
        };
        let (_, state) = single::load(
            &owner,
            &backend
                .config
                .path
                .join(format!("{}.json", hash(descriptor.name().as_bytes()))),
        )
        .unwrap();
        drop(owner);
        unit(backend, descriptor, state)
    }
    #[tokio::test]
    async fn cancelled_publication_owns_the_slot_until_memory_and_disk_agree() {
        use std::{
            future::Future,
            pin::pin,
            task::{Context, Poll, Waker},
        };
        for delete in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let config = config(root.path().join("storage"));
            let backend = JsonBackend::open(config.clone()).unwrap();
            let unit = Arc::new(owned(&backend, spec(Layout::Single, 1)).await);
            unit.write(vec![change("a", Some(json!(null)))])
                .await
                .unwrap();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let first = tokio::spawn({
                let unit = unit.clone();
                async move {
                    unit.publish(
                        vec![change("a", (!delete).then(|| json!(true)))],
                        move |path, limit, document| {
                            entered.send(()).unwrap();
                            released.blocking_recv().unwrap();
                            atomic_write(path, limit, document)
                        },
                    )
                    .await
                }
            });
            started.await.unwrap();
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            let mut second = pin!(unit.write(vec![change("b", Some(json!(null)))]));
            assert!(matches!(
                second
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
            release.send(()).unwrap();
            second.await.unwrap();
            let durable = JsonBackend::open(config)
                .unwrap()
                .open(spec(Layout::Single, 1))
                .await
                .unwrap();
            let state = unit.state.lock().unwrap();
            let UnitState::Single { records, .. } = &*state else {
                unreachable!()
            };
            assert_eq!(durable.snapshot.records.len(), records.len());
            for (address, value) in records {
                assert_eq!(
                    durable.snapshot.records[address].as_bytes(),
                    value.as_bytes()
                );
            }
            assert_eq!(records.len(), if delete { 1 } else { 2 });
        }
    }
    #[tokio::test]
    async fn post_replace_failure_fences_all_units_until_reopen() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path().join("storage"));
        let backend = JsonBackend::open(config.clone()).unwrap();
        let first = owned(&backend, spec(Layout::Single, 1)).await;
        let other_spec = UnitDescriptor::new(
            "other".into(),
            1,
            std::collections::BTreeSet::new(),
            spec(Layout::Single, 1).tables().clone(),
            false,
            Layout::PerRecord,
        )
        .unwrap();
        let other = backend.open(other_spec).await.unwrap();
        first
            .write(vec![change("a", Some(json!(null)))])
            .await
            .unwrap();
        let result = first
            .publish(
                vec![change("a", Some(json!(true)))],
                |path, limit, document| {
                    io::atomic_write_with_sync(path, limit, document, |_| {
                        Err(std::io::Error::other("injected sync"))
                    })
                },
            )
            .await;
        assert!(matches!(result, Err(StorageError::OutcomeUnknown(_))));
        assert_eq!(
            other.unit.ensure_available(),
            Err(StorageError::RecoveryRequired)
        );
        assert_eq!(
            other.unit.write(vec![change("b", Some(json!(1)))]).await,
            Err(StorageError::RecoveryRequired)
        );
        assert!(matches!(
            backend.open(spec(Layout::Single, 1)).await,
            Err(StorageError::RecoveryRequired)
        ));
        let reopened = JsonBackend::open(config)
            .unwrap()
            .open(spec(Layout::Single, 1))
            .await
            .unwrap();
        assert!(
            reopened.snapshot.records[&Address::record("rows", "a").unwrap()]
                .decode::<bool>()
                .unwrap()
        );
    }
    #[tokio::test]
    async fn rejected_encoding_preserves_file_and_generation_admission() {
        for layout in [Layout::Single, Layout::PerRecord] {
            let root = tempfile::tempdir().unwrap();
            let mut config = config(root.path().join("storage"));
            config.maximum_document_bytes = 256;
            let backend = JsonBackend::open(config).unwrap();
            let opened = backend.open(spec(layout, 1)).await.unwrap();
            opened
                .unit
                .write(vec![change("a", Some(json!(true)))])
                .await
                .unwrap();
            assert!(matches!(
                opened
                    .unit
                    .write(vec![change("a", Some(json!("x".repeat(512))))])
                    .await,
                Err(StorageError::InvalidInput(_))
            ));
            opened.unit.ensure_available().unwrap();
            drop(opened);
            let opened = backend.open(spec(layout, 1)).await.unwrap();
            assert!(
                opened.snapshot.records[&Address::record("rows", "a").unwrap()]
                    .decode::<bool>()
                    .unwrap()
            );
        }
    }
    #[tokio::test]
    async fn accepted_depth_survives_reopen_in_both_layouts() {
        for layout in [Layout::Single, Layout::PerRecord] {
            let root = tempfile::tempdir().unwrap();
            let config = config(root.path().join("storage"));
            let backend = JsonBackend::open(config.clone()).unwrap();
            let opened = backend.open(spec(layout, 1)).await.unwrap();
            let mut value = json!(null);
            for depth in 1..rsi_storage::MAXIMUM_STORAGE_VALUE_DEPTH {
                value = if depth % 2 == 0 {
                    json!({"child":value})
                } else {
                    json!([value])
                };
            }
            opened
                .unit
                .write(vec![change("a", Some(value.clone()))])
                .await
                .unwrap();
            assert!(EncodedValue::encode(&json!([value.clone()])).is_err());
            drop(opened);
            let reopened = JsonBackend::open(config)
                .unwrap()
                .open(spec(layout, 1))
                .await
                .unwrap();
            assert_eq!(
                reopened.snapshot.records[&Address::record("rows", "a").unwrap()]
                    .decode::<serde_json::Value>()
                    .unwrap(),
                value
            );
        }
    }
    #[tokio::test]
    async fn absent_delete_does_not_materialize_and_read_does_not_restamp() {
        for layout in [Layout::Single, Layout::PerRecord] {
            let root = tempfile::tempdir().unwrap();
            let backend = JsonBackend::open(config(root.path().join("storage"))).unwrap();
            let opened = backend.open(spec(layout, 1)).await.unwrap();
            opened
                .unit
                .write(vec![change("missing", None)])
                .await
                .unwrap();
            assert!(!backend.config.path.exists());
            opened
                .unit
                .write(vec![change("a", Some(json!(true)))])
                .await
                .unwrap();
            drop(opened);
            let next = UnitDescriptor::new(
                "unit".into(),
                2,
                [1].into(),
                spec(layout, 1).tables().clone(),
                true,
                layout,
            )
            .unwrap();
            let path = if layout == Layout::Single {
                backend.config.path.join(format!("{}.json", hash(b"unit")))
            } else {
                records::record_path(
                    &backend.config.path.join(hash(b"unit")),
                    &Address::record("rows", "a").unwrap(),
                )
            };
            let before = fs::read(&path).unwrap();
            let opened = backend.open(next.clone()).await.unwrap();
            opened
                .unit
                .write(vec![change("missing", None)])
                .await
                .unwrap();
            assert_eq!(fs::read(&path).unwrap(), before);
            opened
                .unit
                .write(vec![change("b", Some(json!(false)))])
                .await
                .unwrap();
            if layout == Layout::PerRecord {
                assert_eq!(fs::read(&path).unwrap(), before);
            }
            drop(opened);
            backend.open(next).await.unwrap();
        }
    }
    #[test]
    fn failed_delete_staging_restores_every_moved_source() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("a.json");
        fs::write(&first, b"original").unwrap();
        assert!(matches!(
            records::delete_many([first.clone(), root.path().join("absent.json")].into_iter()),
            Err(StorageError::Io(_))
        ));
        assert_eq!(fs::read(&first).unwrap(), b"original");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[tokio::test]
    async fn only_disposable_syntax_and_values_are_backed_up_with_original_bytes() {
        let root = tempfile::tempdir().unwrap();
        let backend = JsonBackend::open(config(root.path().join("storage"))).unwrap();
        let disposable = UnitDescriptor::new(
            "unit".into(),
            1,
            std::collections::BTreeSet::new(),
            BTreeMap::from([("rows".into(), RecordPolicy::BackupAndSkip)]),
            true,
            Layout::PerRecord,
        )
        .unwrap();
        let opened = backend.open(disposable.clone()).await.unwrap();
        opened
            .unit
            .write(vec![change("bad", Some(json!(3)))])
            .await
            .unwrap();
        drop(opened);
        let path = records::record_path(
            &backend.config.path.join(hash(b"unit")),
            &Address::record("rows", "bad").unwrap(),
        );
        fs::write(&path, b"{broken").unwrap();
        let opened = backend.open(disposable.clone()).await.unwrap();
        assert!(opened.snapshot.records.is_empty());
        assert_eq!(opened.snapshot.recoveries.len(), 1);
        assert_eq!(
            fs::read(&opened.snapshot.recoveries[0].backup).unwrap(),
            b"{broken"
        );
        assert!(!path.exists());
        opened
            .unit
            .write(vec![change("bad", Some(json!(4)))])
            .await
            .unwrap();
        drop(opened);
        let valid = fs::read_to_string(&path).unwrap();
        for bad in [
            valid.replace("\"version\":1", "\"version\":9"),
            valid.replace("\"unit\":\"unit\"", "\"unit\":\"other\""),
        ] {
            fs::write(&path, &bad).unwrap();
            assert!(backend.open(disposable.clone()).await.is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), bad);
        }
        assert_eq!(
            fs::read_dir(backend.config.path.join(".quarantine"))
                .unwrap()
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn interrupted_empty_materialization_can_be_opened_without_importing_records() {
        let root = tempfile::tempdir().unwrap();
        let backend = JsonBackend::open(config(root.path().join("storage"))).unwrap();
        let unit_root = backend.config.path.join(hash(b"unit"));
        ensure_directory(&unit_root).unwrap();
        fs::write(unit_root.join(".unit.json.1.1.tmp"), b"incomplete").unwrap();
        let opened = backend.open(spec(Layout::PerRecord, 1)).await.unwrap();
        assert!(opened.snapshot.records.is_empty());
        opened
            .unit
            .write(vec![change("a", Some(json!(true)))])
            .await
            .unwrap();
        drop(opened);
        assert_eq!(
            backend
                .open(spec(Layout::PerRecord, 1))
                .await
                .unwrap()
                .snapshot
                .records
                .len(),
            1
        );
    }
}
