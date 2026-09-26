use super::*;

#[tokio::test]
async fn previous_session_format_is_rejected_without_rewriting_database_bytes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "previous-format").await;
    drop(store);
    let path = root.path().join("sessions.sqlite3");
    let connection = Connection::open(&path).unwrap();
    let header: String = connection
        .query_row(
            "SELECT header_json FROM sessions WHERE session_id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let mut header: serde_json::Value = serde_json::from_str(&header).unwrap();
    header["format_version"] = 16.into();
    connection
        .execute(
            "UPDATE sessions SET header_json=?1 WHERE session_id=?2",
            rusqlite::params![header.to_string(), id.as_str()],
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(
        matches!(store.header(&id).await, Err(StoreError::Corrupt(message)) if message.contains("unsupported session format version 16"))
    );
    drop(store);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn integrity_check_reports_the_actual_bounded_sqlite_failure() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE broken (value INTEGER CHECK(value > 0)); PRAGMA ignore_check_constraints = ON; INSERT INTO broken VALUES (-1); PRAGMA ignore_check_constraints = OFF;").unwrap();
    let error = validation::validate_database(&connection).unwrap_err();
    assert!(
        matches!(&error, StoreError::Corrupt(message) if message.contains("CHECK constraint failed in broken")),
        "{error}"
    );
}

#[path = "tests/settlement.rs"]
mod settlement;

#[path = "tests/selection_work.rs"]
mod selection_work;

#[path = "tests/ready_metadata.rs"]
mod ready_metadata;

#[tokio::test]
async fn bounded_reference_suffix_uses_the_1024_fact_limit_and_exact_horizon() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = seed_history(&store, "reference-suffix", 1100, 0).await;
    let page = store
        .read_fact_suffix(&session, 1024, 16 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(page.through_seq, 1100);
    assert_eq!(page.facts.len(), 1024);
    assert_eq!(page.after_seq(), 76);
    assert_eq!(page.facts.first().unwrap().seq(), 77);
    assert_eq!(page.facts.last().unwrap().seq(), 1100);
    assert!(!page.byte_limited);
    page.validate(1024, 16 * 1024 * 1024).unwrap();
    let forward = store
        .read_fact_window(&session, 0, 256, 16 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(forward.durable_seq, 1100);
    assert_eq!(forward.through_seq, 256);
    assert_eq!(forward.facts.len(), 256);
    assert!(forward.omitted.is_empty());
}

#[tokio::test]
async fn forward_window_skips_large_holes_but_stops_before_aggregate_overflow() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = SessionId::new("window-holes").unwrap();
    let mut facts = (1..=5).map(test_fact).collect::<Vec<_>>();
    let mut body = facts[1].body().clone();
    if let SessionFactBody::TurnAccepted { text, .. } = &mut body {
        *text = "x".repeat(8192);
    }
    facts[1] = SessionFact::new(2, 2, body).unwrap();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(test_header(id.as_str())),
            facts: facts.iter().cloned().map(Into::into).collect(),
        })
        .await
        .unwrap();
    let budget = facts[0].encoded_len() + facts[2].encoded_len();
    store
        .inner
        .fact_materializations
        .store(0, Ordering::Relaxed);
    let first = store.read_fact_window(&id, 0, 256, budget).await.unwrap();
    assert_eq!(first.through_seq, 3);
    assert_eq!(first.facts, vec![facts[0].clone(), facts[2].clone()]);
    assert_eq!(
        first.omitted.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(
        store.inner.fact_materializations.swap(0, Ordering::Relaxed),
        2
    );
    let next = store
        .read_fact_window(&id, first.through_seq, 256, budget)
        .await
        .unwrap();
    assert_eq!(next.through_seq, 5);
    assert_eq!(next.facts, facts[3..]);
    assert!(next.omitted.is_empty());
    assert_eq!(store.inner.fact_materializations.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn factory_retains_only_safe_startup_facts_for_its_owner() {
    let root = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(root.path()).unwrap());
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .pragma_update(None, "user_version", AGENT_STORE_SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let factory = SqliteStoreFactory::default();
    let runtime = rsi_meta::Runtime::default();
    let resolved = rsi_meta::ResolvedFactory::linked(
        "store",
        "test",
        rsi_meta::UpdateMode::RestartRequired,
        Arc::new(factory.clone()),
    );
    let handle = runtime
        .root()
        .apply(resolved.clone(), serde_json::json!({"root":root.path()}))
        .await
        .unwrap();
    assert!(matches!(
        handle.snapshot().state,
        rsi_meta::FiberState::Failed(_)
    ));
    let diagnostic = factory.take_startup_failure().unwrap();
    assert_eq!(diagnostic.root, root.path());
    assert_eq!(
        diagnostic.kind,
        SqliteStoreStartupFailureKind::SchemaMismatch {
            expected: AGENT_STORE_SCHEMA_VERSION,
            actual: AGENT_STORE_SCHEMA_VERSION - 1,
        }
    );
    assert!(factory.take_startup_failure().is_none());
    assert!(
        SqliteStoreFactory::default()
            .take_startup_failure()
            .is_none()
    );
    let valid = tempfile::tempdir().unwrap();
    let active = runtime
        .root()
        .apply(resolved, serde_json::json!({"root":valid.path()}))
        .await
        .unwrap();
    assert_eq!(active.snapshot().state, rsi_meta::FiberState::Active);
    assert!(factory.take_startup_failure().is_none());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[ignore = "report-only actual SQLite validation VM and full-scan work"]
async fn online_validation_sql_work() {
    use rusqlite::{
        StatementStatus,
        trace::{TraceEvent, TraceEventCodes},
    };
    static VM: AtomicU64 = AtomicU64::new(0);
    static SCANS: AtomicU64 = AtomicU64::new(0);
    fn count(event: TraceEvent<'_>) {
        if let TraceEvent::Profile(statement, _) = event {
            VM.fetch_add(
                u64::try_from(statement.get_status(StatementStatus::VmStep)).unwrap(),
                Ordering::Relaxed,
            );
            SCANS.fetch_add(
                u64::try_from(statement.get_status(StatementStatus::FullscanStep)).unwrap(),
                Ordering::Relaxed,
            );
        }
    }
    for (facts, controls) in [(1, 0), (1000, 0), (1000, 1000)] {
        for size in [256, 257, 512] {
            let root = tempfile::tempdir().unwrap();
            let store = SqliteStore::open(root.path()).unwrap();
            let mut sessions = Vec::new();
            for index in 0..size {
                sessions
                    .push(seed_history(&store, &format!("scan-{index}"), facts, controls).await);
            }
            drop(store);
            let store = SqliteStore::open(root.path()).unwrap();
            for connection in [
                &store.inner.connections.validation_reader,
                &store.inner.connections.reader,
            ] {
                connection
                    .lock()
                    .unwrap()
                    .trace_v2(TraceEventCodes::SQLITE_TRACE_PROFILE, Some(count));
            }
            for cycle in 0..3 {
                VM.store(0, Ordering::Relaxed);
                SCANS.store(0, Ordering::Relaxed);
                store.inner.validation_queue_ns.store(0, Ordering::Relaxed);
                store.inner.validation_work_ns.store(0, Ordering::Relaxed);
                let previous_runs = store.inner.validation_runs.load(Ordering::Relaxed);
                let start = std::time::Instant::now();
                for id in &sessions {
                    assert_eq!(store.read_facts(id, 0, 1).await.unwrap().facts.len(), 1);
                }
                eprintln!(
                    "sqlite sessions={size} facts={facts} controls={controls} cycle={cycle} fact_page_calls={size} validation_runs={} vm_steps={} full_scan_steps={} validation_queue_ns={} validation_work_ns={} elapsed={:?}",
                    store.inner.validation_runs.load(Ordering::Relaxed) - previous_runs,
                    VM.load(Ordering::Relaxed),
                    SCANS.load(Ordering::Relaxed),
                    store.inner.validation_queue_ns.load(Ordering::Relaxed),
                    store.inner.validation_work_ns.load(Ordering::Relaxed),
                    start.elapsed()
                );
            }
            VM.store(0, Ordering::Relaxed);
            SCANS.store(0, Ordering::Relaxed);
            let validated = store.inner.validation_runs.load(Ordering::Relaxed);
            for _ in 0..100 {
                store
                    .read_facts(sessions.last().unwrap(), 0, 1)
                    .await
                    .unwrap();
            }
            assert_eq!(
                store.inner.validation_runs.load(Ordering::Relaxed),
                validated
            );
            eprintln!(
                "sqlite sessions={size} warm_same_session_calls=100 vm_steps={} full_scan_steps={}",
                VM.load(Ordering::Relaxed),
                SCANS.load(Ordering::Relaxed)
            );
        }
    }
}

#[tokio::test]
async fn offline_validation_pages_every_session_and_rejects_late_corruption() {
    for count in [0, 256, 257, 513] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        for index in 0..count {
            let header = test_header(&format!("page-{index:04}"));
            store
                .append(AppendBatch {
                    session_id: header.session_id().clone(),
                    expected_seq: 0,
                    header: Some(header),
                    facts: vec![test_fact(1).into()],
                })
                .await
                .unwrap();
        }
        drop(store);
        let database = root.path().join("sessions.sqlite3");
        let before = std::fs::read(&database).unwrap();
        SqliteStore::verify(root.path()).unwrap();
        assert_eq!(std::fs::read(&database).unwrap(), before);
        if count > 256 {
            let db = Connection::open(&database).unwrap();
            db.execute(
                "UPDATE sessions SET header_json='{}' WHERE session_id=?1",
                [format!("page-{:04}", count - 1)],
            )
            .unwrap();
            drop(db);
            assert!(matches!(
                SqliteStore::verify(root.path()),
                Err(StoreError::Corrupt(_))
            ));
        }
    }
}

#[test]
fn offline_identity_pages_bound_retention_and_do_not_skip_invalid_rows() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE sessions (session_id TEXT PRIMARY KEY)")
        .unwrap();
    for index in 0..513 {
        db.execute(
            "INSERT INTO sessions VALUES (?1)",
            [format!("page-{index:04}")],
        )
        .unwrap();
    }
    let mut cursor = None;
    let mut sizes = Vec::new();
    loop {
        let page = crate::validation::session_id_page(&db, cursor.as_ref()).unwrap();
        if page.is_empty() {
            break;
        }
        sizes.push(page.len());
        cursor = page.last().cloned();
    }
    assert_eq!(sizes, [256, 256, 1]);
    for invalid in ["z".repeat(257), "z\0invalid".into(), "z invalid".into()] {
        db.execute("INSERT INTO sessions VALUES (?1)", [&invalid])
            .unwrap();
        assert!(matches!(
            crate::validation::session_id_page(&db, cursor.as_ref()),
            Err(StoreError::Corrupt(_))
        ));
        db.execute("DELETE FROM sessions WHERE session_id=?1", [&invalid])
            .unwrap();
    }
}

#[test]
fn offline_identity_pages_classify_non_text_and_invalid_utf8_as_corruption() {
    for sql in [
        "INSERT INTO sessions VALUES (X'616263')",
        "INSERT INTO sessions VALUES (CAST(X'ff' AS TEXT))",
        "INSERT INTO sessions VALUES (NULL)",
    ] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sessions (session_id TEXT PRIMARY KEY)")
            .unwrap();
        db.execute_batch(sql).unwrap();
        let result = crate::validation::session_id_page(&db, None);
        assert!(
            matches!(result, Err(StoreError::Corrupt(_))),
            "{sql}: {result:?}"
        );
    }
}
use crate::session_store::{
    LIST_AGENT_CHILDREN_AFTER_SQL, LIST_READY_MESSAGES_AFTER_SQL, LIST_READY_ROOTS_AFTER_SQL,
    LIST_WAITING_ACTIVATIONS_AFTER_SQL,
};
use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings};
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;

