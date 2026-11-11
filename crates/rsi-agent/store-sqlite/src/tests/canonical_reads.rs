use super::*;
use rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES;

async fn damaged_store(sql: &str) -> (tempfile::TempDir, SqliteStore, SessionId) {
    let root = private_tempdir();
    let header = test_header("canonical-pages");
    let store = SqliteStore::open(root.path()).unwrap();
    rsi_agent_testkit::assert_control_fact_order(&store, header.clone()).await;
    drop(store);
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection.execute_batch(sql).unwrap();
    drop(connection);
    let store = SqliteStore::open(root.path()).unwrap();
    (root, store, header.session_id().clone())
}

#[tokio::test]
async fn canonical_pages_ignore_unused_indexes_but_execution_refuses_them() {
    let (_root, store, id) = damaged_store("DELETE FROM turns").await;
    let bytes = MAXIMUM_STORE_FACT_PAGE_BYTES;
    assert_eq!(
        store
            .read_facts(&id, 0, 2, bytes)
            .await
            .unwrap()
            .facts
            .len(),
        2
    );
    assert_eq!(
        store
            .read_controls(&id, 0, 3, bytes)
            .await
            .unwrap()
            .fact_cursors,
        [1, 1, 2]
    );
    assert_eq!(
        store
            .read_facts_before(&id, 0, 2, bytes)
            .await
            .unwrap()
            .facts
            .len(),
        2
    );
    assert_eq!(
        store
            .read_fact_window(&id, 0, 2, bytes)
            .await
            .unwrap()
            .through_seq,
        2
    );
    assert_eq!(
        store
            .read_fact_suffix(&id, 2, bytes)
            .await
            .unwrap()
            .facts
            .len(),
        2
    );
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
    assert!(matches!(
        store.prepare_session(&id).await,
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        store.list_open_turns(&id, 0, 1).await,
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        store
            .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
                sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                    session_id: id,
                    expected_fact_seq: 2,
                    expected_control_seq: 3,
                    header: None,
                    facts: vec![test_fact(3).into()],
                    controls: vec![],
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn canonical_pages_decode_only_selected_history() {
    let (_root, store, id) = damaged_store(
        "UPDATE facts SET fact_json = json_set(fact_json, '$.timestamp_ms', 'broken') WHERE seq = 2;
         UPDATE agent_controls SET control_json = json_set(control_json, '$.timestamp_ms', 'broken') WHERE seq = 3;"
    ).await;
    let bytes = MAXIMUM_STORE_FACT_PAGE_BYTES;
    assert_eq!(
        store.read_facts(&id, 0, 1, bytes).await.unwrap().facts[0].seq(),
        1
    );
    assert_eq!(
        store
            .read_controls(&id, 0, 2, bytes)
            .await
            .unwrap()
            .records
            .len(),
        2
    );
    assert!(matches!(
        store.read_facts(&id, 1, 1, bytes).await,
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        store.read_controls(&id, 2, 1, bytes).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn canonical_control_pages_check_physical_coordinates_and_previous_anchor() {
    for sql in [
        "DELETE FROM agent_controls WHERE seq = 3; UPDATE agent_controls SET seq = 3 WHERE seq = 2",
        "UPDATE agent_controls SET fact_seq = 0 WHERE seq = 2",
        "DELETE FROM agent_controls WHERE seq = 1",
    ] {
        let (_root, store, id) = damaged_store(sql).await;
        assert!(
            matches!(
                store
                    .read_controls(&id, 1, 1, MAXIMUM_STORE_FACT_PAGE_BYTES)
                    .await,
                Err(StoreError::Corrupt(_))
            ),
            "accepted {sql}"
        );
        assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn canonical_pages_reject_missing_selected_coordinates() {
    let (_root, store, id) =
        damaged_store("DELETE FROM facts WHERE seq = 2; DELETE FROM agent_controls WHERE seq = 3")
            .await;
    let bytes = MAXIMUM_STORE_FACT_PAGE_BYTES;
    for after in [0, 1] {
        assert!(matches!(
            store.read_facts(&id, after, 3, bytes).await,
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            store.read_fact_window(&id, after, 3, bytes).await,
            Err(StoreError::Corrupt(_))
        ));
    }
    for after in [0, 2] {
        assert!(matches!(
            store.read_controls(&id, after, 4, bytes).await,
            Err(StoreError::Corrupt(_))
        ));
    }
    assert!(matches!(
        store.read_facts_before(&id, 0, 3, bytes).await,
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        store.read_fact_suffix(&id, 3, bytes).await,
        Err(StoreError::Corrupt(_))
    ));
    assert_eq!(store.inner.validation_runs.load(Ordering::Relaxed), 0);
}
