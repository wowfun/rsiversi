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
         INSERT INTO agent_nodes VALUES ('missing-session', 'orphan-root', 'orphan-root', '[1]', 'child');
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
                    quiescent_descendants_of: Some(parent.session_id().clone()),
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
    for id in ["subtree-root", "subtree-child"] {
        store
            .append(AppendBatch {
                session_id: SessionId::new(id).unwrap(),
                expected_seq: 0,
                header: Some(test_header(id)),
                facts: (vec![test_fact(1)]).into_iter().map(Into::into).collect(),
            })
            .await
            .unwrap();
    }
    let id = SessionId::new("subtree-root").unwrap();
    // Fault injection deliberately bypasses the Header-derived lineage writer.
    {
        let writer = store.inner.connections.writer.lock().unwrap();
        writer.execute(
            "INSERT INTO agent_nodes VALUES ('subtree-child', 'subtree-root', 'subtree-root', '[1]', 'child')",
            [],
        ).unwrap();
    }
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
        "INSERT INTO agent_nodes VALUES ('subtree-root', 'subtree-root', 'subtree-child', '[2]', 'root')",
        [],
    ).unwrap();
    assert!(matches!(
        store.read_agent_subtree_snapshot(&id).await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
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
    assert!(
        store
            .inner
            .validated_sessions
            .lock()
            .unwrap()
            .recency
            .is_empty()
    );
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
async fn operational_reads_over_257_and_512_sessions_revalidate_evicted_proofs() {
    for count in [257, 512] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let mut sessions = Vec::new();
        for index in 0..count {
            sessions.push(seed_session(&store, &format!("operational-{index}")).await);
        }
        drop(store);
        let store = SqliteStore::open(root.path()).unwrap();
        for cycle in 1..=2 {
            for id in &sessions {
                let page = store.read_facts(id, 0, 1).await.unwrap();
                assert_eq!(page.facts.len(), 1);
            }
            assert_eq!(
                store.inner.validation_runs.load(Ordering::Relaxed),
                count * cycle
            );
            assert_eq!(
                store.inner.validated_sessions.lock().unwrap().recency.len(),
                256
            );
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
                        outcome: rsi_agent_session_protocol::ActivationOutcome::Completed,
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
                        quiescent_descendants_of: Some(candidate),
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
            quiescent_descendants_of: Some(parent.session_id().clone()),
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