fn test_header(session_id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session_id).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn test_fact(sequence: u64) -> SessionFact {
    SessionFact::new(
        sequence,
        sequence,
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: TurnId::new(format!("turn-{sequence}")).unwrap(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn control_projection_replay_enforces_the_individual_control_bound() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("activation-control-bound");
    store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    let writer = store.inner.connections.writer.lock().unwrap();
    let record = AgentControlRecord::new(
        1,
        1,
        AgentControlRecordBody::MessageDiscarded {
            message_id: MessageId::new("discarded").unwrap(),
            reason: MessageDiscardReason::Cancelled,
        },
    )
    .unwrap();
    writer.execute("INSERT INTO agent_controls (session_id,seq,control_json) VALUES (?1,1,?2 || printf('%*s',?3,''))",
        params![header.session_id().as_str(), serde_json::to_string(&record).unwrap(), i64::try_from(MAXIMUM_SESSION_FACT_BYTES).unwrap()]).unwrap();
    let result = crate::validation::validate_agent_indexes(&writer, &header);
    assert!(
        matches!(result, Err(StoreError::Corrupt(message)) if message.contains("encoded bytes")),
        "activation projection admitted an individually oversized control"
    );
}

#[tokio::test]
async fn poisoned_validation_hint_cannot_fail_a_valid_store_commit() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let cache = store.inner.validated_sessions.clone();
    let _ = std::thread::spawn(move || {
        let _guard = cache.lock().unwrap();
        panic!("injected validation-cache poison");
    })
    .join();
    let header = test_header("poisoned-hint");
    let id = header.session_id().clone();
    let commit = store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .expect("optional cache failure overrode Store admission or commit");
    assert_eq!(commit.durable_seq, 1);
    store.validate_session(&id).await.unwrap();
    assert_eq!(store.read_facts(&id, 0, 1).await.unwrap().facts.len(), 1);
}

#[test]
fn prepared_store_charge_includes_inline_and_dynamic_config_state() {
    let config = SqliteStoreConfig {
        root: PathBuf::from("/tmp/rsi-agent-store"),
    };

    assert_eq!(
        store_config_retained_bytes(&config).unwrap(),
        std::mem::size_of::<SqliteStoreConfig>() + config.root.as_os_str().len()
    );
}

#[test]
fn validation_cache_ghosts_are_admission_hints_not_proofs() {
    let first = SessionId::new("session-000").unwrap();
    let mut cache = ValidatedSessionCache::default();
    cache.insert(first.clone());
    for index in 1..=VALIDATED_SESSION_CACHE_CAPACITY {
        cache.insert(SessionId::new(format!("session-{index:03}")).unwrap());
    }

    let ghosts = cache.ghost.clone();
    assert!(!cache.touch(&first));
    assert!(!cache.touch(&first));
    assert_eq!(
        cache.ghost, ghosts,
        "miss checks cannot publish or promote proofs"
    );
    assert!(cache.touch(&SessionId::new("session-001").unwrap()));
    assert_eq!(cache.len(), VALIDATED_SESSION_CACHE_CAPACITY);
    cache.insert(first.clone());
    assert_eq!(cache.reused.back(), Some(&first));
    assert!(!cache.ghost.contains(&first));
    for index in 0..1_024 {
        cache.insert(SessionId::new(format!("archive-{index}")).unwrap());
        assert!(
            cache.touch(&first),
            "archive scans must preserve the active reused proof"
        );
        assert_eq!(cache.len(), VALIDATED_SESSION_CACHE_CAPACITY);
        assert!(cache.ghost.len() <= VALIDATED_SESSION_CACHE_CAPACITY);
    }
}

#[test]
fn cache_insertion_returns_the_retained_proof_and_reuses_it_on_hits() {
    let id = SessionId::new("retained-proof").unwrap();
    let mut cache = ValidatedSessionCache::default();
    let first = cache.insert(id.clone());
    assert!(Arc::ptr_eq(&first, &cache.proofs[&id]));
    let second = cache.insert(id.clone());
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(cache.len(), 1);
}

#[test]
fn recent_session_cursor_seeks_both_columns_of_the_ordering_index() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let connection = store.inner.connections.reader.lock().unwrap();
    let detail = connection
        .query_row(
            "EXPLAIN QUERY PLAN
                 SELECT session_id, created_at_ms FROM sessions
                 WHERE (created_at_ms, session_id) < (?1, ?2)
                 ORDER BY created_at_ms DESC, session_id DESC LIMIT ?3",
            params![1_i64, "session", 8_i64],
            |row| row.get::<_, String>(3),
        )
        .unwrap();
    assert!(detail.contains("sessions_by_created_at"));
    assert!(detail.contains("created_at_ms,session_id"), "{detail}");
}

