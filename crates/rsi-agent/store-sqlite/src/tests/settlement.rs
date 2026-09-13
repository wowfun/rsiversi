use super::*;

pub(super) fn append(id: &SessionId, previous: u64) -> AtomicSessionAppend {
    let activation_id = ActivationId::new(format!("activation-{previous}")).unwrap();
    AtomicSessionAppend {
        session_id: id.clone(),
        expected_fact_seq: 1,
        expected_control_seq: previous,
        header: None,
        facts: Vec::new(),
        controls: vec![
            AgentControlRecord::new(
                previous + 1,
                1,
                AgentControlRecordBody::ActivationStarted {
                    activation_id: activation_id.clone(),
                    parent_session_id: None,
                    root_session_id: id.clone(),
                    path: rsi_agent_session_protocol::AgentPath::root(),
                },
            )
            .unwrap(),
            AgentControlRecord::new(
                previous + 2,
                1,
                AgentControlRecordBody::ActivationSettled {
                    activation_id,
                    outcome: rsi_agent_session_protocol::ActivationOutcome::Completed,
                },
            )
            .unwrap(),
        ],
    }
}
pub(super) fn commit(sessions: Vec<AtomicSessionAppend>) -> AtomicAgentCommit {
    AtomicAgentCommit {
        sessions,
        required_active_activations: Vec::new(),
        quiescent_descendants_of: None,
    }
}

#[tokio::test]
async fn settlement_projection_is_atomic_persistent_and_validated() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let id = seed_session(&store, "settlement").await;
    let other = seed_session(&store, "conflict").await;
    assert_eq!(
        store
            .read_agent_subtree_snapshot(&id)
            .await
            .unwrap()
            .session
            .last_settled_control_seq,
        0
    );
    store
        .commit_agent(commit(vec![append(&id, 0)]))
        .await
        .unwrap();
    let captured = store.read_agent_subtree_snapshot(&id).await.unwrap();
    assert_eq!(captured.session.last_settled_control_seq, 2);
    let mut conflict = append(&other, 0);
    conflict.expected_fact_seq = 0;
    assert!(matches!(
        store
            .commit_agent(commit(vec![append(&id, 2), conflict]))
            .await,
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        store.read_agent_subtree_snapshot(&id).await.unwrap(),
        captured,
        "rollback must not publish the derived sequence"
    );
    let reader = Connection::open_with_flags(
        root.path().join("sessions.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        session_store::read_agent_subtree(&reader, &id).unwrap(),
        captured
    );
    store
        .commit_agent(commit(vec![append(&id, 2)]))
        .await
        .unwrap();
    assert_eq!(
        session_store::read_agent_subtree(&reader, &id).unwrap(),
        captured,
        "an active WAL reader must keep both watermarks at its captured horizon"
    );
    reader.execute_batch("COMMIT").unwrap();
    let fresh = session_store::read_agent_subtree(&reader, &id).unwrap();
    assert_eq!(fresh.session.durable_control_seq, 4);
    assert_eq!(fresh.session.last_settled_control_seq, 4);
    drop(reader);
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    let mut current = store.read_agent_subtree_snapshot(&id).await.unwrap();
    assert_eq!(current.session.last_settled_control_seq, 4);
    current.session.last_settled_control_seq = current.session.durable_control_seq + 1;
    assert!(matches!(current.validate(), Err(StoreError::Corrupt(_))));
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET last_settled_control_seq = 0 WHERE session_id = ?1",
            [id.as_str()],
        )
        .unwrap();
    drop(connection);
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(
        matches!(store.read_agent_subtree_snapshot(&id).await, Err(StoreError::Corrupt(message)) if message.contains("last settlement"))
    );
    drop(store);
    assert!(matches!(
        SqliteStore::verify(root.path()),
        Err(StoreError::Corrupt(_))
    ));
}
