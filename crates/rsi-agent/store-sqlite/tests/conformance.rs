use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, AgentMessage, AgentMessageContent,
    AgentMessageSource, AgentPath, AgentPresetId, EMPTY_CONTROL_PREFIX_DIGEST,
    EMPTY_FACT_PREFIX_DIGEST, ForkOrigin, ForkTurnSelection, FrozenAgentSettings,
    MAXIMUM_DURABLE_AGENT_TREE_NODES, MAXIMUM_PENDING_AGENT_MESSAGES, MAXIMUM_SESSION_FACT_BYTES,
    MAXIMUM_SESSION_HEADER_BYTES, MessageDiscardReason, MessageId, MessageOptions, MessageTarget,
    SessionFact, SessionFactBody, SessionHeader, SessionId, StepId, TurnId,
};
use rsi_agent_store_protocol::{
    AGENT_STORE_SCHEMA_VERSION, AppendBatch, AtomicAgentCommit, AtomicSessionAppend,
    MAXIMUM_STORE_MAILBOX_PAGE_BYTES, SessionStore, StoreError, StoreWorkspaceContextState,
    StoredContextCheckpoint, WriteContextCheckpoint,
};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_agent_testkit::assert_mechanical_store_contract;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use rusqlite::Connection;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;

fn header(session: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session).unwrap(),
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

fn fact(seq: u64) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
            text: format!("text-{seq}"),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

