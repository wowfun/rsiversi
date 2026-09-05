use super::*;

pub(super) fn initialize_or_validate_schema(
    connection: &mut Connection,
    may_initialize: bool,
) -> Result<()> {
    let version = pragma_user_version(connection)?;
    let tables = user_tables(connection)?;
    if version == 0 && tables.is_empty() && may_initialize {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let mut schema = EXPECTED_TABLES
            .iter()
            .map(|(_, sql)| *sql)
            .collect::<Vec<_>>()
            .join(";\n");
        for (_, sql) in EXPECTED_INDEXES {
            schema.push_str(";\n");
            schema.push_str(sql);
        }
        write!(
            &mut schema,
            ";\nPRAGMA user_version = {AGENT_STORE_SCHEMA_VERSION};"
        )
        .expect("writing to a String cannot fail");
        transaction.execute_batch(&schema).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
    } else if version != AGENT_STORE_SCHEMA_VERSION {
        return Err(StoreError::SchemaMismatch {
            expected: AGENT_STORE_SCHEMA_VERSION,
            actual: version,
        });
    }
    validate_schema_shape(connection)
}

pub(super) fn validate_schema_shape(connection: &Connection) -> Result<()> {
    let expected = BTreeSet::from([
        "active_activations".to_owned(),
        "agent_controls".to_owned(),
        "agent_messages".to_owned(),
        "agent_nodes".to_owned(),
        "cas_objects".to_owned(),
        "context_checkpoints".to_owned(),
        "facts".to_owned(),
        "ready_messages".to_owned(),
        "sessions".to_owned(),
        "turns".to_owned(),
    ]);
    let actual = user_tables(connection)?;
    if actual != expected {
        return Err(StoreError::SchemaMismatch {
            expected: AGENT_STORE_SCHEMA_VERSION,
            actual: pragma_user_version(connection)?,
        });
    }
    for (table, expected_sql) in EXPECTED_TABLES {
        let observed_sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?;
        if normalize_schema_sql(&observed_sql) != normalize_schema_sql(expected_sql) {
            return Err(StoreError::Corrupt(format!(
                "SQLite table `{table}` does not match the exact schema"
            )));
        }
    }
    let expected_indexes = EXPECTED_INDEXES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    if user_indexes(connection)? != expected_indexes {
        return Err(StoreError::Corrupt(
            "SQLite schema contains missing or unexpected indexes".into(),
        ));
    }
    for (index, expected_sql) in EXPECTED_INDEXES {
        let observed_sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?;
        if normalize_schema_sql(&observed_sql) != normalize_schema_sql(expected_sql) {
            return Err(StoreError::Corrupt(format!(
                "SQLite index `{index}` does not match the exact schema"
            )));
        }
    }
    let unexpected_triggers_or_views = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('trigger', 'view')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql_error)?;
    if unexpected_triggers_or_views != 0 {
        return Err(StoreError::Corrupt(
            "SQLite schema contains unexpected triggers or views".into(),
        ));
    }
    Ok(())
}

pub(super) fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn pragma_user_version(connection: &Connection) -> Result<u32> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error)?;
    u32::try_from(version).map_err(|_| StoreError::Corrupt("negative user_version".into()))
}

pub(super) fn user_tables(connection: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(sql_error)?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(sql_error)
}

pub(super) fn user_indexes(connection: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(sql_error)?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(sql_error)
}