#[test]
fn agent_cursor_queries_seek_their_complete_ordering_keys() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let connection = store.inner.connections.reader.lock().unwrap();

    let ready_messages = connection
        .query_row(
            &format!("EXPLAIN QUERY PLAN {LIST_READY_MESSAGES_AFTER_SQL}"),
            params!["root", 1_i64, "session", 1_i64, 8_i64],
            |row| row.get::<_, String>(3),
        )
        .unwrap();
    assert!(
        ready_messages.contains("timestamp_ms,session_id,ready_control_seq)>(?,?,?)"),
        "{ready_messages}"
    );

    let children = connection
        .query_row(
            &format!("EXPLAIN QUERY PLAN {LIST_AGENT_CHILDREN_AFTER_SQL}"),
            params!["parent", "session", 8_i64],
            |row| row.get::<_, String>(3),
        )
        .unwrap();
    assert!(
        children.contains("parent_session_id=? AND session_id>?"),
        "{children}"
    );

    let waiting = connection
        .query_row(
            &format!("EXPLAIN QUERY PLAN {LIST_WAITING_ACTIVATIONS_AFTER_SQL}"),
            params!["session", 8_i64],
            |row| row.get::<_, String>(3),
        )
        .unwrap();
    assert!(waiting.contains("session_id>?"), "{waiting}");

    let ready_roots = connection
        .query_row(
            &format!("EXPLAIN QUERY PLAN {LIST_READY_ROOTS_AFTER_SQL}"),
            params!["root", 8_i64],
            |row| row.get::<_, String>(3),
        )
        .unwrap();
    assert!(ready_roots.contains("root_session_id>?"), "{ready_roots}");
}

#[tokio::test]
async fn concurrent_first_access_runs_one_session_validation() {
    let root = tempfile::tempdir().unwrap();
    let session_id = SessionId::new("session-single-flight").unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(test_header(session_id.as_str())),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    drop(store);

    let store = Arc::new(SqliteStore::open(root.path()).unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(17));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let session_id = session_id.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.validate_session(&session_id).await.unwrap();
            store.header(&session_id).await.unwrap()
        }));
    }
    barrier.wait().await;
    for task in tasks {
        assert_eq!(task.await.unwrap().session_id(), &session_id);
    }
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn repeated_recent_listing_does_not_validate_history() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("session-recent-cache").unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(test_header(session_id.as_str())),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    drop(store);

    let store = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        store
            .list_recent_sessions(None, 1)
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
    assert_eq!(
        store
            .list_recent_sessions(None, 1)
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn validated_session_eviction_causes_exactly_one_safe_revalidation() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let first = SessionId::new("session-000").unwrap();
    store
        .append(AppendBatch {
            session_id: first.clone(),
            expected_seq: 0,
            header: Some(test_header(first.as_str())),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    for index in 1..=VALIDATED_SESSION_CACHE_CAPACITY {
        let session_id = SessionId::new(format!("session-{index:03}")).unwrap();
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: Some(test_header(session_id.as_str())),
                facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
    }
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);

    store.validate_session(&first).await.unwrap();
    assert_eq!(store.header(&first).await.unwrap().session_id(), &first);
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
    store.validate_session(&first).await.unwrap();
    assert_eq!(store.header(&first).await.unwrap().session_id(), &first);
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn reader_observes_complete_snapshots_across_an_uncommitted_writer() {
    let root = tempfile::tempdir().unwrap();
    let session_id = SessionId::new("session-snapshot").unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(test_header(session_id.as_str())),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();

    let database = root.path().join("sessions.sqlite3");
    let writer_session = session_id.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut connection = Connection::open(database).unwrap();
        configure_writer(&connection).unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let batch = AppendBatch {
            session_id: writer_session,
            expected_seq: 1,
            header: None,
            facts: (vec![test_fact(2)]).into_iter().map(Into::into).collect(),
        };
        admit_append(&transaction, &batch).unwrap();
        insert_fact(&transaction, &batch.session_id, &batch.facts[0]).unwrap();
        advance_watermark(&transaction, &batch).unwrap();
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        transaction.commit().unwrap();
    });

    entered_rx.await.unwrap();
    let before = store.read_facts(&session_id, 0, 8).await.unwrap();
    assert_eq!(before.durable_seq, 1);
    assert_eq!(before.facts.len(), 1);
    release_tx.send(()).unwrap();
    tokio::task::spawn_blocking(move || writer.join().unwrap())
        .await
        .unwrap();
    let after = store.read_facts(&session_id, 0, 8).await.unwrap();
    assert_eq!(after.durable_seq, 2);
    assert_eq!(after.facts.len(), 2);

    let reader = store.inner.connections.reader.lock().unwrap();
    assert_eq!(
        reader
            .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        reader
            .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        5_000
    );
}

#[tokio::test]
async fn cancelled_blocking_jobs_retain_the_root_writer_lease() {
    for kind in ["reader", "validation", "writer", "cas"] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let owner = Arc::downgrade(&store.inner);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            let operation = move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            };
            match kind {
                "reader" => store.with_reader(move |_| operation()).await,
                "validation" => store.with_validation(move |_| operation()).await,
                "writer" => store.with_writer(move |_| operation()).await,
                _ => store.with_cas(operation).await,
            }
        });
        entered_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(owner.upgrade().is_some(), "{kind}");
        assert!(
            matches!(
                SqliteStore::open(root.path()),
                Err(StoreError::WriterLocked)
            ),
            "{kind}"
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while owner.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
            loop {
                match SqliteStore::open(root.path()) {
                    Ok(reopened) => {
                        drop(reopened);
                        break;
                    }
                    Err(StoreError::WriterLocked) => tokio::task::yield_now().await,
                    Err(error) => panic!("{kind}: {error}"),
                }
            }
        })
        .await
        .unwrap();
        SqliteStore::verify(root.path()).unwrap();
    }
}

