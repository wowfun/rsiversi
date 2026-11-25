use super::{facts, header};
use rsi_agent_session_protocol::{AgentControlRecord, AgentControlRecordBody, SessionId};
use rsi_agent_store_protocol::{
    AtomicAgentCommit, AtomicSessionAppend, MaintenanceOperationId, PurgeRequest, PurgeTarget,
    SessionStore as _, StoreError, StoreMaintenance as _,
};
use rsi_agent_store_sqlite::SqliteStore;
use rusqlite::Connection;
use std::path::Path;

async fn seed(root: &Path) {
    let store = SqliteStore::open(root).unwrap();
    seed_session(&store, "domains").await;
}

async fn seed_session(store: &SqliteStore, name: &str) {
    let [accepted, event, _] = facts();
    rsi_agent_testkit::assert_domain_store_contract(store, header(name), accepted, event).await;
}

#[tokio::test]
async fn sqlite_domains_preserve_atomic_revisions_receipts_and_history() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    seed(&root).await;
    SqliteStore::verify(&root).unwrap();
}

#[tokio::test]
async fn purging_one_session_preserves_matching_domain_keys_in_another() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("store");
    let store = SqliteStore::open(&root).unwrap();
    for name in ["retired", "retained"] {
        seed_session(&store, name).await;
    }
    let retired = header("retired");
    let retained = SessionId::new("retained").unwrap();
    let current = store
        .read_domain_states(&retained, None, None)
        .await
        .unwrap();
    let previous = store
        .read_domain_states(&retained, Some(1), None)
        .await
        .unwrap();
    let [_, _, terminal] = facts();
    let boundary = AgentControlRecord::new(
        3,
        terminal.timestamp_ms(),
        AgentControlRecordBody::TurnBoundaryRecorded {
            turn_id: terminal.body().turn_id().unwrap().clone(),
            terminal_fact_seq: terminal.seq(),
        },
    )
    .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: retired.session_id().clone(),
                expected_fact_seq: 2,
                expected_control_seq: 2,
                header: None,
                facts: vec![terminal.into()],
                controls: vec![boundary],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    let operation_id = MaintenanceOperationId::new("purge-domains").unwrap();
    store
        .begin_purge(PurgeRequest {
            operation_id: operation_id.clone(),
            targets: vec![PurgeTarget {
                session_id: retired.session_id().clone(),
                header_sha256: retired.fingerprint().unwrap(),
                fact_seq: 3,
                control_seq: 3,
            }],
        })
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .purge_batch(operation_id.as_str())
            .await
            .unwrap()
            .complete
    );
    drop(store);
    SqliteStore::verify(&root).unwrap();
    let reopened = SqliteStore::open(&root).unwrap();
    assert!(matches!(
        reopened.header(retired.session_id()).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        reopened
            .read_domain_states(&retained, None, None)
            .await
            .unwrap(),
        current
    );
    assert_eq!(
        reopened
            .read_domain_states(&retained, Some(1), None)
            .await
            .unwrap(),
        previous
    );
}

#[tokio::test]
async fn cold_domain_reads_and_offline_verify_reject_corrupted_canonical_indexes() {
    let mutations = [
        "UPDATE domain_heads SET snapshot_bytes = snapshot_bytes + 1 WHERE domain_id = 'a'",
        "UPDATE domain_heads SET codec_version = 2 WHERE domain_id = 'a'",
        "UPDATE domain_heads SET update_index = 1 WHERE domain_id = 'a'",
        "UPDATE domain_heads SET revision = 3 WHERE domain_id = 'a'",
        "DELETE FROM domain_heads WHERE domain_id = 'a'",
        "UPDATE domain_versions SET snapshot_bytes = snapshot_bytes + 1 WHERE domain_id = 'a' AND control_seq = 1",
        "UPDATE domain_versions SET update_index = 1 WHERE domain_id = 'a' AND control_seq = 1",
        "DELETE FROM domain_versions WHERE domain_id = 'a' AND control_seq = 1",
        "UPDATE domain_requests SET control_seq = 1",
        "UPDATE domain_requests SET request_id = 'renamed'",
        "DELETE FROM domain_requests",
        "INSERT INTO domain_requests VALUES ('domains', 'fabricated', 99, NULL, 1)",
        "UPDATE domain_requests SET source_turn_id = NULL",
        "UPDATE domain_requests SET control_bytes = control_bytes + 1",
        "UPDATE agent_controls SET control_json = json_set(control_json, '$.commit.updates[0].snapshot.state', 'changed') WHERE seq = 2",
        "UPDATE agent_controls SET control_json = json_set(control_json, '$.commit.fact_span.first_seq', 1) WHERE seq = 2",
        "UPDATE domain_heads SET domain_id = printf('%0500000d', 1) WHERE domain_id = 'a'",
    ];
    for mutation in mutations {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("store");
        seed(&root).await;
        let connection = Connection::open(root.join("sessions.sqlite3")).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection.execute_batch(mutation).unwrap();
        drop(connection);
        let reopened = SqliteStore::open(&root).unwrap();
        assert!(
            matches!(
                reopened
                    .read_domain_states(&SessionId::new("domains").unwrap(), None, None)
                    .await,
                Err(StoreError::Corrupt(_))
            ),
            "cold read accepted {mutation}"
        );
        drop(reopened);
        assert!(
            matches!(SqliteStore::verify(&root), Err(StoreError::Corrupt(_))),
            "offline verify accepted {mutation}"
        );
    }
}
