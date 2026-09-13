use super::*;

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
                        source: AgentMessageSource::Human,
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
        .commit_agent(settlement::commit(vec![AtomicSessionAppend {
            session_id: id.clone(),
            expected_fact_seq: 1,
            expected_control_seq: 0,
            header: None,
            facts: vec![],
            controls,
        }]))
        .await
        .unwrap();
    id
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
        let root = tempfile::tempdir().unwrap();
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
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = ready_fixture(&store).await;
    let database = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    database
        .execute("UPDATE agent_messages SET message_json = '{}'", [])
        .unwrap();
    for (encoded, expected) in [
        ("human", AgentMessageSourceKind::Human),
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
        let root = tempfile::tempdir().unwrap();
        drop(SqliteStore::open(root.path()).unwrap());
        let path = root.path().join("sessions.sqlite3");
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            19
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