#[test]
fn null_schema_definition_is_corruption() {
    let root = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(root.path()).unwrap());
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_master SET sql=NULL WHERE type='index' AND name='agent_messages_pending'").unwrap();
    drop(connection);
    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn missing_subtree_session_is_corruption() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("orphan-root");
    store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    store.inner.connections.writer.lock().unwrap().execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO agent_nodes VALUES ('missing-session', 'orphan-root', 'orphan-root', '[1]', 'child', '{}');
         PRAGMA foreign_keys=ON;"
    ).unwrap();
    assert!(matches!(
        store.read_agent_subtree_snapshot(header.session_id()).await,
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn schema_literals_are_compared_without_normalization() {
    for (kind, name, literal) in [
        ("table", "active_activations", "running"),
        ("index", "agent_messages_pending", "pending"),
    ] {
        for replacement in [
            literal.to_uppercase(),
            format!("{} {}", &literal[..1], &literal[1..]),
            format!("{literal} "),
            format!("{literal};"),
        ] {
            let root = tempfile::tempdir().unwrap();
            drop(SqliteStore::open(root.path()).unwrap());
            SqliteStore::verify(root.path()).unwrap();
            let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
            let observed: String = db
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type=?1 AND name=?2",
                    params![kind, name],
                    |row| row.get(0),
                )
                .unwrap();
            let mutated = observed.replace(&format!("'{literal}'"), &format!("'{replacement}'"));
            assert_ne!(observed, mutated);
            db.execute_batch("PRAGMA writable_schema=ON").unwrap();
            db.execute(
                "UPDATE sqlite_master SET sql=?1 WHERE type=?2 AND name=?3",
                params![mutated, kind, name],
            )
            .unwrap();
            drop(db);
            assert!(matches!(
                SqliteStore::open(root.path()),
                Err(StoreError::Corrupt(_))
            ));
            assert!(matches!(
                SqliteStore::verify(root.path()),
                Err(StoreError::Corrupt(_))
            ));
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One cold-open scenario proves every guarded projection and transaction rollback.
async fn cold_subtree_and_quiescence_reject_a_descendant_missing_its_indexes() {
    for table in [
        "turns",
        "agent_messages",
        "ready_messages",
        "active_activations",
    ] {
        let root = tempfile::tempdir().unwrap();
        let parent = test_header("cold-proof-parent");
        let child_id = SessionId::new("cold-proof-child").unwrap();
        let child = test_child_header(&parent, child_id.as_str());
        let store = SqliteStore::open(root.path()).unwrap();
        for header in [parent.clone(), child] {
            store
                .append(AppendBatch {
                    session_id: header.session_id().clone(),
                    expected_seq: 0,
                    header: Some(header),
                    facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
                })
                .await
                .unwrap();
        }
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: child_id.clone(),
                    expected_fact_seq: 1,
                    expected_control_seq: 0,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![
                        AgentControlRecord::new(
                            1,
                            1,
                            AgentControlRecordBody::MessageAccepted {
                                message: rsi_agent_session_protocol::AgentMessage {
                                    message_id: MessageId::new("pending-child").unwrap(),
                                    source: AgentMessageSource::Human,
                                    content: vec![
                                        rsi_agent_session_protocol::AgentMessageContent::Text {
                                            text: "pending".into(),
                                        },
                                    ],
                                    options: rsi_agent_session_protocol::MessageOptions::default(),
                                },
                                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                                bound_turn_id: None,
                                root_session_id: parent.session_id().clone(),
                                target: MessageTarget::NextTurn,
                                wake_required: true,
                            },
                        )
                        .unwrap(),
                        AgentControlRecord::new(
                            2,
                            2,
                            AgentControlRecordBody::ActivationStarted {
                                activation_id: ActivationId::new("child-active").unwrap(),
                                parent_session_id: Some(parent.session_id().clone()),
                                root_session_id: parent.session_id().clone(),
                                path: rsi_agent_session_protocol::AgentPath::new(vec![1]).unwrap(),
                            },
                        )
                        .unwrap(),
                    ],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await
            .unwrap();
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        for _ in 0..2 {
            let snapshot = store
                .read_agent_subtree_snapshot(parent.session_id())
                .await
                .unwrap();
            assert_eq!(snapshot.descendants.len(), 1);
            assert!(snapshot.descendants[0].status.has_open_turn);
        }
        assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 2);
        drop(store);
        let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        connection
            .execute(
                &format!("DELETE FROM {table} WHERE session_id=?1"),
                [child_id.as_str()],
            )
            .unwrap();
        drop(connection);
        let store = SqliteStore::open(root.path()).unwrap();
        assert!(store.header(&child_id).await.is_ok());
        assert!(matches!(
            store.read_agent_subtree_snapshot(parent.session_id()).await,
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store
                .commit_agent(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: parent.session_id().clone(),
                        expected_fact_seq: 1,
                        expected_control_seq: 0,
                        header: None,
                        facts: (vec![test_fact(2)]).into_iter().map(Into::into).collect(),
                        controls: Vec::new()
                    }],
                    required_active_activations: Vec::new(),
                    quiescent_descendants_of: Some((parent.session_id().clone()).into()),
                })
                .await,
            Err(StoreError::Corrupt(_))
        ));
        assert_eq!(
            store
                .read_facts(parent.session_id(), 0, 8)
                .await
                .unwrap()
                .durable_seq,
            1
        );
    }
}