pub(super) fn validate_session(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<SessionHeader> {
    let (header, durable_seq) = read_session_header_row(connection, session_id)?;

    let (fact_count, maximum_sequence) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(seq), 0)
             FROM facts WHERE session_id = ?1",
            [session_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(sql_error)?;
    if decode_u64("session Fact count", fact_count)? != durable_seq
        || decode_u64("session maximum Fact sequence", maximum_sequence)? != durable_seq
    {
        return Err(StoreError::Corrupt(
            "session durable watermark differs from its contiguous Fact stream".into(),
        ));
    }

    let control_seq = connection
        .query_row(
            "SELECT control_seq FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql_error)
        .and_then(|value| decode_u64("control sequence", value))?;
    let (control_count, maximum_control_sequence) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(seq), 0)
             FROM agent_controls WHERE session_id = ?1",
            [session_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(sql_error)?;
    if decode_u64("session control count", control_count)? != control_seq
        || decode_u64("session maximum control sequence", maximum_control_sequence)? != control_seq
    {
        return Err(StoreError::Corrupt(
            "session control watermark differs from its contiguous control stream".into(),
        ));
    }

    validate_turn_index(connection, session_id)?;
    let node = connection
        .query_row(
            "SELECT root_session_id, parent_session_id, path_json, task_name
             FROM agent_nodes WHERE session_id = ?1",
            [session_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    match (header.fork_origin(), node) {
        (None, None) => {}
        (Some(origin), Some((root, parent, path, task_name)))
            if root == origin.root_session_id.as_str()
                && parent == origin.parent_session_id.as_str()
                && decode_json::<rsi_agent_session_protocol::AgentPath>("Agent path", &path)?
                    == origin.path
                && task_name == origin.task_name => {}
        _ => {
            return Err(StoreError::Corrupt(
                "Agent node index disagrees with immutable Header lineage".into(),
            ));
        }
    }
    if let Some(origin) = header.fork_origin()
        && derived_session_root(connection, &origin.parent_session_id)? != origin.root_session_id
    {
        return Err(StoreError::Corrupt(
            "Agent child root differs from its parent's durable root".into(),
        ));
    }
    let active_parent = connection
        .query_row(
            "SELECT parent_session_id FROM active_activations WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(parent) = active_parent
        && parent.as_deref()
            != header
                .fork_origin()
                .map(|origin| origin.parent_session_id.as_str())
    {
        return Err(StoreError::Corrupt(
            "active activation parent disagrees with Header lineage".into(),
        ));
    }
    Ok(header)
}

pub(super) fn read_session_header_row(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<(SessionHeader, u64)> {
    let (
        created_at_ms,
        durable_seq,
        fact_prefix_sha256,
        header_encoded_len,
        header_json,
        control_prefix_sha256,
    ) = connection
        .query_row(
            "SELECT created_at_ms, durable_seq, fact_prefix_sha256,
                    length(CAST(header_json AS BLOB)),
                    CASE WHEN length(CAST(header_json AS BLOB)) <= ?2
                         THEN header_json END, control_prefix_sha256
             FROM sessions WHERE session_id = ?1",
            params![
                session_id.as_str(),
                i64::try_from(MAXIMUM_SESSION_HEADER_BYTES)
                    .expect("session header bound fits SQLite INTEGER"),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
    let durable_seq = decode_u64("durable sequence", durable_seq)?;
    validate_sha256("Fact-prefix digest", &fact_prefix_sha256)?;
    validate_sha256("Control-prefix digest", &control_prefix_sha256)?;
    let header: SessionHeader = decode_projected_json(
        "session header",
        (header_encoded_len, header_json),
        MAXIMUM_SESSION_HEADER_BYTES,
    )?;
    if header.session_id() != session_id {
        return Err(StoreError::Corrupt(
            "session header identity differs from its durable row".into(),
        ));
    }
    if header.created_at_ms() != decode_u64("session creation timestamp", created_at_ms)? {
        return Err(StoreError::Corrupt(
            "session creation timestamp differs from its durable header".into(),
        ));
    }
    Ok((header, durable_seq))
}

pub(super) fn validate_turn_index(connection: &Connection, session_id: &SessionId) -> Result<()> {
    let invalid = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1
               FROM facts AS fact
               LEFT JOIN turns AS turn
                 ON turn.session_id = fact.session_id AND turn.turn_id = fact.turn_id
               WHERE fact.session_id = ?1 AND (
                    turn.turn_id IS NULL
                    OR (fact.fact_kind = 'accepted' AND turn.accepted_seq != fact.seq)
                    OR (fact.fact_kind = 'terminal' AND turn.terminal_seq != fact.seq)
                    OR (fact.fact_kind = 'event' AND (
                         fact.seq <= turn.accepted_seq
                         OR (turn.terminal_seq IS NOT NULL AND fact.seq >= turn.terminal_seq)
                       ))
                  )
               UNION ALL
               SELECT 1
               FROM turns AS turn
               WHERE turn.session_id = ?1 AND (
                    (turn.terminal_seq IS NULL AND turn.terminal_prefix_sha256 IS NOT NULL)
                    OR (turn.terminal_seq IS NOT NULL AND
                        (turn.terminal_prefix_sha256 IS NULL
                         OR length(turn.terminal_prefix_sha256) != 64))
                    OR
                    NOT EXISTS (
                      SELECT 1 FROM facts AS accepted
                      WHERE accepted.session_id = turn.session_id
                        AND accepted.seq = turn.accepted_seq
                        AND accepted.turn_id = turn.turn_id
                        AND accepted.fact_kind = 'accepted'
                    )
                    OR (turn.terminal_seq IS NOT NULL AND NOT EXISTS (
                      SELECT 1 FROM facts AS terminal
                      WHERE terminal.session_id = turn.session_id
                        AND terminal.seq = turn.terminal_seq
                        AND terminal.turn_id = turn.turn_id
                        AND terminal.fact_kind = 'terminal'
                    ))
                  )
             )",
            [session_id.as_str()],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if invalid {
        return Err(StoreError::Corrupt(
            "turn index differs from the canonical Fact stream".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_database(connection: &Connection) -> Result<()> {
    let integrity = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(sql_error)?;
    if integrity != "ok" {
        return Err(StoreError::Corrupt(format!(
            "SQLite integrity_check returned {integrity:?}"
        )));
    }
    let foreign_key_failure = {
        let mut statement = connection
            .prepare("PRAGMA foreign_key_check")
            .map_err(sql_error)?;
        let mut rows = statement.query([]).map_err(sql_error)?;
        rows.next().map_err(sql_error)?.is_some()
    };
    if foreign_key_failure {
        return Err(StoreError::Corrupt(
            "SQLite foreign_key_check reported a violation".into(),
        ));
    }
    let oversized_agent_tree = connection
        .query_row(
            "SELECT 1 FROM agent_nodes
             GROUP BY root_session_id HAVING COUNT(*) >= ?1 LIMIT 1",
            [i64::try_from(MAXIMUM_DURABLE_AGENT_TREE_NODES)
                .expect("durable Agent-tree bound fits SQLite INTEGER")],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql_error)?
        .is_some();
    if oversized_agent_tree {
        return Err(StoreError::Corrupt(
            "Agent tree exceeds its durable node bound".into(),
        ));
    }
    let session_ids = {
        let mut statement = connection
            .prepare("SELECT session_id FROM sessions ORDER BY session_id")
            .map_err(sql_error)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sql_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_error)?
    };
    for encoded_session_id in session_ids {
        let session_id = SessionId::new(encoded_session_id).map_err(|error| {
            StoreError::Corrupt(format!("durable session identity is invalid: {error}"))
        })?;
        validate_session(connection, &session_id)?;
        validate_canonical_fact_prefix(connection, &session_id)?;
        validate_canonical_control_prefix(connection, &session_id)?;
    }
    validate_agent_message_index(connection)?;
    validate_ready_index(connection)?;
    validate_active_activation_index(connection)?;
    Ok(())
}

#[allow(clippy::too_many_lines)] // One streaming verifier keeps canonical control order beside each indexed-row comparison.
pub(super) fn validate_agent_message_index(connection: &Connection) -> Result<()> {
    let mut accepted_messages = 0_u64;
    let mut expected_messages = BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT session_id, length(CAST(control_json AS BLOB)),
                    CASE WHEN length(CAST(control_json AS BLOB)) <= ?1
                         THEN control_json END
             FROM agent_controls ORDER BY session_id, seq",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map(
            [i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                .expect("Agent control bound fits SQLite INTEGER")],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(sql_error)?;
    for row in rows {
        let (encoded_session_id, length, json) = row.map_err(sql_error)?;
        let session_id = SessionId::new(encoded_session_id)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let record: AgentControlRecord = decode_projected_json(
            "Agent control record",
            (length, json),
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        match record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                root_session_id,
                target,
                wake_required,
            } => {
                if derived_session_root(connection, &session_id)? != *root_session_id {
                    return Err(StoreError::Corrupt(
                        "canonical Agent message names a foreign root".into(),
                    ));
                }
                accepted_messages = accepted_messages.checked_add(1).ok_or_else(|| {
                    StoreError::Corrupt("canonical mailbox count overflowed".into())
                })?;
                if expected_messages
                    .insert(
                        (session_id.clone(), message.message_id.clone()),
                        StoreAgentMessage {
                            message: message.clone(),
                            encoded_message_bytes: serde_json::to_vec(message)
                                .map_err(|error| StoreError::Corrupt(error.to_string()))?
                                .len(),
                            root_session_id: root_session_id.clone(),
                            target: *target,
                            wake_required: *wake_required,
                            accepted_control_seq: record.seq(),
                            state: StoreAgentMessageState::Pending,
                        },
                    )
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "canonical controls repeat a mailbox message identity".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageClaimed {
                message_id,
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            } => {
                let expected = expected_messages
                    .get_mut(&(session_id.clone(), message_id.clone()))
                    .ok_or_else(|| {
                        StoreError::Corrupt("canonical claim has no accepted message".into())
                    })?;
                if !matches!(expected.state, StoreAgentMessageState::Pending) {
                    return Err(StoreError::Corrupt(
                        "canonical claim references a non-pending message".into(),
                    ));
                }
                expected.state = StoreAgentMessageState::Claimed {
                    activation_id: activation_id.clone(),
                    turn_id: turn_id.clone(),
                    step_id: step_id.clone(),
                    entered_fact_seq: *entered_fact_seq,
                };
            }
            AgentControlRecordBody::MessagePromoted { message_id } => {
                let expected = expected_messages
                    .get_mut(&(session_id.clone(), message_id.clone()))
                    .ok_or_else(|| {
                        StoreError::Corrupt("canonical promotion has no accepted message".into())
                    })?;
                if !matches!(expected.state, StoreAgentMessageState::Pending)
                    || expected.target != MessageTarget::NextStep
                    || expected.wake_required
                    || !matches!(
                        expected.message.source,
                        AgentMessageSource::Completion { .. }
                    )
                {
                    return Err(StoreError::Corrupt(
                        "canonical promotion requires pending non-waking next-Step completion"
                            .into(),
                    ));
                }
                expected.target = MessageTarget::NextTurn;
                expected.wake_required = true;
            }
            AgentControlRecordBody::MessageDiscarded { message_id, reason } => {
                let expected = expected_messages
                    .get_mut(&(session_id.clone(), message_id.clone()))
                    .ok_or_else(|| {
                        StoreError::Corrupt("canonical discard has no accepted message".into())
                    })?;
                if !matches!(expected.state, StoreAgentMessageState::Pending) {
                    return Err(StoreError::Corrupt(
                        "canonical discard references a non-pending message".into(),
                    ));
                }
                expected.state = StoreAgentMessageState::Discarded {
                    reason: *reason,
                    control_seq: record.seq(),
                };
            }
            AgentControlRecordBody::ActivationStarted { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::ActivationSettled { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::CompletionReserved { .. } => {}
        }
    }
    drop(statement);
    for ((session_id, message_id), expected) in expected_messages {
        if read_indexed_agent_message(connection, &session_id, &message_id)? != expected {
            return Err(StoreError::Corrupt(
                "mailbox index final projection differs from canonical controls".into(),
            ));
        }
    }
    let indexed_messages = connection
        .query_row("SELECT COUNT(*) FROM agent_messages", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(sql_error)
        .and_then(|count| decode_u64("indexed mailbox count", count))?;
    if indexed_messages != accepted_messages {
        return Err(StoreError::Corrupt(
            "mailbox index cardinality differs from canonical controls".into(),
        ));
    }
    Ok(())
}

pub(super) fn read_indexed_agent_message(
    connection: &Connection,
    session_id: &SessionId,
    message_id: &MessageId,
) -> Result<StoreAgentMessage> {
    connection
        .query_row(
            "SELECT length(CAST(message_json AS BLOB)),
                    CASE WHEN length(CAST(message_json AS BLOB)) <= ?3 THEN message_json END,
                    message_source, root_session_id, target, wake_required,
                    accepted_control_seq, state,
                    length(CAST(state_json AS BLOB)),
                    CASE WHEN length(CAST(state_json AS BLOB)) <= ?4 THEN state_json END
             FROM agent_messages WHERE session_id = ?1 AND message_id = ?2",
            params![
                session_id.as_str(),
                message_id.as_str(),
                i64::try_from(MAXIMUM_STORE_MAILBOX_PAGE_BYTES)
                    .expect("mailbox page bound fits SQLite INTEGER"),
                i64::try_from(MAXIMUM_INDEXED_MESSAGE_STATE_BYTES)
                    .expect("message state bound fits SQLite INTEGER"),
            ],
            indexed_message_row,
        )
        .optional()
        .map_err(sql_error)?
        .map(decode_indexed_message)
        .transpose()?
        .ok_or_else(|| {
            StoreError::Corrupt("canonical mailbox control has no indexed message".into())
        })
}

#[allow(clippy::too_many_lines)] // Offline verification keeps every activation transition in one ordered projection scan.
pub(super) fn validate_active_activation_index(connection: &Connection) -> Result<()> {
    let mut expected = BTreeMap::<SessionId, StoreActiveActivation>::new();
    let mut statement = connection
        .prepare(
            "SELECT session_id, length(CAST(control_json AS BLOB)),
                    CASE WHEN length(CAST(control_json AS BLOB)) <= ?1
                         THEN control_json END
             FROM agent_controls ORDER BY session_id, seq",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map(
            [i64::try_from(MAXIMUM_STORE_CONTROL_PAGE_BYTES)
                .expect("control page bound fits SQLite INTEGER")],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(sql_error)?;
    for row in rows {
        let (encoded_session_id, encoded_len, json) = row.map_err(sql_error)?;
        let session_id = SessionId::new(encoded_session_id)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let record = decode_projected_json::<AgentControlRecord>(
            "Agent control record",
            (encoded_len, json),
            MAXIMUM_STORE_CONTROL_PAGE_BYTES,
        )?;
        match record.body() {
            AgentControlRecordBody::ActivationStarted {
                activation_id,
                parent_session_id,
                root_session_id,
                path,
            } => {
                let (header, _) = read_session_header_row(connection, &session_id)?;
                rsi_agent_store_protocol::validate_activation_lineage(
                    &header,
                    root_session_id,
                    parent_session_id.as_ref(),
                    path,
                )
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                if expected
                    .insert(
                        session_id,
                        StoreActiveActivation {
                            activation_id: activation_id.clone(),
                            parent_session_id: parent_session_id.clone(),
                            turn_id: None,
                            phase: StoreActivationPhase::Running,
                            completion_reserved_bytes: None,
                        },
                    )
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "canonical activation start overlaps an active activation".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageClaimed {
                activation_id,
                turn_id,
                ..
            } => {
                let active = expected.get_mut(&session_id).ok_or_else(|| {
                    StoreError::Corrupt("canonical claim has no active activation".into())
                })?;
                if active.activation_id != *activation_id
                    || active
                        .turn_id
                        .as_ref()
                        .is_some_and(|active_turn| active_turn != turn_id)
                {
                    return Err(StoreError::Corrupt(
                        "canonical claim disagrees with active activation".into(),
                    ));
                }
                active.turn_id.get_or_insert_with(|| turn_id.clone());
            }
            AgentControlRecordBody::ActivationWaitingForDescendants { activation_id } => {
                let active = expected.get_mut(&session_id).ok_or_else(|| {
                    StoreError::Corrupt("canonical wait has no active activation".into())
                })?;
                if active.activation_id != *activation_id
                    || active.phase != StoreActivationPhase::Running
                {
                    return Err(StoreError::Corrupt(
                        "canonical wait disagrees with active activation".into(),
                    ));
                }
                active.phase = StoreActivationPhase::WaitingForDescendants;
            }
            AgentControlRecordBody::CompletionReserved {
                activation_id,
                parent_session_id,
                maximum_bytes,
            } => {
                let active = expected.get_mut(&session_id).ok_or_else(|| {
                    StoreError::Corrupt("canonical reservation has no active activation".into())
                })?;
                if active.activation_id != *activation_id
                    || active.parent_session_id.as_ref() != Some(parent_session_id)
                    || active.completion_reserved_bytes.is_some()
                {
                    return Err(StoreError::Corrupt(
                        "canonical reservation disagrees with active activation".into(),
                    ));
                }
                active.completion_reserved_bytes = Some(*maximum_bytes);
            }
            AgentControlRecordBody::ActivationSettled { activation_id, .. } => {
                let active = expected.get(&session_id).ok_or_else(|| {
                    StoreError::Corrupt("canonical settlement has no active activation".into())
                })?;
                if active.activation_id != *activation_id {
                    return Err(StoreError::Corrupt(
                        "canonical settlement disagrees with active activation".into(),
                    ));
                }
                expected.remove(&session_id);
            }
            AgentControlRecordBody::WaitParked {
                activation_id,
                turn_id,
                ..
            } => {
                let active = expected.get_mut(&session_id).ok_or_else(|| {
                    StoreError::Corrupt("canonical parked wait has no activation".into())
                })?;
                if active.activation_id != *activation_id
                    || active.turn_id.as_ref() != Some(turn_id)
                    || active.phase != StoreActivationPhase::Running
                {
                    return Err(StoreError::Corrupt(
                        "canonical parked wait disagrees with its activation".into(),
                    ));
                }
                active.phase = StoreActivationPhase::Parked;
            }
            AgentControlRecordBody::WaitResumed {
                activation_id,
                turn_id,
                ..
            } => {
                let active = expected.get_mut(&session_id).ok_or_else(|| {
                    StoreError::Corrupt("canonical resumed wait has no activation".into())
                })?;
                if active.activation_id != *activation_id
                    || active.turn_id.as_ref() != Some(turn_id)
                    || active.phase != StoreActivationPhase::Parked
                {
                    return Err(StoreError::Corrupt(
                        "canonical resumed wait disagrees with its activation".into(),
                    ));
                }
                active.phase = StoreActivationPhase::Running;
            }
            AgentControlRecordBody::MessageAccepted { .. }
            | AgentControlRecordBody::MessagePromoted { .. }
            | AgentControlRecordBody::MessageDiscarded { .. } => {}
        }
    }

    let mut actual = BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT session_id, activation_id, parent_session_id, turn_id, phase,
                    completion_reserved_bytes
             FROM active_activations ORDER BY session_id",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .map_err(sql_error)?;
    for row in rows {
        let row = row.map_err(sql_error)?;
        let session_id =
            SessionId::new(row.0).map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let phase = match row.4.as_str() {
            "running" => StoreActivationPhase::Running,
            "parked" => StoreActivationPhase::Parked,
            "waiting" => StoreActivationPhase::WaitingForDescendants,
            _ => {
                return Err(StoreError::Corrupt(
                    "active activation phase is invalid".into(),
                ));
            }
        };
        let active = StoreActiveActivation {
            activation_id: rsi_agent_session_protocol::ActivationId::new(row.1)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            parent_session_id: row
                .2
                .map(SessionId::new)
                .transpose()
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            turn_id: row
                .3
                .map(TurnId::new)
                .transpose()
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            phase,
            completion_reserved_bytes: row
                .5
                .map(|value| decode_u64("completion reservation", value))
                .transpose()?,
        };
        if actual.insert(session_id, active).is_some() {
            return Err(StoreError::Corrupt(
                "active activation index repeats a session".into(),
            ));
        }
    }
    if actual != expected {
        return Err(StoreError::Corrupt(
            "active activation index differs from canonical controls".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // One canonical scan compares the complete Fact-derived session projection.
pub(super) fn validate_canonical_fact_prefix(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<()> {
    let (expected_digest, instructions_sha256, skill_catalog_sha256) = connection
        .query_row(
            "SELECT fact_prefix_sha256, workspace_instructions_sha256,
                    workspace_skill_catalog_sha256
             FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(sql_error)?;
    let expected_workspace_context = StoreWorkspaceContextState {
        instructions_sha256,
        skill_catalog_sha256,
        durable_fact_seq: 0,
    };
    expected_workspace_context.validate()?;
    let mut actual_workspace_context = StoreWorkspaceContextState::default();
    let mut digest = EMPTY_FACT_PREFIX_DIGEST;
    let mut next_sequence = 1_u64;
    let mut statement = connection
        .prepare(
            "SELECT seq, turn_id, fact_kind, length(CAST(fact_json AS BLOB)),
                    CASE WHEN length(CAST(fact_json AS BLOB)) <= ?2
                         THEN fact_json END
             FROM facts WHERE session_id = ?1 ORDER BY seq",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            session_id.as_str(),
            i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                .expect("session Fact bound fits SQLite INTEGER")
        ])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let sequence = decode_u64("Fact sequence", row.get::<_, i64>(0).map_err(sql_error)?)?;
        let turn_id = row.get::<_, String>(1).map_err(sql_error)?;
        let fact_kind = row.get::<_, String>(2).map_err(sql_error)?;
        let fact: SessionFact = decode_projected_json(
            "session Fact",
            (
                row.get::<_, i64>(3).map_err(sql_error)?,
                row.get::<_, Option<String>>(4).map_err(sql_error)?,
            ),
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        if sequence != next_sequence || fact.seq() != sequence {
            return Err(StoreError::Corrupt(
                "session Fact JSON sequence differs from its contiguous durable row".into(),
            ));
        }
        if fact.body().turn_id().as_str() != turn_id || fact_index_kind(fact.body()) != fact_kind {
            return Err(StoreError::Corrupt(
                "session Fact JSON differs from its durable turn index columns".into(),
            ));
        }
        digest = advance_fact_prefix_digest(digest, &fact).map_err(|error| {
            StoreError::Corrupt(format!("stored session Fact is invalid: {error}"))
        })?;
        if let SessionFactBody::InputMessageEntered { source, .. } = fact.body() {
            match source {
                InputMessageSource::AgentInstructions { sha256, .. } => {
                    actual_workspace_context.instructions_sha256 = Some(sha256.clone());
                }
                InputMessageSource::SkillCatalog { sha256 } => {
                    actual_workspace_context.skill_catalog_sha256 = Some(sha256.clone());
                }
                InputMessageSource::Human { .. }
                | InputMessageSource::Agent { .. }
                | InputMessageSource::Completion { .. }
                | InputMessageSource::UserSkillInvocation { .. } => {}
            }
        }
        if matches!(fact.body(), SessionFactBody::TurnTerminal { .. }) {
            let terminal_digest = connection
                .query_row(
                    "SELECT terminal_prefix_sha256 FROM turns
                     WHERE session_id = ?1 AND turn_id = ?2 AND terminal_seq = ?3",
                    params![
                        session_id.as_str(),
                        fact.body().turn_id().as_str(),
                        sqlite_u64("turn terminal sequence", fact.seq())?,
                    ],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(sql_error)?
                .ok_or_else(|| {
                    StoreError::Corrupt("terminal turn lacks its prefix digest".into())
                })?;
            if terminal_digest != hex::encode(digest) {
                return Err(StoreError::Corrupt(
                    "terminal-prefix digest differs from the canonical Fact stream".into(),
                ));
            }
        }
        next_sequence = next_sequence.checked_add(1).ok_or_else(|| {
            StoreError::Corrupt("session Fact sequence overflowed during audit".into())
        })?;
    }
    if hex::encode(digest) != expected_digest {
        return Err(StoreError::Corrupt(
            "Fact-prefix digest differs from the canonical Fact stream".into(),
        ));
    }
    if actual_workspace_context.instructions_sha256
        != expected_workspace_context.instructions_sha256
        || actual_workspace_context.skill_catalog_sha256
            != expected_workspace_context.skill_catalog_sha256
    {
        return Err(StoreError::Corrupt(
            "workspace-context digest index differs from the canonical Fact stream".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_canonical_control_prefix(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<()> {
    let expected_digest = connection
        .query_row(
            "SELECT control_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)?;
    let mut digest = EMPTY_CONTROL_PREFIX_DIGEST;
    let mut next_sequence = 1_u64;
    let mut statement = connection
        .prepare(
            "SELECT seq, length(CAST(control_json AS BLOB)),
                    CASE WHEN length(CAST(control_json AS BLOB)) <= ?2
                         THEN control_json END
             FROM agent_controls WHERE session_id = ?1 ORDER BY seq",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            session_id.as_str(),
            i64::try_from(MAXIMUM_SESSION_FACT_BYTES).expect("control bound fits SQLite INTEGER"),
        ])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let sequence = decode_u64("control sequence", row.get::<_, i64>(0).map_err(sql_error)?)?;
        let record: AgentControlRecord = decode_projected_json(
            "Agent control record",
            (
                row.get::<_, i64>(1).map_err(sql_error)?,
                row.get::<_, Option<String>>(2).map_err(sql_error)?,
            ),
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        if sequence != next_sequence || record.seq() != sequence {
            return Err(StoreError::Corrupt(
                "Agent control JSON sequence differs from its contiguous durable row".into(),
            ));
        }
        digest = advance_control_prefix_digest(digest, &record).map_err(|error| {
            StoreError::Corrupt(format!("stored Agent control record is invalid: {error}"))
        })?;
        next_sequence = next_sequence.checked_add(1).ok_or_else(|| {
            StoreError::Corrupt("Agent control sequence overflowed during audit".into())
        })?;
    }
    if hex::encode(digest) != expected_digest {
        return Err(StoreError::Corrupt(
            "control-prefix digest differs from the canonical control stream".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Offline verification derives and compares the complete ready index atomically.
pub(super) fn validate_ready_index(connection: &Connection) -> Result<()> {
    let mut expected = BTreeMap::<(String, String), (String, u64, u64, String)>::new();
    let mut accepted_roots = BTreeMap::<(String, String), String>::new();
    let mut statement = connection
        .prepare(
            "SELECT session_id, length(CAST(control_json AS BLOB)),
                    CASE WHEN length(CAST(control_json AS BLOB)) <= ?1
                         THEN control_json END
             FROM agent_controls ORDER BY session_id, seq",
        )
        .map_err(sql_error)?;
    let rows =
        statement
            .query_map(
                [i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                    .expect("control bound fits SQLite INTEGER")],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .map_err(sql_error)?;
    for row in rows {
        let (session_id, length, json) = row.map_err(sql_error)?;
        let record: AgentControlRecord = decode_projected_json(
            "Agent control record",
            (length, json),
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        match record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                root_session_id,
                target,
                wake_required,
            } => {
                let key = (session_id.clone(), message.message_id.to_string());
                if accepted_roots
                    .insert(key.clone(), root_session_id.to_string())
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "ready-message source repeats one accepted identity".into(),
                    ));
                }
                if !wake_required {
                    continue;
                }
                if expected
                    .insert(
                        key,
                        (
                            root_session_id.to_string(),
                            record.seq(),
                            record.timestamp_ms(),
                            message_target_name(*target).into(),
                        ),
                    )
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "ready-message source repeats one pending identity".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessagePromoted { message_id } => {
                let key = (session_id.clone(), message_id.to_string());
                let root_session_id = accepted_roots.get(&key).ok_or_else(|| {
                    StoreError::Corrupt("ready promotion has no accepted message".into())
                })?;
                if expected
                    .insert(
                        key,
                        (
                            root_session_id.clone(),
                            record.seq(),
                            record.timestamp_ms(),
                            message_target_name(MessageTarget::NextTurn).into(),
                        ),
                    )
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "ready promotion repeats a waking message identity".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageClaimed { message_id, .. }
            | AgentControlRecordBody::MessageDiscarded { message_id, .. } => {
                expected.remove(&(session_id.clone(), message_id.to_string()));
            }
            AgentControlRecordBody::ActivationStarted { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::ActivationSettled { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::CompletionReserved { .. } => {}
        }
    }

    let actual = {
        let mut statement = connection
            .prepare(
                "SELECT session_id, message_id, root_session_id, ready_control_seq,
                        timestamp_ms, target
                 FROM ready_messages ORDER BY session_id, message_id",
            )
            .map_err(sql_error)?;
        statement
            .query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    (
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ),
                ))
            })
            .map_err(sql_error)?
            .map(|row| {
                let (key, (root, sequence, timestamp, target)) = row.map_err(sql_error)?;
                Ok((
                    key,
                    (
                        root,
                        decode_u64("ready control sequence", sequence)?,
                        decode_u64("ready timestamp", timestamp)?,
                        target,
                    ),
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?
    };
    if actual != expected {
        return Err(StoreError::Corrupt(
            "ready-message index differs from the canonical control streams".into(),
        ));
    }
    Ok(())
}
