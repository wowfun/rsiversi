use super::*;
use rsi_agent_session_protocol::{
    AgentMessageContent, AgentMessageSourceKind, MessageDelivery, MessageOptions,
};
use rusqlite::trace::{TraceEvent, TraceEventCodes};

async fn validation_mailbox() -> (tempfile::TempDir, SqliteStore, SessionId) {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("mailbox-validation");
    let id = header.session_id().clone();
    let mut controls = Vec::new();
    for (seq, name) in [(1, "pending"), (2, "discarded")] {
        controls.push(
            AgentControlRecord::new(
                seq,
                seq,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: MessageId::new(name).unwrap(),
                        source: AgentMessageSource::Human { attribution: None },
                        content: vec![AgentMessageContent::Text {
                            text: "input".repeat(200_000),
                        }],
                        options: MessageOptions::default(),
                    },
                    root_session_id: id.clone(),
                    delivery: MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
            .unwrap(),
        );
    }
    controls.push(
        AgentControlRecord::new(
            3,
            3,
            AgentControlRecordBody::MessageDiscarded {
                message_id: MessageId::new("discarded").unwrap(),
                reason: MessageDiscardReason::Cancelled,
            },
        )
        .unwrap(),
    );
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                header: Some(header),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                facts: vec![],
                controls,
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    (root, store, id)
}

#[tokio::test]
async fn cold_mailbox_validation_does_not_reload_acceptance_bodies() {
    static BODY_RESELECTS: AtomicU64 = AtomicU64::new(0);
    let (_root, store, id) = validation_mailbox().await;
    let mut writer = store.inner.connections.writer.lock().unwrap();
    let writer = writer.as_mut().unwrap();
    writer
        .trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(|event: rusqlite::trace::TraceEvent<'_>| {
                if let TraceEvent::Stmt(_, sql) = event
                    && sql.contains("FROM agent_messages")
                    && sql.contains("control_json")
                {
                    BODY_RESELECTS.fetch_add(1, Ordering::Relaxed);
                }
            }),
        )
        .unwrap();
    BODY_RESELECTS.store(0, Ordering::Relaxed);
    validation::validate_session(writer, &id).unwrap();
    writer
        .trace_v2(
            TraceEventCodes::empty(),
            None::<fn(rusqlite::trace::TraceEvent<'_>)>,
        )
        .unwrap();
    assert_eq!(
        BODY_RESELECTS.load(Ordering::Relaxed),
        0,
        "pending and settled index checks reuse the canonical acceptance already replayed"
    );
}

#[tokio::test]
async fn mailbox_index_check_rejects_each_changed_coordinate_and_state() {
    let (_root, store, id) = validation_mailbox().await;
    let mut writer = store.inner.connections.writer.lock().unwrap();
    let writer = writer.as_mut().unwrap();
    let expected =
        validation::read_indexed_agent_message(writer, &id, &MessageId::new("discarded").unwrap())
            .unwrap();
    validation::validate_indexed_message(writer, &id, &expected).unwrap();
    for change in [
        "message_id = 'different'",
        "message_source = 'application'",
        "root_session_id = 'foreign'",
        "target = 'next_step'",
        "wake_required = 0",
        "accepted_control_seq = 3",
        "state = 'pending'",
        "delivery = 'next_step'",
        "bound_turn_id = 'other-turn'",
        "accepted_timestamp_ms = 9",
        "queue_slot_id = 'different'",
        "queue_timestamp_ms = 9",
        "queue_control_seq = 1",
        "has_turn_options = 1",
        "state_json = '{}'",
        "state_json = json_set(state_json, '$.control_seq', 2)",
        "state_json = printf('%4097s', '') || state_json",
    ] {
        let transaction = writer.transaction().unwrap();
        transaction.execute(
            &format!("UPDATE agent_messages SET {change} WHERE session_id = ?1 AND message_id = 'discarded'"),
            [id.as_str()],
        ).unwrap();
        assert!(
            matches!(
                validation::validate_indexed_message(&transaction, &id, &expected),
                Err(StoreError::Corrupt(_))
            ),
            "{change}"
        );
        transaction.rollback().unwrap();
    }
    let transaction = writer.transaction().unwrap();
    transaction.execute(
        "UPDATE agent_messages SET state_json = ?1 WHERE session_id = ?2 AND message_id = 'discarded'",
        params![serde_json::to_string_pretty(&expected.state).unwrap(), id.as_str()],
    ).unwrap();
    validation::validate_indexed_message(&transaction, &id, &expected).unwrap();
    transaction.rollback().unwrap();
    let mut oversized = expected;
    oversized.encoded_message_bytes = MAXIMUM_STORE_MAILBOX_PAGE_BYTES + 1;
    assert!(matches!(
        validation::validate_indexed_message(writer, &id, &oversized),
        Err(StoreError::Corrupt(_))
    ));
}