#[tokio::test]
async fn subtree_snapshot_rejects_cycles_and_oversized_lineage_fields() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let parent = test_header("subtree-root");
    for header in [parent.clone(), test_child_header(&parent, "subtree-child")] {
        store
            .append(AppendBatch {
                session_id: header.session_id().clone(),
                expected_seq: 0,
                header: Some(header),
                facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
    }
    let id = SessionId::new("subtree-root").unwrap();
    assert_eq!(
        store
            .read_agent_subtree_snapshot(&id)
            .await
            .unwrap()
            .descendants
            .len(),
        1
    );
    for (field, value) in [
        ("task_name", "x".repeat(257)),
        ("task_name", "invalid name".into()),
        ("path_json", " ".repeat(4096)),
        ("execution_owner_json", " ".repeat(4097)),
    ] {
        let original: String = {
            let writer = store.inner.connections.writer.lock().unwrap();
            let original = writer
                .query_row(
                    &format!("SELECT {field} FROM agent_nodes WHERE session_id='subtree-child'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            writer
                .execute(
                    &format!("UPDATE agent_nodes SET {field}=?1 WHERE session_id='subtree-child'"),
                    [&value],
                )
                .unwrap();
            original
        };
        assert!(matches!(
            store.read_agent_subtree_snapshot(&id).await,
            Err(StoreError::Corrupt(_))
        ));
        store
            .inner
            .connections
            .writer
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE agent_nodes SET {field}=?1 WHERE session_id='subtree-child'"),
                [&original],
            )
            .unwrap();
    }
    store.inner.connections.writer.lock().unwrap().execute(
        "INSERT INTO agent_nodes VALUES ('subtree-root', 'subtree-root', 'subtree-child', '[2]', 'root', '{}')",
        [],
    ).unwrap();
    assert!(matches!(
        store.read_agent_subtree_snapshot(&id).await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one causal allocation probe compares all public fact-page boundaries"
)]
async fn fact_pages_admit_stored_lengths_before_materializing_the_next_body() {
    use rsi_agent_session_protocol::EffectId;
    use rsi_ai_protocol::{ContentDelta, LanguageEvent};
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = SessionId::new("length-admission").unwrap();
    let turn = TurnId::new("turn-1").unwrap();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(test_header(id.as_str())),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    for seq in 2..=4 {
        let fact = SessionFact::new(
            seq,
            seq,
            SessionFactBody::ModelEvent {
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                turn_id: turn.clone(),
                effect_id: EffectId::new("large-delta").unwrap(),
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("x".repeat(32 * 1024 * 1024)),
                },
            },
        )
        .unwrap();
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: seq - 1,
                header: None,
                facts: (vec![fact]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
    }
    let count = || store.inner.fact_materializations.swap(0, Ordering::Relaxed);
    let suffix = store
        .read_fact_suffix(&id, 1024, 16 * 1024 * 1024)
        .await
        .unwrap();
    assert!(suffix.facts.is_empty());
    assert!(suffix.byte_limited);
    assert_eq!(suffix.through_seq, 4);
    assert_eq!(suffix.encoded_bytes, 0);
    assert_eq!(
        count(),
        0,
        "an oversized first suffix body must never materialize"
    );
    let window = store
        .read_fact_window(&id, 0, 256, 16 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(window.through_seq, 4);
    assert_eq!(window.facts.len(), 1);
    assert_eq!(
        window.omitted.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert_eq!(
        count(),
        1,
        "length-only omissions never materialize any of the 32 MiB originals"
    );
    let window = store
        .read_fact_window(&id, 1, 256, 16 * 1024 * 1024)
        .await
        .unwrap();
    assert!(window.facts.is_empty());
    assert_eq!(window.through_seq, 4);
    assert_eq!(
        count(),
        0,
        "the first forward original obeys the caller's budget"
    );
    let page = store.read_facts(&id, 0, 8).await.unwrap();
    assert_eq!(page.facts.len(), 2);
    assert_eq!(count(), 2);
    drop(page);
    let page = store.read_facts(&id, 2, 8).await.unwrap();
    assert_eq!(page.facts[0].seq(), 3);
    assert_eq!(page.facts.len(), 1);
    assert_eq!(count(), 1);
    drop(page);
    let page = store.read_facts_before(&id, 0, 8).await.unwrap();
    assert_eq!(page.facts[0].seq(), 4);
    assert!(page.has_more);
    assert_eq!(count(), 1);
    drop(page);
    let page = store.read_turn_facts(&id, &turn, 1, 8).await.unwrap();
    assert_eq!(page.facts[0].seq(), 2);
    assert!(page.has_more);
    assert_eq!(count(), 1);
    drop(page);
    let page = store.read_turn_facts(&id, &turn, 2, 1).await.unwrap();
    assert_eq!(page.facts[0].seq(), 3);
    assert!(page.has_more);
    assert_eq!(count(), 1, "count lookahead must not materialize a body");
}

#[tokio::test]
async fn control_pages_admit_stored_lengths_before_materializing_the_next_body() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("control-length-admission");
    store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    {
        let writer = store.inner.connections.writer.lock().unwrap();
        // Valid JSON whitespace exercises the stored-byte bound independently of
        // the much smaller canonical record. No mailbox projection is consulted.
        for seq in 1..=2 {
            let record = AgentControlRecord::new(
                seq,
                seq,
                AgentControlRecordBody::MessageDiscarded {
                    message_id: MessageId::new(format!("discarded-{seq}")).unwrap(),
                    reason: MessageDiscardReason::Cancelled,
                },
            )
            .unwrap();
            let mut json = serde_json::to_string(&record).unwrap();
            json.extend(std::iter::repeat_n(' ', 32 * 1024 * 1024));
            writer
                .execute(
                    "INSERT INTO agent_controls (session_id,seq,control_json) VALUES (?1,?2,?3)",
                    params![
                        header.session_id().as_str(),
                        i64::try_from(seq).unwrap(),
                        json
                    ],
                )
                .unwrap();
        }
        writer
            .execute(
                "UPDATE sessions SET control_seq=2 WHERE session_id=?1",
                [header.session_id().as_str()],
            )
            .unwrap();
    }
    let page = store
        .read_controls(header.session_id(), 0, 8)
        .await
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].seq(), 1);
    let next = store
        .read_controls(header.session_id(), 1, 8)
        .await
        .unwrap();
    assert_eq!(next.records.len(), 1);
    assert_eq!(next.records[0].seq(), 2);
}

#[tokio::test]
async fn metadata_catalog_larger_than_validation_cache_never_validates_history() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    for index in 0..512 {
        let id = SessionId::new(format!("metadata-{index:03}")).unwrap();
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: 0,
                header: Some(test_header(id.as_str())),
                facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
    }
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    for _ in 0..2 {
        let mut cursor = None;
        loop {
            let page = store
                .list_recent_sessions(cursor.as_ref(), 256)
                .await
                .unwrap();
            for session in &page.sessions {
                store.header(session.header.session_id()).await.unwrap();
            }
            if !page.has_more {
                break;
            }
            cursor = page.sessions.last().map(StoreRecentSession::cursor);
        }
    }
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
    assert_eq!(store.inner.validated_sessions.lock().unwrap().len(), 0);
}

fn pause_next_validation(
    store: &SqliteStore,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *store.inner.validation_barrier.lock().unwrap() = Some((entered_tx, release_rx));
    (entered_rx, release_tx)
}

async fn seed_session(store: &SqliteStore, name: &str) -> SessionId {
    let id = SessionId::new(name).unwrap();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(test_header(name)),
            facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
        })
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn paused_cold_validation_does_not_block_foreground_reads_or_writes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let cold = seed_session(&store, "cold").await;
    let warm = seed_session(&store, "warm").await;
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    store.read_facts(&warm, 0, 1).await.unwrap();
    let (entered, release) = pause_next_validation(&store);
    let worker = store.clone();
    let validation = tokio::spawn(async move { worker.read_facts(&cold, 0, 1).await });
    entered.await.unwrap();
    let foreground = tokio::time::timeout(Duration::from_secs(2), async {
        store.header(&warm).await.unwrap();
        store.read_facts(&warm, 0, 1).await.unwrap();
        store
            .append(AppendBatch {
                session_id: warm.clone(),
                expected_seq: 1,
                header: None,
                facts: (vec![test_fact(2)]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
        store.inspect_session(&warm).await.unwrap();
        store.read_agent_subtree_snapshot(&warm).await.unwrap();
    })
    .await;
    release.send(()).unwrap();
    validation.await.unwrap().unwrap();
    foreground.expect("cold validation held a foreground connection");
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn cancelled_admitted_validation_publishes_proof_before_next_waiter() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "cancelled-validator").await;
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    let (entered, release) = pause_next_validation(&store);
    let worker = store.clone();
    let candidate = id.clone();
    let first = tokio::spawn(async move { worker.validate_session(&candidate).await });
    entered.await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let worker = store.clone();
    let candidate = id.clone();
    let second = tokio::spawn(async move { worker.read_facts(&candidate, 0, 1).await });
    release.send(()).unwrap();
    second.await.unwrap().unwrap();
    assert!(store.touch_validated_session(&id));
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn cancelled_queued_validation_never_dispatches_a_worker() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "queued-validator").await;
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    let permit = store
        .inner
        .validation_admission
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let worker = store.clone();
    let first = tokio::spawn(async move { worker.validate_session(&id).await });
    tokio::task::yield_now().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    drop(permit);
    let _drained = store
        .inner
        .validation_admission
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn operational_reads_reuse_proofs_under_bounded_scan_pressure() {
    for (count, expected) in [
        (256, [256, 0, 0, 0, 0]),
        (257, [257, 193, 4, 4, 4]),
        (512, [512, 448, 321, 448, 259]),
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let mut sessions = Vec::new();
        for index in 0..count {
            sessions.push(seed_session(&store, &format!("operational-{index}")).await);
        }
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        for misses in expected {
            let previous = store.inner.validation_runs.load(Ordering::Relaxed);
            for id in &sessions {
                let page = store.read_facts(id, 0, 1).await.unwrap();
                assert_eq!(page.facts.len(), 1);
            }
            assert_eq!(
                store.inner.validation_runs.load(Ordering::Relaxed),
                previous + misses
            );
            assert_eq!(store.inner.validated_sessions.lock().unwrap().len(), 256);
        }
    }
}

#[tokio::test]
async fn cold_control_replay_decodes_each_record_once() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "single-decode").await;
    let mut controls = Vec::new();
    for seq in (1..=500).step_by(2) {
        let message_id = MessageId::new(format!("message-{seq}")).unwrap();
        controls.push(
            AgentControlRecord::new(
                seq,
                seq,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: message_id.clone(),
                        source: AgentMessageSource::Human,
                        content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                            text: "bounded".into(),
                        }],
                        options: rsi_agent_session_protocol::MessageOptions::default(),
                    },
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    root_session_id: id.clone(),
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
            .unwrap(),
        );
        controls.push(
            AgentControlRecord::new(
                seq + 1,
                seq + 1,
                AgentControlRecordBody::MessageDiscarded {
                    message_id,
                    reason: MessageDiscardReason::Cancelled,
                },
            )
            .unwrap(),
        );
    }
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                expected_fact_seq: 1,
                expected_control_seq: 0,
                header: None,
                facts: vec![],
                controls,
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    store.read_facts(&id, 0, 1).await.unwrap();
    store.read_agent_mailbox(&id, None).await.unwrap();
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 1);
    assert_eq!(store.inner.control_decodes.load(Ordering::Relaxed), 500);
    assert_one_header_read_for_control_validation(&store, &id).await;
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
}