fn terminal_fact(seq: u64, turn: u64) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::TurnTerminal {
            turn_id: TurnId::new(format!("turn-{turn}")).unwrap(),
            outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn fork_boundary_rejects_an_unselected_turn_interleaved_in_the_interval() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("session-fork-interleaved").unwrap();
    let invoking = TurnId::new("turn-3").unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header(session_id.as_str())),
            facts: vec![
                fact(1),
                fact(2),
                terminal_fact(3, 1),
                terminal_fact(4, 2),
                SessionFact::new(
                    5,
                    5,
                    SessionFactBody::TurnAccepted {
                        turn_id: invoking.clone(),
                        text: "spawn".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();
    assert!(matches!(
        store
            .resolve_fork_boundary(&session_id, &invoking, ForkTurnSelection::Last(1))
            .await,
        Err(StoreError::Invalid(message))
            if message.contains("balanced contiguous completed-turn interval")
    ));
    assert_eq!(
        store
            .resolve_fork_boundary(&session_id, &invoking, ForkTurnSelection::All)
            .await
            .unwrap()
            .effective_turns,
        2
    );
}

#[tokio::test]
async fn workspace_context_digests_are_recovered_from_the_sqlite_fact_projection() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("session-workspace-context-state").unwrap();
    let turn_id = TurnId::new("turn-workspace-context-state").unwrap();
    let step_id = StepId::new("step-workspace-context-state").unwrap();
    let instructions_sha256 = "a".repeat(64);
    let skill_catalog_sha256 = "b".repeat(64);
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header(session_id.as_str())),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn_id.clone(),
                        text: "publish workspace context".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
                SessionFact::new(
                    2,
                    1,
                    SessionFactBody::StepStarted {
                        turn_id: turn_id.clone(),
                        step_id: step_id.clone(),
                    },
                )
                .unwrap(),
                SessionFact::new(
                    3,
                    1,
                    SessionFactBody::InputMessageEntered {
                        turn_id: turn_id.clone(),
                        step_id: step_id.clone(),
                        source: rsi_agent_session_protocol::InputMessageSource::AgentInstructions {
                            source: "project/AGENTS.md".into(),
                            sha256: instructions_sha256.clone(),
                            replacement: true,
                            tombstone: false,
                        },
                        content: vec![AgentMessageContent::Text {
                            text: "workspace instructions".into(),
                        }],
                    },
                )
                .unwrap(),
                SessionFact::new(
                    4,
                    1,
                    SessionFactBody::InputMessageEntered {
                        turn_id: turn_id.clone(),
                        step_id,
                        source: rsi_agent_session_protocol::InputMessageSource::SkillCatalog {
                            sha256: skill_catalog_sha256.clone(),
                        },
                        content: vec![AgentMessageContent::Text {
                            text: "workspace skills".into(),
                        }],
                    },
                )
                .unwrap(),
                SessionFact::new(
                    5,
                    1,
                    SessionFactBody::TurnTerminal {
                        turn_id,
                        outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();

    assert_eq!(
        store
            .read_workspace_context_state(&session_id)
            .await
            .unwrap(),
        StoreWorkspaceContextState {
            instructions_sha256: Some(instructions_sha256),
            skill_catalog_sha256: Some(skill_catalog_sha256),
            durable_fact_seq: 5,
        }
    );
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
}

fn accepted_message_control(
    seq: u64,
    session_id: &SessionId,
    message_id: &MessageId,
) -> AgentControlRecord {
    AgentControlRecord::new(
        seq,
        seq,
        AgentControlRecordBody::MessageAccepted {
            message: AgentMessage {
                message_id: message_id.clone(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "mailbox input".into(),
                }],
                options: MessageOptions::default(),
            },
            root_session_id: session_id.clone(),
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    )
    .unwrap()
}

fn insert_agent_node_without_admission(database: &Path, header: &SessionHeader) {
    let session_id = header.session_id();
    let origin = header.fork_origin().unwrap();
    let connection = Connection::open(database).unwrap();
    connection
        .execute(
            "INSERT INTO sessions
                 (session_id, created_at_ms, header_json, durable_seq,
                  fact_prefix_sha256, control_seq, control_prefix_sha256)
             VALUES (?1, ?2, ?3, 0, ?4, 0, ?5)",
            rusqlite::params![
                session_id.as_str(),
                i64::try_from(header.created_at_ms()).unwrap(),
                serde_json::to_string(header).unwrap(),
                hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                hex::encode(EMPTY_CONTROL_PREFIX_DIGEST),
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO agent_nodes
                 (session_id, root_session_id, parent_session_id, path_json, task_name)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                session_id.as_str(),
                origin.root_session_id.as_str(),
                origin.parent_session_id.as_str(),
                serde_json::to_string(&origin.path).unwrap(),
                &origin.task_name,
            ],
        )
        .unwrap();
}

#[tokio::test]
async fn zero_fact_agent_session_is_durable_and_multi_session_conflict_rolls_back() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let parent_id = SessionId::new("atomic-parent").unwrap();
    store
        .append(AppendBatch {
            session_id: parent_id.clone(),
            expected_seq: 0,
            header: Some(header(parent_id.as_str())),
            facts: vec![fact(1)],
        })
        .await
        .unwrap();

    let child_id = SessionId::new("atomic-zero-fact-child").unwrap();
    let child_message = MessageId::new("atomic-child-message").unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: child_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header(child_id.as_str())),
                facts: Vec::new(),
                controls: vec![accepted_message_control(1, &child_id, &child_message)],
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        store.read_facts(&child_id, 0, 1).await.unwrap().durable_seq,
        0
    );
    assert_eq!(
        store
            .read_controls(&child_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        1
    );

    let parent_message = MessageId::new("atomic-parent-message").unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![
                    AtomicSessionAppend {
                        session_id: child_id.clone(),
                        expected_fact_seq: 0,
                        expected_control_seq: 1,
                        header: None,
                        facts: Vec::new(),
                        controls: vec![
                            AgentControlRecord::new(
                                2,
                                2,
                                AgentControlRecordBody::MessageDiscarded {
                                    message_id: child_message,
                                    reason: MessageDiscardReason::Cancelled,
                                },
                            )
                            .unwrap(),
                        ],
                    },
                    AtomicSessionAppend {
                        session_id: parent_id.clone(),
                        expected_fact_seq: 99,
                        expected_control_seq: 0,
                        header: None,
                        facts: Vec::new(),
                        controls: vec![accepted_message_control(1, &parent_id, &parent_message,)],
                    },
                ],
                required_active_activations: Vec::new(),
                quiescent_sessions: Vec::new(),
            })
            .await,
        Err(StoreError::Conflict {
            expected: 99,
            actual: 1
        })
    ));
    assert_eq!(
        store
            .read_controls(&child_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        1
    );
    assert_eq!(
        store
            .read_controls(&parent_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        0
    );
}

#[tokio::test]
async fn durable_agent_tree_accepts_exactly_its_declared_node_bound() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let root_id = SessionId::new("bounded-tree-root").unwrap();
    let root_header = header(root_id.as_str());
    store
        .append(AppendBatch {
            session_id: root_id.clone(),
            expected_seq: 0,
            header: Some(root_header.clone()),
            facts: vec![fact(1)],
        })
        .await
        .unwrap();
    let parent_header_fingerprint = root_header.fingerprint().unwrap();
    let child_header = |index: usize| {
        let child_id = SessionId::new(format!("bounded-tree-child-{index}")).unwrap();
        root_header
            .forked_child(
                child_id,
                u64::try_from(index + 1).unwrap(),
                ForkOrigin {
                    parent_session_id: root_id.clone(),
                    root_session_id: root_id.clone(),
                    path: AgentPath::new(vec![u16::try_from(index).unwrap()]).unwrap(),
                    task_name: format!("bounded-task-{index}"),
                    parent_header_fingerprint: parent_header_fingerprint.clone(),
                    invoking_turn_id: TurnId::new("turn-1").unwrap(),
                    resolved_after_seq: 0,
                    resolved_terminal_seq: 0,
                    terminal_prefix_sha256: hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                    requested_turns: ForkTurnSelection::None,
                    effective_turns: 0,
                },
            )
            .unwrap()
    };

    for index in 1..MAXIMUM_DURABLE_AGENT_TREE_NODES {
        let child = child_header(index);
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: child.session_id().clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(child),
                    facts: vec![fact(1)],
                    controls: Vec::new(),
                }],
                required_active_activations: Vec::new(),
                quiescent_sessions: Vec::new(),
            })
            .await
            .unwrap();
    }

    let overflow = child_header(MAXIMUM_DURABLE_AGENT_TREE_NODES);
    let corrupt_overflow = overflow.clone();
    let overflow_id = overflow.session_id().clone();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: overflow_id.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(overflow),
                    facts: vec![fact(1)],
                    controls: Vec::new(),
                }],
                required_active_activations: Vec::new(),
                quiescent_sessions: Vec::new(),
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("durable node bound")
    ));
    assert!(matches!(
        store.header(&overflow_id).await,
        Err(StoreError::NotFound(_))
    ));
    drop(store);

    insert_agent_node_without_admission(&root.path().join("sessions.sqlite3"), &corrupt_overflow);

    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(message)) if message.contains("durable node bound")
    ));
}

