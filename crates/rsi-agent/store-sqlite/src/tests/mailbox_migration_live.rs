use super::*;

#[tokio::test]
#[ignore = "requires an explicitly supplied, cleanly closed schema-44 database; copies it before use"]
async fn preserves_live_history_while_removing_the_mailbox_body_copy() {
    let source =
        PathBuf::from(std::env::var_os("RSI_STORE_SCHEMA44_FIXTURE").expect("fixture database"));
    let root = private_tempdir();
    let database = root.path().join("sessions.sqlite3");
    fs::copy(&source, &database).unwrap();
    let connection = Connection::open(&database).unwrap();
    assert_eq!(pragma_user_version(&connection).unwrap(), 44);
    let canonical = |connection: &Connection| {
        let mut hash = Sha256::new();
        let mut counts = Vec::new();
        for sql in [
            "SELECT header_json FROM sessions ORDER BY session_id",
            "SELECT fact_json FROM facts ORDER BY session_id,seq",
            "SELECT control_json FROM agent_controls ORDER BY session_id,seq",
        ] {
            let mut count = 0;
            for row in connection
                .prepare(sql)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
            {
                let row = row.unwrap();
                hash.update(row.len().to_le_bytes());
                hash.update(row);
                count += 1;
            }
            counts.push(count);
        }
        (hex::encode(hash.finalize()), counts)
    };
    let before = canonical(&connection);
    let removed_bytes: i64 = connection
        .query_row(
            "SELECT coalesce(sum(length(CAST(message_json AS BLOB))),0) FROM agent_messages",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let messages: Vec<(SessionId,AgentMessage)> = connection.prepare(
        "SELECT m.session_id,c.control_json FROM agent_messages AS m JOIN agent_controls AS c ON c.session_id=m.session_id AND c.seq=m.accepted_control_seq"
    ).unwrap().query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).unwrap()
        .map(|row| {let (id,json)=row.unwrap();let control: AgentControlRecord=serde_json::from_str(&json).unwrap();
            let AgentControlRecordBody::MessageAccepted { message,.. }=control.into_body() else {panic!("acceptance");};
            (SessionId::new(id).unwrap(),message)
        }).collect();
    assert!(!messages.is_empty());
    drop(connection);
    for _ in 0..2 {
        let store = SqliteStore::open(root.path()).unwrap();
        for (id, message) in &messages {
            assert_eq!(
                &store
                    .read_agent_message(id, &message.message_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .message,
                message
            );
        }
        drop(store);
        SqliteStore::verify(root.path()).unwrap();
    }
    let connection = Connection::open(&database).unwrap();
    assert_eq!(canonical(&connection), before);
    assert_eq!(
        pragma_user_version(&connection).unwrap(),
        AGENT_STORE_SCHEMA_VERSION
    );
    println!(
        "{{\"sessions\":{},\"facts\":{},\"controls\":{},\"messages\":{},\"removedMessageBodyBytes\":{},\"canonicalSha256\":\"{}\"}}",
        before.1[0],
        before.1[1],
        before.1[2],
        messages.len(),
        removed_bytes,
        before.0
    );
}