async fn assert_one_header_read_for_control_validation(store: &SqliteStore, id: &SessionId) {
    let id = id.clone();
    let reads = store
        .with_validation(move |connection| {
            crate::validation::HEADER_READS.set(0);
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            validate_session(&transaction, &id)?;
            Ok(crate::validation::HEADER_READS.get())
        })
        .await
        .unwrap();
    assert_eq!(
        reads, 1,
        "immutable Header must be shared across all control projections"
    );
}

#[tokio::test]
async fn cold_activation_replay_reads_the_immutable_header_once() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "activation-header-reads").await;
    let controls = (1..=500)
        .step_by(2)
        .flat_map(|seq| {
            let activation_id = ActivationId::new(format!("activation-{seq}")).unwrap();
            [
                AgentControlRecord::new(
                    seq,
                    seq,
                    AgentControlRecordBody::ActivationStarted {
                        activation_id: activation_id.clone(),
                        parent_session_id: None,
                        root_session_id: id.clone(),
                        path: rsi_agent_session_protocol::AgentPath::root(),
                    },
                )
                .unwrap(),
                AgentControlRecord::new(
                    seq + 1,
                    seq + 1,
                    AgentControlRecordBody::ActivationSettled {
                        activation_id,
                        outcome: rsi_agent_session_protocol::ActivationOutcome::Completed {
                            result: None,
                        },
                    },
                )
                .unwrap(),
            ]
        })
        .collect();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                expected_fact_seq: 1,
                expected_control_seq: 0,
                header: None,
                facts: Vec::new(),
                controls,
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    assert_one_header_read_for_control_validation(&store, &id).await;
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
}

fn test_child_header(parent: &SessionHeader, id: &str) -> SessionHeader {
    parent
        .forked_child(
            SessionId::new(id).unwrap(),
            2,
            rsi_agent_session_protocol::ForkOrigin {
                parent_session_id: parent.session_id().clone(),
                root_session_id: parent.session_id().clone(),
                path: rsi_agent_session_protocol::AgentPath::new(vec![1]).unwrap(),
                task_name: "child".into(),
                parent_header_fingerprint: parent.fingerprint().unwrap(),
                invoking_turn_id: TurnId::new("turn-1").unwrap(),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: "0".repeat(64),
                resolved_terminal_control_seq: 0,
                terminal_control_prefix_sha256: "0".repeat(64),
                requested_turns: rsi_agent_session_protocol::ForkTurnSelection::None,
                effective_turns: 0,
            },
            rsi_agent_session_protocol::ModelSelection::baseline(parent.settings()),
        )
        .unwrap()
}

#[tokio::test]
async fn cold_subtree_inspection_and_quiescence_use_the_validation_lane() {
    for operation in ["subtree", "inspection", "quiescence"] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let parent = test_header("tree-root");
        seed_session(&store, parent.session_id().as_str()).await;
        let other = seed_session(&store, "unrelated-warm").await;
        let child = test_child_header(&parent, "cold-child");
        store
            .append(AppendBatch {
                session_id: child.session_id().clone(),
                expected_seq: 0,
                header: Some(child),
                facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        store.validate_session(parent.session_id()).await.unwrap();
        store.validate_session(&other).await.unwrap();
        let (entered, release) = pause_next_validation(&store);
        let worker = store.clone();
        let candidate = parent.session_id().clone();
        let task = tokio::spawn(async move {
            match operation {
                "subtree" => worker
                    .read_agent_subtree_snapshot(&candidate)
                    .await
                    .map(|_| ()),
                "inspection" => worker.inspect_session(&candidate).await.map(|_| ()),
                _ => worker
                    .commit_agent(AtomicAgentCommit {
                        sessions: vec![AtomicSessionAppend {
                            session_id: candidate.clone(),
                            expected_fact_seq: 1,
                            expected_control_seq: 0,
                            header: None,
                            facts: (vec![test_fact(2)]).into_iter().map(Into::into).collect(),
                            controls: vec![],
                        }],
                        required_active_activations: vec![],
                        quiescent_descendants_of: Some((candidate).into()),
                    })
                    .await
                    .map(|_| ()),
            }
        });
        entered.await.unwrap();
        let progress = tokio::time::timeout(Duration::from_secs(2), async {
            store.header(&other).await.unwrap();
            store.read_facts(&other, 0, 1).await.unwrap();
            store
                .append(AppendBatch {
                    session_id: other,
                    expected_seq: 1,
                    header: None,
                    facts: (vec![test_fact(2)]).into_iter().map(Into::into).collect(),
                })
                .await
                .unwrap();
        })
        .await;
        release.send(()).unwrap();
        let result = task.await.unwrap();
        progress.unwrap_or_else(|_| {
            panic!("{operation} held foreground or writer during cold validation")
        });
        if operation == "quiescence" {
            assert!(matches!(
                result,
                Err(StoreError::SessionNotQuiescent { .. })
            ));
            assert_eq!(
                store
                    .read_facts(parent.session_id(), 0, 1)
                    .await
                    .unwrap()
                    .durable_seq,
                1
            );
        } else {
            result.unwrap();
        }
        assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 3);
    }
}