#[tokio::test]
async fn ready_index_schema_rejects_nonwaking_next_step_rows() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = SessionId::new("ready-target-schema").unwrap();
    let message = MessageId::new("ready-target-schema-message").unwrap();
    let control = AgentControlRecord::new(
        1,
        1,
        AgentControlRecordBody::MessageAccepted {
            message: AgentMessage {
                message_id: message.clone(),
                source: AgentMessageSource::Completion {
                    child_session_id: SessionId::new("ready-target-source").unwrap(),
                    activation_id: rsi_agent_session_protocol::ActivationId::new(
                        "ready-target-activation",
                    )
                    .unwrap(),
                },
                content: vec![AgentMessageContent::Text {
                    text: "done".into(),
                }],
                options: MessageOptions::default(),
            },
            root_session_id: session.clone(),
            target: MessageTarget::NextStep,
            wake_required: false,
        },
    )
    .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header(session.as_str())),
                facts: Vec::new(),
                controls: vec![control],
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        })
        .await
        .unwrap();
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let error = connection
        .execute(
            "INSERT INTO ready_messages
                 (root_session_id, session_id, message_id, ready_control_seq,
                  timestamp_ms, target)
             VALUES (?1, ?1, ?2, 1, 1, 'next_step')",
            rusqlite::params![session.as_str(), message.as_str()],
        )
        .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint failed"));
}

#[tokio::test]
async fn sqlite_store_passes_the_shared_mechanical_contract() {
    let root = tempfile::tempdir().unwrap();
    let turn = TurnId::new("turn-1").unwrap();
    assert_mechanical_store_contract(
        &SqliteStore::open(root.path()).unwrap(),
        header("shared-contract"),
        fact(1),
        SessionFact::new(
            2,
            2,
            SessionFactBody::CancelRequested {
                turn_id: turn.clone(),
                reason: Some("stop".into()),
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::TurnTerminal {
                turn_id: turn,
                outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
            },
        )
        .unwrap(),
    )
    .await;
    SqliteStore::verify(root.path()).unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One scenario pins append, both page orders, conflict, and reopen.
async fn append_pagination_conflict_and_reopen_match_the_store_contract() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = SessionId::new("session-1").unwrap();
    let commit = store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-1")),
            facts: vec![fact(1), fact(2)],
        })
        .await
        .unwrap();
    assert_eq!(commit.durable_seq, 2);
    assert!(matches!(
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 1,
                header: None,
                facts: vec![fact(2)],
            })
            .await,
        Err(StoreError::Conflict {
            expected: 1,
            actual: 2
        })
    ));
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 2,
            header: None,
            facts: vec![fact(3)],
        })
        .await
        .unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 3,
            header: None,
            facts: vec![terminal_fact(4, 2)],
        })
        .await
        .unwrap();
    let first = store.read_facts(&session, 0, 2).await.unwrap();
    assert_eq!(first.facts, vec![fact(1), fact(2)]);
    assert!(!first.caught_up());
    let second = store.read_facts(&session, 2, 2).await.unwrap();
    assert_eq!(second.facts, vec![fact(3), terminal_fact(4, 2)]);
    assert!(second.caught_up());
    for name in ["session-2", "session-3"] {
        let id = SessionId::new(name).unwrap();
        store
            .append(AppendBatch {
                session_id: id,
                expected_seq: 0,
                header: Some(header(name)),
                facts: vec![fact(1)],
            })
            .await
            .unwrap();
    }
    let first_sessions = store.list_sessions(None, 2).await.unwrap();
    assert_eq!(
        first_sessions.sessions,
        vec![
            SessionId::new("session-1").unwrap(),
            SessionId::new("session-2").unwrap()
        ]
    );
    assert!(first_sessions.has_more);
    let second_sessions = store
        .list_sessions(first_sessions.sessions.last(), 2)
        .await
        .unwrap();
    assert_eq!(
        second_sessions.sessions,
        vec![SessionId::new("session-3").unwrap()]
    );
    let recent = store.list_recent_sessions(None, 2).await.unwrap();
    assert_eq!(
        recent
            .sessions
            .iter()
            .map(|row| row.header.session_id().as_str())
            .collect::<Vec<_>>(),
        vec!["session-3", "session-2"]
    );
    assert!(recent.has_more);
    let next_recent = store
        .list_recent_sessions(Some(&recent.sessions[1].cursor()), 2)
        .await
        .unwrap();
    assert_eq!(
        next_recent
            .sessions
            .iter()
            .map(|row| row.header.session_id().as_str())
            .collect::<Vec<_>>(),
        vec!["session-1"]
    );
    assert!(!next_recent.has_more);
    assert!(!second_sessions.has_more);
    drop(store);

    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        reopened.header(&session).await.unwrap(),
        header("session-1")
    );
    let sessions = reopened.list_sessions(None, 1).await.unwrap();
    assert_eq!(sessions.sessions, vec![session]);
    assert!(sessions.has_more);
    assert_eq!(
        reopened
            .read_facts(&SessionId::new("session-1").unwrap(), 0, 8)
            .await
            .unwrap()
            .durable_seq,
        4
    );
}

