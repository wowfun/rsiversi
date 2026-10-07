use super::*;
use std::ops::ControlFlow;
use tokio_util::sync::CancellationToken;

async fn inspect_once(
    root: &Path,
    mode: CasInspectionMode,
    cancellation: CancellationToken,
    visitor: impl FnMut(CasInspectionEvent) -> ControlFlow<()> + Send + 'static,
) -> std::result::Result<CasInspectionSummary, CasInspectionError> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        SqliteStore::inspect_cas(root, mode, &cancellation, visitor)
    })
    .await
    .unwrap()
}

async fn inspect(
    root: &Path,
    mode: CasInspectionMode,
) -> (CasInspectionSummary, Vec<CasInspectionEvent>) {
    let cancellation = CancellationToken::new();
    let mut scanning = Box::pin(async {
        loop {
            let captured = Arc::new(Mutex::new(Vec::new()));
            let visitor_events = captured.clone();
            let result = inspect_once(root, mode, cancellation.clone(), move |event| {
                visitor_events.lock().unwrap().push(event);
                ControlFlow::Continue(())
            })
            .await;
            let events = std::mem::take(&mut *captured.lock().unwrap());
            match result {
                Err(error) if matches!(error.source, StoreError::WriterLocked) => {
                    tokio::task::yield_now().await;
                }
                result => return (result.unwrap(), events),
            }
        }
    });
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut scanning).await {
        result
    } else {
        cancellation.cancel();
        // A dropped JoinHandle leaves its blocking worker running. Keep the
        // original operation until cooperative cancellation releases its lease.
        let _ = scanning.await;
        panic!("offline inspection exceeded its deadline");
    }
}

#[tokio::test(start_paused = true)]
async fn inspection_worker_allows_runtime_timer_to_cancel_a_running_scan() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    for body in [b"first".as_slice(), b"second"] {
        store.put_cas(Arc::from(body)).await.unwrap();
    }
    drop(store);
    let cancellation = CancellationToken::new();
    let worker_cancel = cancellation.clone();
    let worker_root = root.path().to_path_buf();
    let (entered, entering) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = tokio::spawn(async move {
        let mut entered = Some(entered);
        inspect_once(
            &worker_root,
            CasInspectionMode::Full,
            worker_cancel,
            move |_| {
                if let Some(entered) = entered.take() {
                    let _ = entered.send(());
                    let _ = released.recv();
                }
                ControlFlow::Continue(())
            },
        )
        .await
    });
    entering.await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let timer_cancel = cancellation.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        timer_cancel.cancel();
        release.send(()).unwrap();
    });
    tokio::time::advance(Duration::from_secs(5)).await;
    cancellation.cancelled().await;
    timer.await.unwrap();
    let summary = worker.await.unwrap().unwrap();
    assert_eq!(summary.completion, CasInspectionCompletion::Cancelled);
    assert_eq!(summary.metadata_rows, 1);
    assert_eq!(summary.verified_objects, 1);
    assert!(
        SqliteStore::open(root.path()).is_ok(),
        "worker must release its lease"
    );
}

#[tokio::test]
async fn inspection_is_read_only_reports_retained_files_and_hashes_exact_bodies() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let reference = store
        .put_cas(Arc::from(b"retained capture".as_slice()))
        .await
        .unwrap();
    drop(store);
    let orphan = b"unregistered body";
    let digest = hex::encode(Sha256::digest(orphan));
    fs::write(root.path().join("cas").join(&digest), orphan).unwrap();
    fs::write(root.path().join("cas/unknown-name"), b"unknown owner").unwrap();
    let staging = root.path().join("cas/staging/retained.tmp");
    fs::write(&staging, b"retained temporary").unwrap();
    let database = root.path().join("sessions.sqlite3");
    let before = fs::read(&database).unwrap();
    let (metadata, _) = inspect(root.path(), CasInspectionMode::Metadata).await;
    assert_eq!(metadata.completion, CasInspectionCompletion::Complete);
    assert_eq!(metadata.metadata_rows, 1);
    assert_eq!(metadata.hashed_bytes, 0);
    assert_eq!(metadata.unregistered_files, 1);
    let (full, events) = inspect(root.path(), CasInspectionMode::Full).await;
    assert_eq!(full.verified_objects, 1);
    assert_eq!(full.hashed_bytes, reference.byte_len);
    assert_eq!(full.unregistered_bytes, orphan.len() as u64);
    assert!(
        events
            .iter()
            .any(|event| event.issue == Some(CasInspectionIssue::Unregistered))
    );
    assert!(
        events
            .iter()
            .any(|event| event.issue == Some(CasInspectionIssue::UnexpectedEntry))
    );
    assert_eq!(fs::read(database).unwrap(), before);
    assert_eq!(fs::read(staging).unwrap(), b"retained temporary");
    assert_eq!(
        fs::read(root.path().join("cas").join(digest)).unwrap(),
        orphan
    );
    assert_eq!(
        fs::read(root.path().join("cas/unknown-name")).unwrap(),
        b"unknown owner"
    );
}

