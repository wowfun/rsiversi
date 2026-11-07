use super::*;
use rsi_agent_session_protocol::{
    AgentMessageContent, AgentMessageSourceKind, MessageDelivery, MessageOptions,
};
use rusqlite::trace::{TraceEvent, TraceEventCodes};

#[tokio::test]
async fn summary_observes_pending_step_order_without_materializing_bodies() {
    static SELECTS: AtomicU64 = AtomicU64::new(0);
    static BODY_SELECTS: AtomicU64 = AtomicU64::new(0);
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
    store.validate_session(&id).await.unwrap();
    for connection in store.inner.connections.readers.lock().unwrap().iter_mut() {
        connection.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(|event| {
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
        );
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
        connection.trace_v2(TraceEventCodes::empty(), None);
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