#[tokio::test]
async fn clean_close_makes_the_main_database_a_complete_store() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = SessionId::new("session-clean-close").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![fact(1)],
        })
        .await
        .unwrap();
    drop(store);

    let copy = tempfile::tempdir().unwrap();
    std::fs::copy(
        root.path().join("sessions.sqlite3"),
        copy.path().join("sessions.sqlite3"),
    )
    .unwrap();
    let copied = SqliteStore::open(copy.path()).unwrap();
    assert_eq!(
        copied.header(&session).await.unwrap(),
        header(session.as_str())
    );
}

#[test]
fn open_does_not_initialize_an_existing_nonempty_database() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("VACUUM;").unwrap();
    drop(connection);
    assert_ne!(std::fs::metadata(&database).unwrap().len(), 0);

    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::SchemaMismatch { actual: 0, .. })
    ));
}

#[tokio::test]
async fn open_session_cursor_pagination_is_lexical_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    for name in ["session-1", "session-2", "session-3"] {
        store
            .append(AppendBatch {
                session_id: SessionId::new(name).unwrap(),
                expected_seq: 0,
                header: Some(header(name)),
                facts: vec![fact(1)],
            })
            .await
            .unwrap();
    }

    let first = store.list_open_sessions(None, 2).await.unwrap();
    assert_eq!(
        first.sessions,
        vec![
            SessionId::new("session-1").unwrap(),
            SessionId::new("session-2").unwrap()
        ]
    );
    assert!(first.has_more);
    let second = store
        .list_open_sessions(first.sessions.last(), 2)
        .await
        .unwrap();
    assert_eq!(second.sessions, vec![SessionId::new("session-3").unwrap()]);
    assert!(!second.has_more);
}

#[tokio::test]
async fn checkpoint_reads_reassert_immutable_session_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = SessionId::new("session-checkpoint-metadata").unwrap();
    let session_header = header(session.as_str());
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(session_header.clone()),
            facts: vec![fact(1)],
        })
        .await
        .unwrap();
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let fact_prefix_sha256 = connection
        .query_row(
            "SELECT fact_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    drop(connection);
    store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: session.clone(),
            expected_durable_seq: 1,
            checkpoint: StoredContextCheckpoint {
                header_fingerprint: session_header.fingerprint().unwrap(),
                through_seq: 1,
                fact_prefix_sha256,
                bytes: Arc::from(&b"opaque"[..]),
            },
        })
        .await
        .unwrap();

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE context_checkpoints SET header_fingerprint = ?1 WHERE session_id = ?2",
            rusqlite::params!["b".repeat(64), session.as_str()],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        store.read_context_checkpoint(&session).await,
        Err(StoreError::Corrupt(message)) if message.contains("header fingerprint")
    ));

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE context_checkpoints SET header_fingerprint = ?1, through_seq = 2
             WHERE session_id = ?2",
            rusqlite::params![session_header.fingerprint().unwrap(), session.as_str()],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        store.read_context_checkpoint(&session).await,
        Err(StoreError::Corrupt(message)) if message.contains("durable tail")
    ));
}

#[tokio::test]
async fn turn_indexes_support_exact_outcome_and_open_turn_queries() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = SessionId::new("session-turn-index").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![fact(1), fact(2), fact(3), terminal_fact(4, 2)],
        })
        .await
        .unwrap();

    let turn = store
        .read_turn_facts(&session, &TurnId::new("turn-2").unwrap(), 0, 8)
        .await
        .unwrap();
    assert_eq!(turn.facts, vec![fact(2), terminal_fact(4, 2)]);
    assert!(!turn.has_more);
    let open = store.list_open_turns(&session, 0, 8).await.unwrap();
    assert_eq!(
        open.turns
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1", "turn-3"]
    );
}