#[tokio::test]
async fn inspection_detects_missing_length_and_digest_errors_without_hiding_other_rows() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut objects = Vec::new();
    for body in [b"missing".as_slice(), b"shorter", b"digest"] {
        objects.push(store.put_cas(Arc::from(body)).await.unwrap());
    }
    drop(store);
    fs::remove_file(root.path().join("cas").join(&objects[0].sha256)).unwrap();
    fs::write(root.path().join("cas").join(&objects[1].sha256), b"short").unwrap();
    fs::write(root.path().join("cas").join(&objects[2].sha256), b"wrong!").unwrap();
    let (summary, events) = inspect(root.path(), CasInspectionMode::Full).await;
    assert_eq!(summary.completion, CasInspectionCompletion::Complete);
    assert_eq!(summary.metadata_rows, 3);
    assert_eq!(summary.verified_objects, 0);
    for expected in [
        CasInspectionIssue::Missing,
        CasInspectionIssue::LengthMismatch {
            expected: 7,
            actual: 5,
        },
        CasInspectionIssue::DigestMismatch,
    ] {
        assert!(
            events
                .iter()
                .any(|event| event.issue == Some(expected.clone())),
            "{expected:?}"
        );
    }
}

#[tokio::test]
async fn inspection_bounds_corrupt_keys_and_continues_across_negative_rowids_and_pages() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    drop(store);
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO cas_objects(rowid, sha256, byte_len) VALUES (?1, ?2, 1)",
            params![i64::MIN, "a".repeat(64)],
        )
        .unwrap();
    for index in 0..260_i64 {
        connection
            .execute(
                "INSERT INTO cas_objects(rowid, sha256, byte_len) VALUES (?1, ?2, 1)",
                params![index - 20, format!("{index:064x}")],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO cas_objects(sha256, byte_len) VALUES (printf('%0500000d', 1), 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cas_objects(sha256, byte_len) VALUES (?1, ?2)",
            params![
                "f".repeat(64),
                i64::try_from(MAXIMUM_STORE_CAS_BYTES).unwrap() + 1
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cas_objects(sha256, byte_len) VALUES (?1, 1)",
            ["E".repeat(64)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cas_objects(sha256, byte_len) VALUES (CAST(?1 AS TEXT), 1)",
            [vec![0xff; 64]],
        )
        .unwrap();
    // Pinned SQLite marks column access for octet_length() with BYTELENARG
    // (0xc0), avoiding corrupt-key body materialization in the SQL worker too.
    let mut statement = connection
        .prepare("EXPLAIN SELECT octet_length(sha256) FROM cas_objects")
        .unwrap();
    let flags = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i32>(6)?))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        flags
            .iter()
            .any(|(opcode, flags)| opcode == "Column" && flags & 0xc0 == 0xc0)
    );
    drop(statement);
    drop(connection);
    let (summary, events) = inspect(root.path(), CasInspectionMode::Metadata).await;
    assert_eq!(summary.metadata_rows, 265);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.issue == Some(CasInspectionIssue::InvalidMetadata))
            .count(),
        4
    );
    assert!(events.iter().any(|event| matches!(
        event.location,
        CasInspectionLocation::Metadata { rowid: -20, .. }
    )));
    assert!(events.iter().any(|event| matches!(
        event.location,
        CasInspectionLocation::Metadata {
            rowid: i64::MIN,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event.location,
        CasInspectionLocation::Metadata {
            sha256: None,
            key_bytes: Some(500_000),
            ..
        }
    )));
    assert!(
        events
            .iter()
            .all(|event| !format!("{event:?}").contains(&"0".repeat(1000)))
    );
}