#[tokio::test]
async fn quiescence_checks_children_created_by_the_same_commit_without_caching_rollback() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let parent = test_header("new-child-guard");
    seed_session(&store, parent.session_id().as_str()).await;
    let child = test_child_header(&parent, "uncommitted-child");
    let child_id = child.session_id().clone();
    let result = store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![
                AtomicSessionAppend {
                    session_id: parent.session_id().clone(),
                    expected_fact_seq: 1,
                    expected_control_seq: 0,
                    header: None,
                    facts: (vec![test_fact(2)]).into_iter().map(Into::into).collect(),
                    controls: vec![],
                },
                AtomicSessionAppend {
                    session_id: child_id.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(child),
                    facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
                    controls: vec![],
                },
            ],
            required_active_activations: vec![],
            quiescent_descendants_of: Some((parent.session_id().clone()).into()),
        })
        .await;
    assert!(
        matches!(result, Err(StoreError::SessionNotQuiescent { session }) if session == child_id.as_str())
    );
    assert!(matches!(
        store.header(&child_id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(!store.touch_validated_session(&child_id));
    assert_eq!(
        store
            .read_facts(parent.session_id(), 0, 1)
            .await
            .unwrap()
            .durable_seq,
        1
    );
}

async fn seed_history(
    store: &SqliteStore,
    name: &str,
    fact_count: u64,
    controls: u64,
) -> SessionId {
    let id = seed_session(store, name).await;
    let mut fact_seq = 1;
    while fact_seq < fact_count {
        let end = (fact_seq + 500).min(fact_count);
        let facts = (fact_seq + 1..=end)
            .map(|seq| {
                SessionFact::new(
                    seq,
                    seq,
                    SessionFactBody::ModelEvent {
                        purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                        turn_id: TurnId::new("turn-1").unwrap(),
                        effect_id: rsi_agent_session_protocol::EffectId::new("model").unwrap(),
                        event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                            index: 0,
                            delta: rsi_ai_protocol::ContentDelta::Text("delta".into()),
                        },
                    },
                )
                .unwrap()
                .into()
            })
            .collect();
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: fact_seq,
                header: None,
                facts,
            })
            .await
            .unwrap();
        fact_seq = end;
    }
    for previous in (0..controls).step_by(500) {
        let mut append = settlement::append(&id, previous);
        append.expected_fact_seq = fact_count;
        append.controls = (previous..previous + 500)
            .step_by(2)
            .flat_map(|seq| settlement::append(&id, seq).controls)
            .collect();
        store
            .commit_agent(settlement::commit(vec![append]))
            .await
            .unwrap();
    }
    id
}

#[tokio::test]
async fn mixed_archive_scans_preserve_hot_validation_proofs_and_allow_writes() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut hot = Vec::new();
    let mut archive = Vec::new();
    for index in 0..32 {
        hot.push(seed_session(&store, &format!("hot-{index}")).await);
    }
    for index in 0..512 {
        archive.push(seed_session(&store, &format!("archive-{index}")).await);
    }
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    for id in &hot {
        store.read_facts(id, 0, 1).await.unwrap();
    }
    let mut fact_seq = 1;
    for cycle in 0..3 {
        let mut hot_misses = 0;
        for chunk in archive.chunks(16) {
            for id in chunk {
                store.read_facts(id, 0, 1).await.unwrap();
            }
            let before = store.inner.validation_runs.load(Ordering::Relaxed);
            for id in &hot {
                store.read_facts(id, 0, 1).await.unwrap();
            }
            hot_misses += store.inner.validation_runs.load(Ordering::Relaxed) - before;
            store
                .append(AppendBatch {
                    session_id: hot[0].clone(),
                    expected_seq: fact_seq,
                    header: None,
                    facts: vec![test_fact(fact_seq + 1).into()],
                })
                .await
                .unwrap();
            fact_seq += 1;
        }
        // Each hot identity validates once more when probation eviction promotes its ghost.
        assert_eq!(hot_misses, if cycle == 0 { 32 } else { 0 }, "cycle {cycle}");
        assert!(store.inner.validated_sessions.lock().unwrap().len() <= 256);
    }
}

#[tokio::test]
async fn prepared_proofs_survive_churn_and_share_saturated_pin_capacity() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut ids = Vec::new();
    for index in 0..257 {
        ids.push(seed_session(&store, &format!("pinned-{index}")).await);
    }
    let mut leases = Vec::new();
    for id in &ids[..256] {
        leases.push(store.prepare_session(id).await.unwrap());
    }
    assert_eq!(store.inner.pin_admission.available_permits(), 0);
    let duplicate = store.prepare_session(&ids[0]).await.unwrap();
    assert_eq!(store.inner.pin_admission.available_permits(), 0);
    drop(duplicate);
    let before = store.inner.validation_runs.load(Ordering::Relaxed);
    for index in 0..512 {
        seed_session(&store, &format!("churn-{index}")).await;
    }
    store.read_facts(&ids[0], 0, 1).await.unwrap();
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), before);
    store.validate_session(&ids[256]).await.unwrap();
    let (first, second) = {
        let mut first = store.prepare_session(&ids[256]);
        let mut second = store.prepare_session(&ids[256]);
        std::future::poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            assert!(second.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        // Trusted writes may advance this exact Session while its proof waits for capacity.
        store
            .append(AppendBatch {
                session_id: ids[256].clone(),
                expected_seq: 1,
                header: None,
                facts: vec![test_fact(2).into()],
            })
            .await
            .unwrap();
        drop(leases.pop());
        let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        (first.unwrap(), second.unwrap())
    };
    assert_eq!(store.inner.pin_admission.available_permits(), 0);
    let page = store
        .read_fact_suffix(&ids[256], 1, MAXIMUM_SESSION_FACT_BYTES)
        .await
        .unwrap();
    assert_eq!(page.through_seq, 2);
    assert_eq!(page.facts[0].seq(), 2);
    drop(first);
    assert_eq!(store.inner.pin_admission.available_permits(), 0);
    drop(second);
    assert_eq!(store.inner.pin_admission.available_permits(), 1);
    drop(leases);
    // Inserting a new pin prunes every dead Weak, including unrelated slots.
    let lease = store.prepare_session(&ids[0]).await.unwrap();
    assert_eq!(store.inner.pins.lock().unwrap().len(), 1);
    drop(store);
    assert!(SqliteStore::open(root.path()).is_err());
    drop(lease);
    drop(SqliteStore::open(root.path()).unwrap());
}