#[tokio::test]
async fn oversized_fact_rows_are_rejected_by_sql_length_before_json_decode() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session = SessionId::new("session-oversized-row").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![fact(1)],
        })
        .await
        .unwrap();

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE facts SET fact_json = printf('%.*c', ?1, 'x')
             WHERE session_id = ?2 AND seq = 1",
            rusqlite::params![
                i64::try_from(MAXIMUM_SESSION_FACT_BYTES + 1).unwrap(),
                session.as_str()
            ],
        )
        .unwrap();
    drop(connection);

    for error in [
        store.read_facts(&session, 0, 1).await.unwrap_err(),
        store
            .read_turn_facts(&session, &TurnId::new("turn-1").unwrap(), 0, 1)
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, StoreError::Corrupt(message) if message.contains("exceeds") && message.contains("session Fact"))
        );
    }

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET header_json = printf('%.*c', ?1, 'x')
             WHERE session_id = ?2",
            rusqlite::params![
                i64::try_from(MAXIMUM_SESSION_HEADER_BYTES + 1).unwrap(),
                session.as_str()
            ],
        )
        .unwrap();
    drop(connection);
    assert!(
        matches!(store.header(&session).await, Err(StoreError::Corrupt(message)) if message.contains("exceeds") && message.contains("session header"))
    );
    assert!(
        matches!(store.list_recent_sessions(None, 1).await, Err(StoreError::Corrupt(message)) if message.contains("exceeds") && message.contains("session header"))
    );
}

#[test]
fn dormant_turn_index_corruption_is_lazy_and_explicit_verify_finds_it() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(store.append(AppendBatch {
            session_id: SessionId::new("session-index-corrupt").unwrap(),
            expected_seq: 0,
            header: Some(header("session-index-corrupt")),
            facts: vec![fact(1)],
        }))
        .unwrap();
    runtime
        .block_on(store.append(AppendBatch {
            session_id: SessionId::new("session-valid").unwrap(),
            expected_seq: 0,
            header: Some(header("session-valid")),
            facts: vec![fact(1)],
        }))
        .unwrap();
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE facts SET turn_id = 'turn-tampered' WHERE session_id = ?1 AND seq = 1",
            ["session-index-corrupt"],
        )
        .unwrap();
    drop(connection);

    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        runtime
            .block_on(reopened.header(&SessionId::new("session-valid").unwrap()))
            .unwrap()
            .session_id()
            .as_str(),
        "session-valid"
    );
    assert!(matches!(
        runtime.block_on(
            reopened.read_agent_mailbox_summary(
                &SessionId::new("session-index-corrupt").unwrap()
            )
        ),
        Err(StoreError::Corrupt(message)) if message.contains("turn index")
    ));
    assert!(matches!(
        runtime.block_on(reopened.header(&SessionId::new("session-index-corrupt").unwrap())),
        Err(StoreError::Corrupt(message)) if message.contains("turn index")
    ));
    drop(reopened);
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn mailbox_read_rejects_an_oversized_indexed_message_before_loading_its_json() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let session = SessionId::new("session-oversized-mailbox-message").unwrap();
    let message = MessageId::new("message-oversized-mailbox-message").unwrap();
    runtime
        .block_on(store.commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header(session.as_str())),
                facts: Vec::new(),
                controls: vec![accepted_message_control(1, &session, &message)],
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        }))
        .unwrap();

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let original_message_json = connection
        .query_row(
            "SELECT message_json FROM agent_messages
             WHERE session_id = ?1 AND message_id = ?2",
            rusqlite::params![session.as_str(), message.as_str()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE agent_messages SET message_json = printf('%.*c', ?1, 'x')
             WHERE session_id = ?2 AND message_id = ?3",
            rusqlite::params![
                i64::try_from(MAXIMUM_STORE_MAILBOX_PAGE_BYTES + 1).unwrap(),
                session.as_str(),
                message.as_str(),
            ],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        runtime.block_on(store.read_agent_mailbox(&session, Some(&message))),
        Err(StoreError::Corrupt(message))
            if message.contains("Agent message") && message.contains("exceeds")
    ));

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE agent_messages
             SET message_json = ?1, state_json = printf('%.*c', ?2, 'x')
             WHERE session_id = ?3 AND message_id = ?4",
            rusqlite::params![
                original_message_json,
                i64::try_from(MAXIMUM_STORE_MAILBOX_PAGE_BYTES + 1).unwrap(),
                session.as_str(),
                message.as_str(),
            ],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        runtime.block_on(store.read_agent_mailbox(&session, Some(&message))),
        Err(StoreError::Corrupt(message))
            if message.contains("Agent message state") && message.contains("exceeds")
    ));
}