#[tokio::test]
async fn full_inspection_checks_canonical_program_references_beyond_sqlite_verify() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    Box::pin(rsi_agent_testkit::assert_program_store_contract(
        &store,
        &test_header("inspection-program"),
        &test_fact(1),
    ))
    .await;
    drop(store);
    let (summary, events) = inspect(root.path(), CasInspectionMode::Full).await;
    assert!(summary.references > 0);
    assert!(events.iter().any(|event| matches!(
        event.location,
        CasInspectionLocation::Reference { .. }
    ) && event.issue == Some(CasInspectionIssue::Missing)));
    SqliteStore::verify(root.path()).unwrap();
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO cas_objects(sha256, byte_len) VALUES (?1, 11)",
            ["a".repeat(64)],
        )
        .unwrap();
    drop(connection);
    let (_, events) = inspect(root.path(), CasInspectionMode::Full).await;
    assert!(events.iter().any(|event| matches!(
        event.location,
        CasInspectionLocation::Reference { .. }
    ) && event.issue
        == Some(CasInspectionIssue::LengthMismatch {
            expected: 10,
            actual: 11
        })));
}

#[tokio::test]
async fn inspection_reports_stop_and_cancellation_and_safely_restarts() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    for body in [b"one".as_slice(), b"two"] {
        store.put_cas(Arc::from(body)).await.unwrap();
    }
    drop(store);
    let stopped = inspect_once(
        root.path(),
        CasInspectionMode::Full,
        CancellationToken::new(),
        |_| ControlFlow::Break(()),
    )
    .await
    .unwrap();
    assert_eq!(stopped.completion, CasInspectionCompletion::VisitorStopped);
    assert_eq!(stopped.metadata_rows, 1);
    let cancellation = CancellationToken::new();
    let visitor_cancel = cancellation.clone();
    let cancelled = inspect_once(
        root.path(),
        CasInspectionMode::Full,
        cancellation,
        move |_| {
            visitor_cancel.cancel();
            ControlFlow::Continue(())
        },
    )
    .await
    .unwrap();
    assert_eq!(cancelled.completion, CasInspectionCompletion::Cancelled);
    let (completed, _) = inspect(root.path(), CasInspectionMode::Full).await;
    assert_eq!(completed.metadata_rows, 2);
    assert_eq!(completed.verified_objects, 2);
    assert_eq!(completed.completion, CasInspectionCompletion::Complete);
}

