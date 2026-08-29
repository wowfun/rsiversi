use rsi_agent_session_protocol::{
    FrozenAgentProfile, MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_SESSION_HEADER_BYTES, SessionFact,
    SessionFactBody, SessionHeader, SessionId, TurnId,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore, StoreError};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_agent_testkit::assert_mechanical_store_contract;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use rusqlite::Connection;
use std::sync::Arc;

fn header(session: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session).unwrap(),
        1,
        "/workspace",
        FrozenAgentProfile::new(
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
}

#[tokio::test]
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
}

#[test]
fn reopen_rejects_a_turn_index_that_disagrees_with_canonical_facts() {
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
    drop(store);

    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE facts SET turn_id = 'turn-tampered' WHERE session_id = ?1 AND seq = 1",
            ["session-index-corrupt"],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::Corrupt(_))
    ));
}

#[tokio::test]
async fn cas_is_immutable_verified_and_does_not_delete_unowned_files() {
    let root = tempfile::tempdir().unwrap();
    let unrelated = root.path().join("keep-me.txt");
    std::fs::write(&unrelated, b"user-owned").unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let bytes: Arc<[u8]> = Arc::from(&b"immutable"[..]);
    let reference = store.put_cas(bytes.clone()).await.unwrap();
    assert_eq!(store.read_cas(&reference).await.unwrap(), bytes);
    std::fs::write(root.path().join("cas").join(&reference.sha256), b"corrupt").unwrap();
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
    drop(store);
    assert!(matches!(
        SqliteStore::open(root.path()),
        Err(StoreError::WriterLocked)
    ));
    drop(clone);
    SqliteStore::open(root.path()).unwrap();
}

#[test]
fn old_or_partial_schema_is_rejected_without_migration() {
    for (version, partial) in [(2, false), (0, true)] {
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