#[test]
fn mailbox_summary_bounds_completion_identity_rows_before_decoding() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let session = SessionId::new("session-corrupt-mailbox-summary").unwrap();
    let message = MessageId::new("message-corrupt-mailbox-summary").unwrap();
    runtime
        .block_on(store.commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header(session.as_str())),
                facts: Vec::new(),
                controls: vec![accepted_message_control(1, &session, &message)],
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        }))
        .unwrap();

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let corrupt_count = MAXIMUM_PENDING_AGENT_MESSAGES + 2;
    connection
        .execute_batch(&format!(
            "WITH RECURSIVE counter(value) AS (
                 SELECT 2 UNION ALL SELECT value + 1 FROM counter WHERE value < {corrupt_count}
             )
             INSERT INTO agent_controls (session_id, seq, control_json)
             SELECT '{session}', value, source.control_json
             FROM counter
             JOIN agent_controls AS source
               ON source.session_id = '{session}' AND source.seq = 1;

             UPDATE agent_messages
             SET message_source = 'completion', target = 'next_step', wake_required = 0
             WHERE session_id = '{session}';

             WITH RECURSIVE counter(value) AS (
                 SELECT 2 UNION ALL SELECT value + 1 FROM counter WHERE value < {corrupt_count}
             )
             INSERT INTO agent_messages
                 (session_id, message_id, accepted_control_seq, root_session_id,
                  message_source, message_json, target, wake_required, state, state_json)
             SELECT '{session}',
                    CASE WHEN value = {corrupt_count}
                         THEN printf('%.*c', 257, 'x')
                         ELSE printf('completion-%03d', value) END,
                    value, '{session}', 'completion', source.message_json,
                    'next_step', 0, 'pending', source.state_json
             FROM counter
             JOIN agent_messages AS source
               ON source.session_id = '{session}'
              AND source.message_id = 'message-corrupt-mailbox-summary';",
            session = session.as_str(),
        ))
        .unwrap();
    drop(connection);

    assert!(matches!(
        runtime.block_on(store.read_agent_mailbox_summary(&session)),
        Err(StoreError::Corrupt(message)) if message.contains("mailbox summary exceeds")
    ));
}

#[test]
fn verify_rejects_a_fabricated_claim_for_a_never_claimed_message() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let session = SessionId::new("session-message-state-corrupt").unwrap();
    let message = MessageId::new("message-state-corrupt").unwrap();
    runtime
        .block_on(store.commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header(session.as_str())),
                facts: Vec::new(),
                controls: vec![accepted_message_control(1, &session, &message)],
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        }))
        .unwrap();
    drop(store);

    let fabricated =
        serde_json::to_string(&rsi_agent_store_protocol::StoreAgentMessageState::Claimed {
            activation_id: rsi_agent_session_protocol::ActivationId::new("fabricated-activation")
                .unwrap(),
            turn_id: TurnId::new("fabricated-turn").unwrap(),
            step_id: rsi_agent_session_protocol::StepId::new("fabricated-step").unwrap(),
            entered_fact_seq: 1,
        })
        .unwrap();
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE agent_messages SET state = 'claimed', state_json = ?1
                 WHERE session_id = ?2 AND message_id = ?3",
                rusqlite::params![fabricated, session.as_str(), message.as_str()],
            )
            .unwrap(),
        1
    );
    drop(connection);

    let error = SqliteStore::verify(root.path()).unwrap_err();
    assert!(
        matches!(&error, StoreError::Corrupt(message) if message.contains("final projection")),
        "unexpected verification error: {error:?}"
    );
}

#[test]
fn indexed_turn_boundary_rejects_fact_json_with_a_different_sequence() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let session = SessionId::new("session-indexed-fact-sequence").unwrap();
    let turn = TurnId::new("turn-1").unwrap();
    runtime
        .block_on(store.append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header(session.as_str())),
            facts: vec![fact(1)],
        }))
        .unwrap();
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let fact_json = connection
        .query_row(
            "SELECT fact_json FROM facts WHERE session_id = ?1 AND seq = 1",
            [session.as_str()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE facts SET fact_json = ?1 WHERE session_id = ?2 AND seq = 1",
                [
                    fact_json.replacen("\"seq\":1", "\"seq\":2", 1),
                    session.to_string()
                ],
            )
            .unwrap(),
        1
    );
    drop(connection);

    let reopened = SqliteStore::open(root.path()).unwrap();
    assert!(matches!(
        runtime.block_on(reopened.read_turn_boundary(&session, &turn)),
        Err(StoreError::Corrupt(message)) if message.contains("indexed Fact")
    ));
}

#[test]
fn malformed_fact_prefix_digest_is_rejected_on_access_and_by_verify() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(store.append(AppendBatch {
            session_id: SessionId::new("session-prefix-corrupt").unwrap(),
            expected_seq: 0,
            header: Some(header("session-prefix-corrupt")),
            facts: vec![fact(1)],
        }))
        .unwrap();
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET fact_prefix_sha256 = 'not-a-digest' WHERE session_id = ?1",
            ["session-prefix-corrupt"],
        )
        .unwrap();
    drop(connection);

    let reopened = SqliteStore::open(root.path()).unwrap();
    assert!(matches!(
        runtime.block_on(reopened.header(&SessionId::new("session-prefix-corrupt").unwrap())),
        Err(StoreError::Corrupt(message)) if message.contains("Fact-prefix digest")
    ));
    drop(reopened);
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(message)) if message.contains("Fact-prefix digest")
    ));
}