#[tokio::test]
async fn watermarks_are_one_cold_metadata_read_without_payload_replay() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_history(&store, "scalar", 10, 500).await;
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    let value = store.read_watermarks(&id).await.unwrap();
    assert_eq!(
        (value.durable_fact_seq, value.durable_control_seq),
        (10, 500)
    );
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
    assert_eq!(store.inner.control_decodes.load(Ordering::Relaxed), 0);
    assert_eq!(store.inner.fact_materializations.load(Ordering::Relaxed), 0);
    assert!(matches!(
        store
            .read_watermarks(&SessionId::new("absent").unwrap())
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn prepared_reads_examine_one_pin_regardless_of_unrelated_leases() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut leases = Vec::new();
    for index in 0..VALIDATED_SESSION_CACHE_CAPACITY {
        let id = seed_session(&store, &format!("pin-work-{index}")).await;
        leases.push(store.prepare_session(&id).await.unwrap());
    }
    let id = SessionId::new("pin-work-0").unwrap();
    store.inner.pin_entries_examined.store(0, Ordering::Relaxed);
    for _ in 0..16 {
        assert_eq!(store.read_facts(&id, 0, 1).await.unwrap().facts.len(), 1);
    }
    assert_eq!(store.inner.pin_entries_examined.load(Ordering::Relaxed), 16);
}

#[tokio::test]
async fn prepared_proof_survives_its_own_validated_tail_advances() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "advancing-proof").await;
    let lease = store.prepare_session(&id).await.unwrap();
    let before = store.inner.validation_runs.load(Ordering::Relaxed);
    for seq in 2..=5 {
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: seq - 1,
                header: None,
                facts: vec![test_fact(seq).into()],
            })
            .await
            .unwrap();
        let page = store
            .read_fact_suffix(&id, 1, MAXIMUM_SESSION_FACT_BYTES)
            .await
            .unwrap();
        assert_eq!(page.through_seq, seq);
        assert_eq!(page.facts[0].seq(), seq);
        assert_eq!(
            store.read_watermarks(&id).await.unwrap().durable_fact_seq,
            seq
        );
    }
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), before);
    drop(lease);
}

#[tokio::test]
async fn scalar_watermarks_never_authorize_a_corrupt_session_commit() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "unvalidated-cursors").await;
    drop(store);
    {
        let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        db.execute(
            "UPDATE sessions SET header_json='{}' WHERE session_id=?1",
            [id.as_str()],
        )
        .unwrap();
    }
    let store = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        store.read_watermarks(&id).await.unwrap().durable_fact_seq,
        1
    );
    let mut append = settlement::append(&id, 0);
    append.expected_fact_seq = 1;
    assert!(matches!(
        store.commit_agent(settlement::commit(vec![append])).await,
        Err(StoreError::Corrupt(_))
    ));
    let watermark = store.read_watermarks(&id).await.unwrap();
    assert_eq!(
        (watermark.durable_fact_seq, watermark.durable_control_seq),
        (1, 0)
    );
}

#[tokio::test]
async fn unrelated_pin_notifications_preserve_distinct_session_waiter_order() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let mut ids = vec![];
    for index in 0..258 {
        ids.push(seed_session(&store, &format!("fair-pin-{index}")).await);
    }
    let mut leases = vec![];
    for id in &ids[..256] {
        leases.push(store.prepare_session(id).await.unwrap());
    }
    let mut first = store.prepare_session(&ids[256]);
    let mut second = store.prepare_session(&ids[257]);
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    store.inner.pin_changed.notify_waiters();
    // A notification about a different Session must not put the first waiter last.
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(leases.pop());
    let first_result =
        std::future::poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx))).await;
    assert!(
        first_result.is_ready(),
        "the earlier distinct-session waiter lost its FIFO position"
    );
    drop(first_result);
    drop(second);
    drop(leases);
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[ignore = "opt-in warm reader contention measurement; no timing pass threshold"]
async fn measure_warm_fact_pages_and_small_metadata() {
    warm_reader_case(64, 48 * 1024, 30, true).await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn warm_reader_measurements_cover_workers_without_revalidation() {
    warm_reader_case(4, 1024, 2, false).await;
}

#[cfg(feature = "test-support")]
async fn warm_reader_case(fact_count: usize, text_bytes: usize, iterations: usize, report: bool) {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "warm-mixed-reader").await;
    let facts = (2..=u64::try_from(fact_count).unwrap() + 1)
        .map(|seq| {
            SessionFact::new(
                seq,
                seq,
                SessionFactBody::ModelEvent {
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                    turn_id: TurnId::new("turn-1").unwrap(),
                    effect_id: rsi_agent_session_protocol::EffectId::new("model").unwrap(),
                    event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                        index: 0,
                        delta: rsi_ai_protocol::ContentDelta::Text("x".repeat(text_bytes)),
                    },
                },
            )
            .unwrap()
            .into()
        })
        .collect();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 1,
            header: None,
            facts,
        })
        .await
        .unwrap();
    store.prepare_session(&id).await.unwrap();
    let warm = store.read_facts(&id, 1, fact_count).await.unwrap();
    assert_eq!(warm.facts.len(), fact_count);
    let page_bytes: usize = warm.facts.iter().map(SessionFact::encoded_len).sum();
    drop(warm);
    let before = store.validation_counts().0;
    for mixed in [false, true] {
        store.begin_reader_measurements();
        for _ in 0..iterations {
            if mixed {
                let (page, header) =
                    tokio::join!(store.read_facts(&id, 1, fact_count), store.header(&id));
                assert_eq!(page.unwrap().facts.len(), fact_count);
                header.unwrap();
            } else {
                store.header(&id).await.unwrap();
            }
        }
        let samples = store.take_reader_measurements();
        assert_eq!(samples.len(), iterations * if mixed { 2 } else { 1 });
        assert!(
            samples
                .iter()
                .all(|sample| sample.json_decode_ns <= sample.worker_ns)
        );
        if report {
            eprintln!(
                "warm_reader {}",
                serde_json::json!({"mixed":mixed,"fact_page_bytes":page_bytes,"samples":samples})
            );
        }
    }
    assert_eq!(
        store.validation_counts().0,
        before,
        "warm measurement must never revalidate the session"
    );
}

#[path = "tests/program.rs"]
mod program;

#[tokio::test]
async fn warm_subtree_reads_no_full_headers_and_cold_owner_corruption_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let parent = test_header("owner-projection-root");
    let child = test_child_header(&parent, "owner-projection-child");
    for header in [parent.clone(), child.clone()] {
        store
            .append(AppendBatch {
                session_id: header.session_id().clone(),
                expected_seq: 0,
                header: Some(header),
                facts: vec![test_fact(1).into()],
            })
            .await
            .unwrap();
    }
    let id = parent.session_id().clone();
    store.read_agent_subtree_snapshot(&id).await.unwrap();
    let owner = store.inner.clone();
    let reads = store
        .with_validation(move |connection| {
            validation::HEADER_READS.set(0);
            let tree = owner.read_validated_agent_subtree(connection, &id)?;
            assert_eq!(
                tree.descendants[0].execution_owner,
                child.execution_owner().unwrap().clone()
            );
            Ok(validation::HEADER_READS.get())
        })
        .await
        .unwrap();
    assert_eq!(
        reads, 0,
        "warm subtree must decode only bounded owner projections"
    );
    drop(store);
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection.execute("UPDATE agent_nodes SET execution_owner_json=json_set(execution_owner_json,'$.turn_id','forged')", []).unwrap();
    drop(connection);
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(matches!(
        store.read_agent_subtree_snapshot(parent.session_id()).await,
        Err(StoreError::Corrupt(_))
    ));
    drop(store);
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(_))
    ));
}
