use super::*;
use rsi_agent_store_protocol::MailboxRead;

async fn ready_fixture(store: &SqliteStore) -> SessionId {
    let id = seed_session(store, "ready-metadata").await;
    let controls = (1..=3)
        .map(|seq| {
            AgentControlRecord::new(
                seq,
                seq,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: MessageId::new(format!("message-{seq}")).unwrap(),
                        source: AgentMessageSource::Human { attribution: None },
                        content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                            text: "ordinary".into(),
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
            .unwrap()
        })
        .collect();
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
    id
}

#[tokio::test]
async fn schema_44_discards_only_the_message_copy_and_reads_canonical_bodies_after_reopen() {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = ready_fixture(&store).await;
    let before = store
        .read_agent_mailbox(&id, MailboxRead::All)
        .await
        .unwrap();
    drop(store);
    let mut db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let controls = |db: &Connection| -> Vec<String> {
        db.prepare("SELECT control_json FROM agent_controls ORDER BY session_id, seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    let canonical = controls(&db);
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('agent_messages')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let old_sql = EXPECTED_TABLES
        .iter()
        .find(|(name, _)| *name == "agent_messages")
        .unwrap()
        .1
        .replace(
            "message_source TEXT NOT NULL,",
            "message_source TEXT NOT NULL,\n            message_json TEXT NOT NULL,",
        );
    let transaction = db.transaction().unwrap();
    transaction
        .execute_batch("ALTER TABLE agent_messages RENAME TO current_messages")
        .unwrap();
    transaction.execute_batch(&old_sql).unwrap();
    transaction.execute_batch(&format!(
        "INSERT INTO agent_messages ({},message_json) SELECT {},json_extract(c.control_json,'$.message') \
         FROM current_messages AS m JOIN agent_controls AS c ON c.session_id=m.session_id AND c.seq=m.accepted_control_seq; \
         DROP TABLE current_messages",
        columns.join(","), columns.iter().map(|name| format!("m.{name}")).collect::<Vec<_>>().join(",")
    )).unwrap();
    for (_, sql) in EXPECTED_INDEXES {
        if sql.contains("ON agent_messages") {
            transaction.execute_batch(sql).unwrap();
        }
    }
    transaction.pragma_update(None, "user_version", 44).unwrap();
    transaction.commit().unwrap();
    drop(db);
    let store = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        store
            .read_agent_mailbox(&id, MailboxRead::All)
            .await
            .unwrap(),
        before
    );
    for row in &before.pending {
        assert_eq!(
            store
                .read_agent_message(&id, &row.message.message_id)
                .await
                .unwrap(),
            Some(row.clone())
        );
    }
    let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    assert_eq!(controls(&db), canonical);
    assert_eq!(
        pragma_user_version(&db).unwrap(),
        AGENT_STORE_SCHEMA_VERSION
    );
    validate_schema_shape(&db).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM pragma_table_info('agent_messages') WHERE name='message_json'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .read_agent_mailbox(&id, MailboxRead::All)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn indexed_message_reads_join_once_and_reject_a_missing_canonical_acceptance() {
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static READS: AtomicUsize = AtomicUsize::new(0);
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = ready_fixture(&store).await;
    let db = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    db.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT,
        Some(|event| {
            if matches!(event, TraceEvent::Stmt(..)) {
                READS.fetch_add(1, Ordering::Relaxed);
            }
        }),
    );
    let message = MessageId::new("message-1").unwrap();
    READS.store(0, Ordering::Relaxed);
    assert!(
        super::super::session_store::read_indexed_message(&db, &id, &message)
            .unwrap()
            .is_some()
    );
    assert_eq!(READS.load(Ordering::Relaxed), 1);
    db.trace_v2(TraceEventCodes::empty(), None);
    db.execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM agent_controls WHERE seq=1")
        .unwrap();
    assert!(matches!(
        super::super::session_store::read_indexed_message(&db, &id, &message),
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn inspection_rejects_corrupt_scalar_text_without_filtering_pending_messages() {
    for corruption in [
        "UPDATE agent_messages SET message_id = printf('%1000000s', '') WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET delivery = printf('%1000000s', '') WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET target = CAST(X'ff' AS TEXT) WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET bound_turn_id = printf('%1000000s', '') WHERE message_id = 'message-2'",
    ] {
        let root = private_tempdir();
        let store = SqliteStore::open(root.path()).unwrap();
        let id = ready_fixture(&store).await;
        let before = store.inspect_session(&id).await.unwrap();
        assert_eq!(before.pending.len(), 3);
        assert!(
            before
                .pending
                .iter()
                .all(|message| message.bound_turn_id.is_none())
        );
        let database = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        database
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON")
            .unwrap();
        database.execute_batch(corruption).unwrap();
        assert!(
            matches!(
                store.inspect_session(&id).await,
                Err(StoreError::Corrupt(_))
            ),
            "{corruption}"
        );
    }
}

#[tokio::test]
async fn ready_projection_bounds_source_and_rejects_corrupt_lookahead_without_filtering() {
    for corruption in [
        "DELETE FROM agent_messages WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET message_source = 'unknown' WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET message_source = printf('%1000000s', '') WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET message_source = CAST(X'ff' AS TEXT) WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET root_session_id = 'other-root' WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET state = 'claimed' WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET target = 'next_step' WHERE message_id = 'message-2'",
        "UPDATE agent_messages SET wake_required = 0 WHERE message_id = 'message-2'",
    ] {
        let root = private_tempdir();
        let store = SqliteStore::open(root.path()).unwrap();
        let id = ready_fixture(&store).await;
        let first = store.list_ready_messages(&id, None, 1).await.unwrap();
        assert!(first.has_more);
        let database = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        database
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON")
            .unwrap();
        database.execute_batch(corruption).unwrap();
        for after in [None, Some(first.messages[0].cursor())] {
            let result = store.list_ready_messages(&id, after.as_ref(), 1).await;
            assert!(
                matches!(result, Err(StoreError::Corrupt(_))),
                "{corruption}: {result:?}"
            );
        }
    }
}

#[tokio::test]
async fn ready_projection_classifies_metadata_without_reading_message_bodies() {
    use rsi_agent_session_protocol::AgentMessageSourceKind;
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = ready_fixture(&store).await;
    let database = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    database
        .execute("UPDATE agent_controls SET control_json = '{}'", [])
        .unwrap();
    for (encoded, expected) in [
        ("human", AgentMessageSourceKind::Human),
        ("application", AgentMessageSourceKind::Application),
        ("agent", AgentMessageSourceKind::Agent),
        ("completion", AgentMessageSourceKind::Completion),
        ("continuation", AgentMessageSourceKind::Continuation),
    ] {
        database
            .execute("UPDATE agent_messages SET message_source = ?1", [encoded])
            .unwrap();
        let page = store.list_ready_messages(&id, None, 2).await.unwrap();
        assert!(page.has_more);
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.control_seq)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(
            page.messages
                .iter()
                .all(|message| message.source_kind == expected)
        );
        let next = store
            .list_ready_messages(&id, Some(&page.messages[1].cursor()), 2)
            .await
            .unwrap();
        assert!(!next.has_more);
        assert_eq!(next.messages[0].control_seq, 3);
        assert_eq!(next.messages[0].source_kind, expected);
    }
    // Metadata is deliberately not a substitute for the canonical validation proof.
    drop(database);
    drop(store);
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn exact_schema_rejects_missing_or_changed_waiting_predicate() {
    for replacement in [
        "",
        "CREATE INDEX active_activations_waiting ON active_activations (session_id) WHERE phase = 'running'",
    ] {
        let root = private_tempdir();
        drop(SqliteStore::open(root.path()).unwrap());
        let path = root.path().join("sessions.sqlite3");
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            rsi_agent_store_protocol::AGENT_STORE_SCHEMA_VERSION
        );
        connection
            .execute_batch("DROP INDEX active_activations_waiting")
            .unwrap();
        connection.execute_batch(replacement).unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            SqliteStore::open(root.path()),
            Err(StoreError::Corrupt(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(matches!(
            SqliteStore::verify(root.path()),
            Err(StoreError::Corrupt(_))
        ));
    }
}