#[tokio::test]
async fn inspection_refuses_active_writer_wal_and_missing_root_without_creation() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let error = SqliteStore::inspect_cas(
        root.path(),
        CasInspectionMode::Metadata,
        &CancellationToken::new(),
        |_| ControlFlow::Continue(()),
    )
    .unwrap_err();
    assert!(matches!(error.source, StoreError::WriterLocked));
    assert_eq!(error.partial.completion, CasInspectionCompletion::Failed);
    drop(store);
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL; INSERT INTO cas_objects VALUES ('invalid', 1)")
        .unwrap();
    let wal = root.path().join("sessions.sqlite3-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);
    let before = fs::read(&wal).unwrap();
    assert!(
        SqliteStore::inspect_cas(
            root.path(),
            CasInspectionMode::Metadata,
            &CancellationToken::new(),
            |_| ControlFlow::Continue(())
        )
        .is_err()
    );
    assert_eq!(fs::read(wal).unwrap(), before);
    drop(connection);
    let absent = root.path().join("absent");
    assert!(
        SqliteStore::inspect_cas(
            &absent,
            CasInspectionMode::Full,
            &CancellationToken::new(),
            |_| ControlFlow::Continue(())
        )
        .is_err()
    );
    assert!(!absent.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn inspection_never_follows_leaf_or_cas_root_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let object = store.put_cas(Arc::from(b"body".as_slice())).await.unwrap();
    drop(store);
    let outside = root.path().join("outside");
    fs::write(&outside, b"outside data").unwrap();
    let leaf = root.path().join("cas").join(object.sha256);
    fs::remove_file(&leaf).unwrap();
    std::os::unix::fs::symlink(&outside, &leaf).unwrap();
    let (summary, events) = inspect(root.path(), CasInspectionMode::Full).await;
    assert_eq!(summary.hashed_bytes, 0);
    assert!(
        events
            .iter()
            .any(|event| event.issue == Some(CasInspectionIssue::UnexpectedEntry))
    );
    assert_eq!(fs::read(&outside).unwrap(), b"outside data");
    fs::rename(root.path().join("cas"), root.path().join("old-cas")).unwrap();
    std::os::unix::fs::symlink(root.path().join("old-cas"), root.path().join("cas")).unwrap();
    let error = SqliteStore::inspect_cas(
        root.path(),
        CasInspectionMode::Full,
        &CancellationToken::new(),
        |_| ControlFlow::Continue(()),
    )
    .unwrap_err();
    assert_eq!(error.partial.completion, CasInspectionCompletion::Failed);
    assert_eq!(error.partial.hashed_bytes, 0);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One durable fixture checks both canonical reference streams.
async fn full_inspection_checks_frozen_references_in_controls_and_facts() {
    use rsi_agent_session_protocol::{
        FrozenReference, InputMessageSource, MessageDelivery, MessageOptions, ReferenceBinding,
        ReferenceCapture, ReferenceContentKind, ReferenceMetadata, ReferenceRecord,
        ReferenceSelection, ReferenceSnapshotRef, ReferenceSource,
    };
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "frozen-inspection").await;
    let digest = "c".repeat(64);
    let reference = FrozenReference {
        snapshot: ReferenceSnapshotRef {
            sha256: digest.clone(),
            byte_len: 10,
        },
        metadata: ReferenceMetadata {
            source: ReferenceSource::Observed {
                owner: "test".into(),
                id: "source".into(),
                epoch: 1,
            },
            target: ReferenceBinding {
                session_id: id.clone(),
                header_sha256: test_header("frozen-inspection").fingerprint().unwrap(),
            },
            capture: ReferenceCapture::Selected {
                selection: ReferenceSelection {
                    record: ReferenceRecord {
                        sequence: 1,
                        kind: ReferenceContentKind::Human,
                        content_index: 0,
                    },
                    through_seq: 1,
                    start: 0,
                    end: 1,
                    text_sha256: "d".repeat(64),
                    scanned_bytes: 1,
                },
            },
            text_bytes: 1,
        },
        preview: "x".into(),
    };
    reference.validate().unwrap();
    let content = vec![rsi_agent_session_protocol::AgentMessageContent::Reference { reference }];
    let message_id = MessageId::new("frozen-message").unwrap();
    let message = AgentMessage {
        message_id: message_id.clone(),
        source: AgentMessageSource::Human,
        content: content.clone(),
        options: MessageOptions::default(),
    };
    store
        .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
            sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                session_id: id.clone(),
                expected_fact_seq: 1,
                expected_control_seq: 0,
                header: None,
                facts: vec![
                    SessionFact::new(
                        2,
                        2,
                        SessionFactBody::StepStarted {
                            turn_id: TurnId::new("turn-1").unwrap(),
                            step_id: StepId::new("step").unwrap(),
                        },
                    )
                    .unwrap()
                    .into(),
                    SessionFact::new(
                        3,
                        3,
                        SessionFactBody::InputMessageEntered {
                            turn_id: TurnId::new("turn-1").unwrap(),
                            step_id: StepId::new("step").unwrap(),
                            source: InputMessageSource::Human { message_id },
                            content,
                        },
                    )
                    .unwrap()
                    .into(),
                ],
                controls: vec![
                    AgentControlRecord::new(
                        1,
                        1,
                        AgentControlRecordBody::MessageAccepted {
                            message,
                            delivery: MessageDelivery::NextTurn,
                            bound_turn_id: None,
                            root_session_id: id,
                            target: MessageTarget::NextTurn,
                            wake_required: true,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    drop(store);
    let (summary, events) = inspect(root.path(), CasInspectionMode::Full).await;
    assert_eq!(summary.references, 2);
    for stream in [CasInspectionStream::Control, CasInspectionStream::Fact] {
        assert!(events.iter().any(|event| event.issue == Some(CasInspectionIssue::Missing)
            && matches!(&event.location, CasInspectionLocation::Reference { stream: actual, sha256, .. } if *actual == stream && sha256 == &digest)));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn inspection_bounds_multibyte_filename_previews() {
    let root = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(root.path()).unwrap());
    let name = "界".repeat(66);
    fs::write(root.path().join("cas").join(name), b"unowned").unwrap();
    let (_, events) = inspect(root.path(), CasInspectionMode::Metadata).await;
    assert!(
        events
            .iter()
            .any(|event| event.issue == Some(CasInspectionIssue::UnexpectedEntry))
    );
    assert!(events.iter().all(|event| match &event.location {
        CasInspectionLocation::File { name } => name.len() <= 128,
        _ => true,
    }));
}

#[cfg(unix)]
#[test]
fn filename_preview_bounds_invalid_bytes_without_requiring_filesystem_support() {
    use std::os::unix::ffi::OsStringExt;
    let name = std::ffi::OsString::from_vec(vec![0xff; 200]);
    let preview = crate::inspection::file_name_preview(&name);
    assert_eq!(preview, "\u{fffd}".repeat(42));
    assert!(preview.len() <= 128);
}
