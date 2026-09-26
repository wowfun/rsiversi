use super::*;
use rsi_agent_testkit::assert_program_store_contract;
async fn program_contract(
    store: &dyn SessionStore,
) -> (SessionId, rsi_agent_session_protocol::ProgramRunId) {
    assert_program_store_contract(store, &test_header("template"), &test_fact(1)).await
}
#[tokio::test]
async fn program_records_share_memory_sqlite_bounds_and_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let (id, run) = program_contract(&store).await;
    let before = store.read_program_records(&id, &run).await.unwrap();
    drop(store);
    SqliteStore::verify(root.path()).unwrap();
    let reopened = SqliteStore::open(root.path()).unwrap();
    assert_eq!(
        reopened.read_program_records(&id, &run).await.unwrap(),
        before
    );
}
#[tokio::test]
async fn program_index_corruption_is_rejected_before_selected_record_read() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let (id, run) = program_contract(&store).await;
    let first = store
        .read_program_records(&id, &run)
        .await
        .unwrap()
        .unwrap()
        .head
        .first_control_seq;
    drop(store);
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    assert_eq!(connection.execute("UPDATE program_records SET encoded_bytes=encoded_bytes+1 WHERE session_id=?1 AND control_seq=?2",params![id.as_str(), i64::try_from(first).unwrap()]).unwrap(), 1);
    drop(connection);
    assert!(SqliteStore::verify(root.path()).is_err());
    let store = SqliteStore::open(root.path()).unwrap();
    assert!(store.read_program_records(&id, &run).await.is_err());
}

#[tokio::test]
async fn program_notice_graph_rejects_coordinated_control_and_mailbox_corruption() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let (id, _) = program_contract(&store).await;
    drop(store);
    let connection = Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let mut records: Vec<AgentControlRecord> = connection
        .prepare("SELECT control_json FROM agent_controls WHERE session_id=?1 ORDER BY seq")
        .unwrap()
        .query_map([id.as_str()], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect();
    let notice = records.last_mut().unwrap();
    let mut value = serde_json::to_value(&*notice).unwrap();
    value["message"]["source"]["source"]["run_id"] = serde_json::json!("missing-run");
    *notice = serde_json::from_value(value).unwrap();
    let AgentControlRecordBody::MessageAccepted { message, .. } = notice.body() else {
        panic!("notice");
    };
    connection
        .execute(
            "UPDATE agent_controls SET control_json=?1 WHERE session_id=?2 AND seq=?3",
            params![
                serde_json::to_string(notice).unwrap(),
                id.as_str(),
                i64::try_from(notice.seq()).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE agent_messages SET message_json=?1 WHERE session_id=?2 AND message_id=?3",
            params![
                serde_json::to_string(message).unwrap(),
                id.as_str(),
                message.message_id.as_str()
            ],
        )
        .unwrap();
    let digest = records
        .iter()
        .fold(EMPTY_CONTROL_PREFIX_DIGEST, |digest, record| {
            advance_control_prefix_digest(digest, record).unwrap()
        });
    connection
        .execute(
            "UPDATE sessions SET control_prefix_sha256=?1 WHERE session_id=?2",
            params![hex::encode(digest), id.as_str()],
        )
        .unwrap();
    validation::validate_canonical_control_prefix(&connection, &id).unwrap();
    drop(connection);
    let error = SqliteStore::verify(root.path()).unwrap_err().to_string();
    assert!(error.contains("Program graph"), "{error}");
    let reopened = SqliteStore::open(root.path()).unwrap();
    assert!(reopened.prepare_session(&id).await.is_err());
}

#[tokio::test]
async fn activation_counterpart_lookup_work_is_independent_of_control_history() {
    use rsi_agent_session_protocol::ProgramRunId;
    use rsi_agent_store_protocol::{ProgramGraphQuery, ProgramGraphRead};
    use rusqlite::StatementStatus;
    let mut work = Vec::new();
    for history in [1, 5000] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(root.path()).unwrap();
        let header = test_header("counterpart-history");
        let id = header.session_id().clone();
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: 0,
                header: Some(header),
                facts: vec![test_fact(1).into()],
            })
            .await
            .unwrap();
        let mut connection = store.inner.connections.writer.lock().unwrap();
        let tx = connection.transaction().unwrap();
        for seq in 1..=history {
            let record = AgentControlRecord::new(
                seq,
                1,
                AgentControlRecordBody::ProgramCompletionReserved {
                    activation_id: ActivationId::new(format!("activation-{seq}")).unwrap(),
                    run_id: ProgramRunId::new("run").unwrap(),
                    ordinal: 1,
                },
            )
            .unwrap();
            tx.execute(
                "INSERT INTO agent_controls(session_id,seq,control_json) VALUES(?1,?2,?3)",
                params![
                    id.as_str(),
                    i64::try_from(seq).unwrap(),
                    serde_json::to_string(&record).unwrap()
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let activation = ActivationId::new(format!("activation-{history}")).unwrap();
        assert_eq!(
            crate::program_graph::Graph(&connection)
                .control(&id, ProgramGraphQuery::Sink(&activation))
                .unwrap()
                .seq(),
            history
        );
        let mut query = connection
            .prepare(crate::program_graph::ACTIVATION_COUNTERPART_SQL)
            .unwrap();
        let results = query
            .query_map(
                params![
                    id.as_str(),
                    None::<String>,
                    "program_completion_reserved",
                    None::<String>,
                    None::<u32>,
                    activation.as_str(),
                    i64::try_from(MAXIMUM_SESSION_FACT_BYTES).unwrap()
                ],
                |row| row.get::<_, String>(1),
            )
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(results.len(), 1);
        work.push(query.get_status(StatementStatus::VmStep));
        assert_eq!(query.get_status(StatementStatus::FullscanStep), 0);
    }
    eprintln!("activation counterpart controls=1/5000 vm_steps={work:?}");
    assert!(
        work[1] <= work[0] + 8,
        "counterpart lookup must seek exact activation: {work:?}"
    );
}