async fn summary_mailbox() -> (tempfile::TempDir, SqliteStore, SessionId) {
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("wait-summary");
    let id = header.session_id().clone();
    let acceptance = |seq, message_id: &str, source, delivery, target, text: String| {
        AgentControlRecord::new(
            seq,
            seq,
            AgentControlRecordBody::MessageAccepted {
                message: AgentMessage {
                    message_id: MessageId::new(message_id).unwrap(),
                    source,
                    content: vec![AgentMessageContent::Text { text }],
                    options: MessageOptions::default(),
                },
                root_session_id: id.clone(),
                delivery,
                bound_turn_id: None,
                target,
                wake_required: target == MessageTarget::NextTurn,
            },
        )
        .unwrap()
    };
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                header: Some(header),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                facts: vec![],
                controls: vec![
                    acceptance(
                        1,
                        "turn",
                        AgentMessageSource::Human { attribution: None },
                        MessageDelivery::NextTurn,
                        MessageTarget::NextTurn,
                        "queued turn".into(),
                    ),
                    acceptance(
                        2,
                        "z-first",
                        AgentMessageSource::Human { attribution: None },
                        MessageDelivery::NextStep,
                        MessageTarget::NextStep,
                        "x".repeat(1024 * 1024),
                    ),
                    acceptance(
                        3,
                        "a-second",
                        AgentMessageSource::Application { attribution: None },
                        MessageDelivery::NextStep,
                        MessageTarget::NextStep,
                        "later input".into(),
                    ),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    (root, store, id)
}

#[tokio::test]
async fn summary_observes_pending_step_order_without_materializing_bodies() {
    static SELECTS: AtomicU64 = AtomicU64::new(0);
    static BODY_SELECTS: AtomicU64 = AtomicU64::new(0);
    let (root, store, id) = summary_mailbox().await;
    store.validate_session(&id).await.unwrap();
    for connection in store.inner.connections.readers.lock().unwrap().iter_mut() {
        connection
            .trace_v2(
                TraceEventCodes::SQLITE_TRACE_STMT,
                Some(|event: rusqlite::trace::TraceEvent<'_>| {
                    if let TraceEvent::Stmt(_, sql) = event
                        && sql.trim_start().starts_with("SELECT")
                    {
                        SELECTS.fetch_add(1, Ordering::Relaxed);
                        if ["control_json", "fact_json", "header_json"]
                            .iter()
                            .any(|column| sql.contains(column))
                        {
                            BODY_SELECTS.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }),
            )
            .unwrap();
    }
    for _ in 0..3 {
        let summary = store.read_agent_mailbox_summary(&id).await.unwrap();
        assert_eq!(
            summary.first_pending_step_source,
            Some(AgentMessageSourceKind::Human)
        );
        assert_eq!(summary.pending_count, 3);
        assert_eq!(
            (summary.durable_fact_seq, summary.durable_control_seq),
            (0, 3)
        );
    }
    assert_eq!(
        SELECTS.load(Ordering::Relaxed),
        6,
        "source lookup stays in the existing summary query"
    );
    assert_eq!(BODY_SELECTS.load(Ordering::Relaxed), 0);
    for connection in store.inner.connections.readers.lock().unwrap().iter_mut() {
        connection
            .trace_v2(
                TraceEventCodes::empty(),
                None::<fn(rusqlite::trace::TraceEvent<'_>)>,
            )
            .unwrap();
    }
    for (seq, message_id, expected) in [
        (4, "z-first", Some(AgentMessageSourceKind::Application)),
        (5, "a-second", None),
    ] {
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: id.clone(),
                    header: None,
                    expected_fact_seq: 0,
                    expected_control_seq: seq - 1,
                    facts: vec![],
                    controls: vec![
                        AgentControlRecord::new(
                            seq,
                            seq,
                            AgentControlRecordBody::MessageDiscarded {
                                message_id: MessageId::new(message_id).unwrap(),
                                reason: MessageDiscardReason::Cancelled,
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
        let summary = store.read_agent_mailbox_summary(&id).await.unwrap();
        assert_eq!(summary.first_pending_step_source, expected);
        assert_eq!(summary.pending_count, usize::try_from(6 - seq).unwrap());
        assert_eq!(summary.durable_control_seq, seq);
    }
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    let reopened = SqliteStore::open(root.path()).unwrap();
    let summary = reopened.read_agent_mailbox_summary(&id).await.unwrap();
    assert_eq!(summary.first_pending_step_source, None);
    assert_eq!(
        summary.pending_count, 1,
        "next-Turn input does not resume a waiting Tool"
    );
}