#[test]
fn verify_decodes_every_dormant_session_header() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(store.append(AppendBatch {
            session_id: SessionId::new("session-header-corrupt").unwrap(),
            expected_seq: 0,
            header: Some(header("session-header-corrupt")),
            facts: vec![fact(1)],
        }))
        .unwrap();
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET header_json = '{not-json' WHERE session_id = ?1",
            ["session-header-corrupt"],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(message)) if message.contains("session header")
    ));
}

#[test]
fn verify_recomputes_every_canonical_fact_prefix_digest() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let session = SessionId::new("session-fact-corrupt").unwrap();
    runtime
        .block_on(store.append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-fact-corrupt")),
            facts: vec![fact(1)],
        }))
        .unwrap();
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE facts SET fact_json = replace(fact_json, 'text-1', 'tampered')
                 WHERE session_id = ?1 AND seq = 1",
                ["session-fact-corrupt"],
            )
            .unwrap(),
        1
    );
    drop(connection);

    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        runtime
            .block_on(reopened.header(&session))
            .unwrap()
            .session_id(),
        &session
    );
    assert_eq!(
        runtime
            .block_on(reopened.read_facts(&session, 0, 8))
            .unwrap()
            .facts
            .len(),
        1
    );
    drop(reopened);

    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(message)) if message.contains("Fact-prefix digest")
    ));
}

#[test]
fn verify_never_creates_a_missing_store() {
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("missing-agent-store");

    assert!(matches!(
        SqliteStore::verify(&missing),
        Err(StoreError::NotFound(_))
    ));
    assert!(!missing.exists());
}

#[tokio::test]
async fn cas_is_immutable_digest_verified_and_does_not_delete_unowned_files() {
    let root = tempfile::tempdir().unwrap();
    let unrelated = root.path().join("keep-me.txt");
    std::fs::write(&unrelated, b"user-owned").unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let bytes: Arc<[u8]> = Arc::from(&b"immutable"[..]);
    let reference = store.put_cas(bytes.clone()).await.unwrap();
    assert_eq!(store.read_cas(&reference).await.unwrap(), bytes);
    assert_eq!(b"immutable".len(), b"mutated!!".len());
    std::fs::write(
        root.path().join("cas").join(&reference.sha256),
        b"mutated!!",
    )
    .unwrap();
    assert!(matches!(
        store.read_cas(&reference).await,
        Err(StoreError::Corrupt(_))
    ));
    drop(store);
    assert_eq!(std::fs::read(unrelated).unwrap(), b"user-owned");
}

#[test]
fn reopen_removes_only_orphaned_files_from_the_owned_cas_staging_directory() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    drop(store);
    let staging = root.path().join("cas").join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let orphan = staging.join("orphan.tmp");
    std::fs::write(&orphan, b"partial CAS object").unwrap();
    let unowned = root.path().join("cas").join("keep-me");
    std::fs::write(&unowned, b"unowned").unwrap();

    let reopened = SqliteStore::open(root.path()).unwrap();
    assert!(!orphan.exists());
    assert_eq!(std::fs::read(unowned).unwrap(), b"unowned");
    drop(reopened);
}

#[test]
fn writer_lease_is_held_from_open_until_last_clone_drops() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let clone = store.clone();
    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::WriterLocked)
    ));
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::WriterLocked)
    ));
    drop(store);
    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::WriterLocked)
    ));
    drop(clone);
    SqliteStore::verify(root.path()).unwrap();
}

#[cfg(unix)]
#[test]
fn verify_accepts_a_clean_read_only_store_copy() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("store ?#% copy");
    std::fs::create_dir(&root).unwrap();
    let store = SqliteStore::open(&root).unwrap();
    drop(store);
    let database = root.join("sessions.sqlite3");
    let writer_lock = root.join(".writer.lock");
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o400)).unwrap();
    std::fs::set_permissions(&writer_lock, std::fs::Permissions::from_mode(0o400)).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();

    let result = SqliteStore::verify(&root);

    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&writer_lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    result.unwrap();
}

