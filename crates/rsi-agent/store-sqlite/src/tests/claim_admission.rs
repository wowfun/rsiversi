use super::*;
use rsi_agent_session_protocol::{AgentMessageContent, AgentPath, MessageDelivery, MessageOptions};
use rusqlite::trace::{TraceEvent, TraceEventCodes};

#[tokio::test]
async fn mailbox_claim_checks_the_append_without_rereading_its_fact() {
    static FACT_READS: AtomicU64 = AtomicU64::new(0);
    let root = private_tempdir();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("claim-admission");
    let session_id = header.session_id().clone();
    let message = AgentMessage {
        message_id: MessageId::new("input").unwrap(),
        source: AgentMessageSource::Human { attribution: None },
        content: vec![AgentMessageContent::Text {
            text: "input".repeat(32_000),
        }],
        options: MessageOptions::default(),
    };
    let commit = |append| AtomicAgentCommit {
        sessions: vec![append],
        required_active_activations: vec![],
        quiescent_descendants_of: None,
    };
    store
        .commit_agent(commit(accepted_append(header, &message)))
        .await
        .unwrap();
    store.validate_session(&session_id).await.unwrap();
    store
        .with_writer(|connection| {
            connection.trace_v2(
                TraceEventCodes::SQLITE_TRACE_STMT,
                Some(|event| {
                    if let TraceEvent::Stmt(_, sql) = event
                        && sql.trim_start().starts_with("SELECT")
                        && sql.contains("fact_json")
                    {
                        FACT_READS.fetch_add(1, Ordering::Relaxed);
                    }
                }),
            );
            Ok(())
        })
        .await
        .unwrap();
    for (claimed_fact_seq, matching) in [(2, true), (4, true), (3, false), (3, true)] {
        let result = store
            .commit_agent(commit(claim_append(
                &session_id,
                &message,
                claimed_fact_seq,
                matching,
            )))
            .await;
        if claimed_fact_seq == 3 && matching {
            assert!(result.is_ok(), "{result:?}");
        } else {
            assert!(matches!(result, Err(StoreError::Invalid(_))), "{result:?}");
            let marks = store.read_watermarks(&session_id).await.unwrap();
            assert_eq!((marks.durable_fact_seq, marks.durable_control_seq), (0, 1));
        }
    }
    assert_eq!(
        FACT_READS.load(Ordering::Relaxed),
        0,
        "claim admission must use its typed append"
    );
    store
        .with_writer(|connection| {
            connection.trace_v2(TraceEventCodes::empty(), None);
            Ok(())
        })
        .await
        .unwrap();
    let facts = store
        .read_facts(&session_id, 0, 3, 1024 * 1024)
        .await
        .unwrap();
    assert!(
        matches!(facts.facts[2].body(), SessionFactBody::InputMessageClaimed {content, ..} if content == &message.content)
    );
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .read_facts(&session_id, 0, 3, 1024 * 1024)
            .await
            .unwrap()
            .facts,
        facts.facts
    );
}

fn claim_append(
    session_id: &SessionId,
    message: &AgentMessage,
    claimed_fact_seq: u64,
    matching: bool,
) -> AtomicSessionAppend {
    let turn = TurnId::new("turn").unwrap();
    let step = StepId::new("step").unwrap();
    let activation = ActivationId::new("activation").unwrap();
    let bodies = [
        SessionFactBody::MessageTurnAccepted {
            turn_id: turn.clone(),
            activation_id: activation.clone(),
            message_ids: vec![message.message_id.clone()],
            reasoning_effort: None,
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::StepStarted {
            turn_id: turn.clone(),
            step_id: step.clone(),
        },
        SessionFactBody::InputMessageClaimed {
            turn_id: turn.clone(),
            step_id: step.clone(),
            source: message.claimed_source(),
            content: if matching {
                message.content.clone()
            } else {
                vec![AgentMessageContent::Text {
                    text: "changed".into(),
                }]
            },
        },
    ];
    let facts = bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| Arc::new(SessionFact::new(index as u64 + 1, 2, body).unwrap()))
        .collect();
    AtomicSessionAppend {
        session_id: session_id.clone(),
        header: None,
        expected_fact_seq: 0,
        expected_control_seq: 1,
        facts,
        controls: vec![
            AgentControlRecord::new(
                2,
                2,
                AgentControlRecordBody::ActivationStarted {
                    activation_id: activation.clone(),
                    root_session_id: session_id.clone(),
                    parent_session_id: None,
                    path: AgentPath::root(),
                },
            )
            .unwrap(),
            AgentControlRecord::new(
                3,
                2,
                AgentControlRecordBody::MessageClaimed {
                    message_id: message.message_id.clone(),
                    activation_id: activation.clone(),
                    turn_id: turn.clone(),
                    step_id: step.clone(),
                    claimed_fact_seq,
                },
            )
            .unwrap(),
        ],
    }
}

fn accepted_append(header: SessionHeader, message: &AgentMessage) -> AtomicSessionAppend {
    let session_id = header.session_id().clone();
    AtomicSessionAppend {
        session_id: session_id.clone(),
        header: Some(header),
        expected_fact_seq: 0,
        expected_control_seq: 0,
        facts: vec![],
        controls: vec![
            AgentControlRecord::new(
                1,
                1,
                AgentControlRecordBody::MessageAccepted {
                    message: message.clone(),
                    root_session_id: session_id.clone(),
                    delivery: MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
            .unwrap(),
        ],
    }
}
