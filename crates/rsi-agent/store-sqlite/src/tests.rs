use super::*;
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
            turn_id: TurnId::new(format!("turn-{sequence}")).unwrap(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
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
fn validated_session_cache_has_exact_recency_eviction() {
    let first = SessionId::new("session-000").unwrap();
    let mut cache = ValidatedSessionCache::default();
    cache.insert(first.clone());
    for index in 1..=VALIDATED_SESSION_CACHE_CAPACITY {
        cache.insert(SessionId::new(format!("session-{index:03}")).unwrap());
    }

    assert!(!cache.touch(&first));
    assert!(cache.touch(&SessionId::new("session-001").unwrap()));
    assert_eq!(cache.recency.len(), VALIDATED_SESSION_CACHE_CAPACITY);
}

#[test]
fn recent_session_cursor_seeks_both_columns_of_the_ordering_index() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let connection = store.connections.reader.lock().unwrap();
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
    let connection = store.connections.reader.lock().unwrap();

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
            facts: vec![test_fact(1)],
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
            store.header(&session_id).await.unwrap()
        }));
    }
    barrier.wait().await;
    for task in tasks {
        assert_eq!(task.await.unwrap().session_id(), &session_id);
    }
    assert_eq!(store.validation_runs.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn repeated_recent_listing_reuses_the_session_validation_cache() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("session-recent-cache").unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(test_header(session_id.as_str())),
            facts: vec![test_fact(1)],
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
    assert_eq!(store.validation_runs.load(Ordering::Relaxed), 1);
    assert_eq!(
        store
            .list_recent_sessions(None, 1)
            .await
            .unwrap()
            .sessions
            .len(),
        1
    );
    assert_eq!(store.validation_runs.load(Ordering::Relaxed), 1);
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
            facts: vec![test_fact(1)],
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
                facts: vec![test_fact(1)],
            })
            .await
            .unwrap();
    }
    assert_eq!(store.validation_runs.load(Ordering::Relaxed), 0);

    assert_eq!(store.header(&first).await.unwrap().session_id(), &first);
    assert_eq!(store.validation_runs.load(Ordering::Relaxed), 1);
    assert_eq!(store.header(&first).await.unwrap().session_id(), &first);
    assert_eq!(store.validation_runs.load(Ordering::Relaxed), 1);
}

#[test]
fn validation_gates_are_shared_only_within_one_session() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let first = SessionId::new("session-first").unwrap();
    let second = SessionId::new("session-second").unwrap();

    let first_gate = store.validation_gate(&first).unwrap();
    let same_session_gate = store.validation_gate(&first).unwrap();
    let other_session_gate = store.validation_gate(&second).unwrap();

    assert!(Arc::ptr_eq(&first_gate, &same_session_gate));
    assert!(!Arc::ptr_eq(&first_gate, &other_session_gate));
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
            facts: vec![test_fact(1)],
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
            facts: vec![test_fact(2)],
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

    let reader = store.connections.reader.lock().unwrap();
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