#[test]
fn verify_refuses_a_snapshot_with_an_uncheckpointed_wal() {
    let live_root = tempfile::tempdir().unwrap();
    drop(SqliteStore::open(live_root.path()).unwrap());
    let live_store = SqliteStore::open(live_root.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(live_store.append(AppendBatch {
            session_id: SessionId::new("session-in-wal").unwrap(),
            expected_seq: 0,
            header: Some(header("session-in-wal")),
            facts: vec![fact(1)],
        }))
        .unwrap();
    let wal = live_root.path().join("sessions.sqlite3-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);

    let snapshot = tempfile::tempdir().unwrap();
    for name in ["sessions.sqlite3", "sessions.sqlite3-wal", ".writer.lock"] {
        std::fs::copy(live_root.path().join(name), snapshot.path().join(name)).unwrap();
    }

    assert!(matches!(
        SqliteStore::verify(snapshot.path()),
        Err(StoreError::Invalid(message))
            if message.contains("nonempty WAL") && message.contains("cleanly closed")
    ));
    drop(live_store);
}

#[test]
fn old_or_partial_schema_is_rejected_without_migration() {
    let immediately_previous = AGENT_STORE_SCHEMA_VERSION
        .checked_sub(1)
        .expect("the Store schema starts above version zero");
    for (version, partial) in [(immediately_previous, false), (2, false), (0, true)] {
        let root = tempfile::tempdir().unwrap();
        let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        if partial {
            connection
                .execute_batch("CREATE TABLE legacy (value TEXT) STRICT;")
                .unwrap();
        }
        connection
            .execute_batch(&format!("PRAGMA user_version = {version};"))
            .unwrap();
        drop(connection);
        assert!(matches!(
            SqliteStore::open(root.path()),
            Err(StoreError::SchemaMismatch { actual, .. }) if actual == version
        ));
    }
}

#[test]
fn version_one_with_incompatible_constraints_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                session_id BLOB,
                header_json INTEGER,
                durable_seq TEXT
             );
             CREATE TABLE facts (
                session_id BLOB,
                seq TEXT,
                fact_json INTEGER
             );
             CREATE TABLE cas_objects (
                sha256 BLOB,
                byte_len TEXT
             );
             PRAGMA user_version = 1;",
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::Corrupt(_) | StoreError::SchemaMismatch { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_store_root_is_rejected() {
    use std::os::unix::fs::symlink;
    let parent = tempfile::tempdir().unwrap();
    let real = parent.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = parent.path().join("link");
    symlink(&real, &link).unwrap();
    assert!(matches!(
        SqliteStore::open(link),
        Err(StoreError::Invalid(_))
    ));
}

#[cfg(unix)]
#[test]
fn store_tightens_owned_directories_before_opening_durable_files() {
    use std::os::unix::fs::PermissionsExt as _;

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

    let store = SqliteStore::open(&root).unwrap();
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(root.join("cas"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(store);
}

#[tokio::test]
async fn verification_derives_activation_lineage_from_the_immutable_header() {
    use rsi_agent_session_protocol::{ActivationId, advance_control_prefix_digest};
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("lineage-root").unwrap();
    let start = |root_session_id| {
        AgentControlRecord::new(
            1,
            1,
            AgentControlRecordBody::ActivationStarted {
                activation_id: ActivationId::new("lineage-activation").unwrap(),
                root_session_id,
                parent_session_id: None,
                path: AgentPath::root(),
            },
        )
        .unwrap()
    };
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header(session_id.as_str())),
                facts: Vec::new(),
                controls: vec![start(session_id)],
            }],
            required_active_activations: Vec::new(),
            quiescent_sessions: Vec::new(),
        })
        .await
        .unwrap();
    let database = root.path().join("sessions.sqlite3");
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    let corrupted = start(SessionId::new("fabricated-root").unwrap());
    let connection = Connection::open(database).unwrap();
    connection
        .execute(
            "UPDATE agent_controls SET control_json = ?1",
            [serde_json::to_string(&corrupted).unwrap()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sessions SET control_prefix_sha256 = ?1",
            [hex::encode(
                advance_control_prefix_digest(EMPTY_CONTROL_PREFIX_DIGEST, &corrupted).unwrap(),
            )],
        )
        .unwrap();
    drop(connection);
    assert!(
        matches!(SqliteStore::verify(root.path()), Err(StoreError::Corrupt(message)) if message.contains("lineage"))
    );
}

#[tokio::test]
async fn lazy_session_access_rejects_a_malformed_control_digest() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("bad-control-digest").unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header(session_id.as_str())),
            facts: vec![fact(1)],
        })
        .await
        .unwrap();
    let database = root.path().join("sessions.sqlite3");
    drop(store);
    Connection::open(database)
        .unwrap()
        .execute("UPDATE sessions SET control_prefix_sha256 = 'bad'", [])
        .unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(matches!(
        store.header(&session_id).await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn a_missing_fork_terminal_digest_is_corruption() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let session_id = SessionId::new("missing-terminal-digest").unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header(session_id.as_str())),
            facts: vec![fact(1), terminal_fact(2, 1), fact(3)],
        })
        .await
        .unwrap();
    drop(store);
    Connection::open(root.path().join("sessions.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE turns SET terminal_prefix_sha256 = NULL WHERE terminal_seq IS NOT NULL",
            [],
        )
        .unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(matches!(
        store
            .resolve_fork_boundary(
                &session_id,
                &TurnId::new("turn-3").unwrap(),
                ForkTurnSelection::All
            )
            .await,
        Err(StoreError::Corrupt(_))
    ));
}
