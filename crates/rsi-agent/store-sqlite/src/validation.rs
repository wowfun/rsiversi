use super::*;

pub(super) trait CanonicalObserver {
    fn checkpoint(&mut self) -> Result<()> {
        Ok(())
    }
    fn control(&mut self, _: &SessionId, _: &AgentControlRecord) -> Result<()> {
        Ok(())
    }
    fn fact(&mut self, _: &SessionId, _: &SessionFact) -> Result<()> {
        Ok(())
    }
}
struct Unobserved;
impl CanonicalObserver for Unobserved {}

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
    let expected = EXPECTED_TABLES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let actual = user_tables(connection)?;
    if actual != expected {
        return Err(StoreError::Corrupt(
            "Store table set differs from its declared schema".into(),
        ));
    }
    for (table, expected_sql) in EXPECTED_TABLES {
        let observed_sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| bounded_text(row, 0, expected_sql.len()),
            )
            .map_err(sql_error)?;
        if observed_sql != *expected_sql {
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
                |row| bounded_text(row, 0, expected_sql.len()),
            )
            .map_err(sql_error)?;
        if observed_sql != *expected_sql {
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
        .query_map([], |row| bounded_text(row, 0, 256))
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
        .query_map([], |row| bounded_text(row, 0, 256))
        .map_err(sql_error)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(sql_error)
}

pub(super) fn validate_session(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<ControlReplaySummary> {
    validate_session_observed(connection, session_id, &mut Unobserved)
}

pub(super) fn validate_session_observed(
    connection: &Connection,
    session_id: &SessionId,
    observer: &mut impl CanonicalObserver,
) -> Result<ControlReplaySummary> {
    let (header, durable_seq) = read_session_header_row(connection, session_id)?;
    rsi_agent_store_protocol::validate_program_header(
        &super::program_graph::Graph(connection),
        &header,
    )
    .map_err(|error| StoreError::Corrupt(error.to_string()))?;

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
    validate_session_lineage(connection, &header)?;
    validate_agent_indexes_observed(connection, &header, observer)
}

fn validate_session_lineage(connection: &Connection, header: &SessionHeader) -> Result<()> {
    let session_id = header.session_id();
    let node = connection
        .query_row(
            "SELECT root_session_id, parent_session_id, path_json, task_name, execution_owner_json
             FROM agent_nodes WHERE session_id = ?1",
            [session_id.as_str()],
            |row| {
                Ok((
                    bounded_text(row, 0, 256)?,
                    bounded_text(row, 1, 256)?,
                    bounded_text(
                        row,
                        2,
                        rsi_agent_session_protocol::AgentPath::MAXIMUM_JSON_BYTES,
                    )?,
                    bounded_text(row, 3, 256)?,
                    bounded_text(row, 4, super::MAXIMUM_INDEXED_EXECUTION_OWNER_BYTES)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    match (header.fork_origin(), node) {
        (None, None) => {}
        (Some(origin), Some((root, parent, path, task_name, owner)))
            if root == origin.root_session_id.as_str()
                && parent == origin.parent_session_id.as_str()
                && decode_json::<rsi_agent_session_protocol::AgentPath>("Agent path", &path)?
                    == origin.path
                && task_name == origin.task_name
                && Some(&decode_json::<rsi_agent_session_protocol::ExecutionOwner>(
                    "execution owner",
                    &owner,
                )?) == header.execution_owner() => {}
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
            |row| optional_text(row, 0, 256),
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
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static CONTROL_OBSERVER: std::cell::RefCell<Option<Arc<AtomicU64>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(any(test, feature = "test-support"))]
pub(super) struct ControlObserver(Option<Arc<AtomicU64>>);
#[cfg(any(test, feature = "test-support"))]
impl ControlObserver {
    pub(super) fn enter(counter: Arc<AtomicU64>) -> Self {
        Self(CONTROL_OBSERVER.with(|scope| scope.replace(Some(counter))))
    }
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for ControlObserver {
    fn drop(&mut self) {
        CONTROL_OBSERVER.with(|scope| scope.replace(self.0.take()));
    }
}
#[cfg(test)]
thread_local! {
    pub(super) static READY_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CONTROL_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static HEADER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn read_session_header_row(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<(SessionHeader, u64)> {
    #[cfg(test)]
    HEADER_READS.set(HEADER_READS.get() + 1);
    let (
        created_at_ms,
        durable_seq,
        fact_prefix_sha256,
        header_encoded_len,
        header_json,
        control_prefix_sha256,
        control_seq,
        coordinates_key,
        last_activity_ms,
    ) = connection
        .query_row(
            "SELECT created_at_ms, durable_seq,
                    CASE WHEN length(CAST(fact_prefix_sha256 AS BLOB)) = 64
                         THEN fact_prefix_sha256 END,
                    length(CAST(header_json AS BLOB)),
                    CASE WHEN length(CAST(header_json AS BLOB)) <= ?2
                         THEN header_json END,
                    CASE WHEN length(CAST(control_prefix_sha256 AS BLOB)) = 64
                         THEN control_prefix_sha256 END, control_seq, coordinates_key, last_activity_ms
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
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    bounded_text(row, 7, 128 * 1024)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
    let durable_seq = decode_u64("durable sequence", durable_seq)?;
    decode_u64("durable control sequence", control_seq)?;
    for (label, digest) in [
        ("Fact-prefix digest", fact_prefix_sha256),
        ("Control-prefix digest", control_prefix_sha256),
    ] {
        let digest =
            digest.ok_or_else(|| StoreError::Corrupt(format!("{label} has invalid length")))?;
        validate_sha256(label, &digest)?;
    }
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
    if super::activity::coordinates_key(header.coordinates())? != coordinates_key
        || decode_u64("activity timestamp", last_activity_ms)? < header.created_at_ms()
    {
        return Err(StoreError::Corrupt(
            "Session coordinate or activity projection differs from its Header".into(),
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
                    OR (fact.fact_kind = 'terminal' AND turn.terminal_seq IS NOT fact.seq)
                    OR (fact.fact_kind = 'event' AND (
                         fact.seq <= turn.accepted_seq
                         OR (turn.terminal_seq IS NOT NULL AND fact.seq >= turn.terminal_seq)
                       ))
                  )
               UNION ALL
               SELECT 1
               FROM turns AS turn
               WHERE turn.session_id = ?1 AND (
                    (turn.terminal_seq IS NULL AND (turn.terminal_prefix_sha256 IS NOT NULL
                        OR turn.terminal_control_seq IS NOT NULL OR turn.terminal_control_prefix_sha256 IS NOT NULL))
                    OR (turn.terminal_seq IS NOT NULL AND
                        (turn.terminal_prefix_sha256 IS NULL
                         OR length(turn.terminal_prefix_sha256) != 64
                         OR turn.terminal_control_seq IS NULL
                         OR turn.terminal_control_prefix_sha256 IS NULL
                         OR length(turn.terminal_control_prefix_sha256) != 64))
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
    validate_database_observed(connection, &mut Unobserved)
}

pub(super) fn validate_database_observed(
    connection: &Connection,
    observer: &mut impl CanonicalObserver,
) -> Result<()> {
    let integrity = connection
        .query_row("PRAGMA integrity_check", [], |row| {
            bounded_text(
                row,
                0,
                rsi_agent_session_protocol::MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
            )
        })
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
    let mut cursor = None;
    loop {
        observer.checkpoint()?;
        let page = session_id_page(connection, cursor.as_ref())?;
        if page.is_empty() {
            break;
        }
        for session_id in &page {
            observer.checkpoint()?;
            let controls = validate_session_observed(connection, session_id, observer)?;
            let facts = validate_canonical_fact_prefix(connection, session_id, observer)?;
            let (created, activity) = connection
                .query_row(
                    "SELECT created_at_ms, last_activity_ms FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(sql_error)?;
            if decode_u64("activity timestamp", activity)?
                != decode_u64("creation timestamp", created)?
                    .max(facts)
                    .max(controls.activity_ms)
            {
                return Err(StoreError::Corrupt(
                    "activity index differs from canonical records".into(),
                ));
            }
        }
        cursor = page.last().cloned();
    }
    Ok(())
}

// Separate keyset statements keep the continuation an indexed range scan. The
// length predicate gates projection, never row selection: corruption cannot hide.
pub(super) fn session_id_page(
    connection: &Connection,
    after: Option<&SessionId>,
) -> Result<Vec<SessionId>> {
    let projection =
        "SELECT CASE WHEN length(CAST(session_id AS BLOB)) <= ?1 THEN session_id END FROM sessions";
    let sql = if after.is_some() {
        format!("{projection} WHERE session_id > ?2 ORDER BY session_id LIMIT 256")
    } else {
        format!("{projection} ORDER BY session_id LIMIT 256")
    };
    let maximum = i64::try_from(rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES)
        .expect("identity bound fits SQLite INTEGER");
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let mut rows = if let Some(after) = after {
        statement.query(params![maximum, after.as_str()])
    } else {
        statement.query([maximum])
    }
    .map_err(sql_error)?;
    let mut page = Vec::with_capacity(256);
    while let Some(row) = rows.next().map_err(sql_error)? {
        let encoded = row
            .get::<_, Option<String>>(0)
            .map_err(|error| {
                StoreError::Corrupt(format!("invalid durable session identity: {error}"))
            })?
            .ok_or_else(|| {
                StoreError::Corrupt(
                    "durable session identity is missing or exceeds its byte bound".into(),
                )
            })?;
        page.push(SessionId::new(encoded).map_err(|error| {
            StoreError::Corrupt(format!("durable session identity is invalid: {error}"))
        })?);
    }
    Ok(page)
}

#[derive(Default)]
struct MailboxProjection {
    accepted_messages: u64,
    expected_messages: BTreeMap<(SessionId, MessageId), StoreAgentMessage>,
}

impl MailboxProjection {
    #[allow(clippy::too_many_lines)] // Keep each ordered control transition projection together.
    fn apply(
        &mut self,
        connection: &Connection,
        header: &SessionHeader,
        record: &AgentControlRecord,
        slot: Option<&rsi_agent_session_protocol::QueueSlot>,
    ) -> Result<()> {
        let session_id = header.session_id();
        let Self {
            accepted_messages,
            expected_messages,
        } = self;
        match record.body() {
            AgentControlRecordBody::MessageSuccessor { predecessor_id, .. } => {
                let mut predecessor = expected_messages
                    .remove(&(session_id.clone(), predecessor_id.clone()))
                    .ok_or_else(|| {
                        StoreError::Corrupt("queue predecessor is not pending".into())
                    })?;
                super::queue::validate_successor(connection, session_id, &predecessor, record)?;
                predecessor.state = StoreAgentMessageState::Discarded {
                    reason: MessageDiscardReason::Replaced,
                    control_seq: record.seq(),
                };
                if read_indexed_agent_message(connection, session_id, predecessor_id)?
                    != predecessor
                {
                    return Err(StoreError::Corrupt(
                        "queue predecessor projection differs".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageAccepted {
                message,
                delivery,
                bound_turn_id,
                root_session_id,
                target,
                wake_required,
            } => {
                if header
                    .fork_origin()
                    .map_or(session_id, |origin| &origin.root_session_id)
                    != root_session_id
                {
                    return Err(StoreError::Corrupt(
                        "canonical Agent message names a foreign root".into(),
                    ));
                }
                *accepted_messages = accepted_messages.checked_add(1).ok_or_else(|| {
                    StoreError::Corrupt("canonical mailbox count overflowed".into())
                })?;
                if expected_messages
                    .insert(
                        (session_id.clone(), message.message_id.clone()),
                        StoreAgentMessage {
                            queue_slot: slot.expect("canonical acceptance slot").clone(),
                            delivery: *delivery,
                            bound_turn_id: bound_turn_id.clone(),
                            accepted_timestamp_ms: record.timestamp_ms(),
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
                expected.validate_claim_turn(turn_id)?;
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
                    || !expected.permits_promotion()
                {
                    return Err(StoreError::Corrupt(
                        "canonical promotion requires eligible pending next-Step input".into(),
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
            AgentControlRecordBody::QueueMutationRecorded { .. }
            | AgentControlRecordBody::ActivationStarted { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::ActivationSettled { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::CompletionReserved { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::DomainStateCommitted { .. }
            | AgentControlRecordBody::ProgramRun { .. }
            | AgentControlRecordBody::ProgramCompletionReserved { .. } => {}
        }
        if let AgentControlRecordBody::MessageClaimed { message_id, .. }
        | AgentControlRecordBody::MessageDiscarded { message_id, .. } = record.body()
        {
            let expected = expected_messages
                .remove(&(session_id.clone(), message_id.clone()))
                .expect("validated closing message");
            if read_indexed_agent_message(connection, session_id, message_id)? != expected {
                return Err(StoreError::Corrupt(
                    "mailbox index final projection differs from canonical controls".into(),
                ));
            }
        }
        if expected_messages.len() > rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES {
            return Err(StoreError::Corrupt(
                "canonical pending mailbox exceeds its count bound".into(),
            ));
        }
        Ok(())
    }
    fn finish(self, connection: &Connection, selected: &SessionId) -> Result<()> {
        let Self {
            accepted_messages,
            expected_messages,
        } = self;

        for ((session_id, message_id), expected) in expected_messages {
            if read_indexed_agent_message(connection, &session_id, &message_id)? != expected {
                return Err(StoreError::Corrupt(
                    "mailbox index final projection differs from canonical controls".into(),
                ));
            }
        }
        let indexed_messages = connection
            .query_row(
                "SELECT COUNT(*) FROM agent_messages WHERE session_id = ?1",
                [selected.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)
            .and_then(|count| decode_u64("indexed mailbox count", count))?;
        if indexed_messages != accepted_messages {
            return Err(StoreError::Corrupt(
                "mailbox index cardinality differs from canonical controls".into(),
            ));
        }
        Ok(())
    }
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
                    CASE WHEN length(CAST(state_json AS BLOB)) <= ?4 THEN state_json END,
                    delivery, CASE WHEN bound_turn_id IS NULL OR length(CAST(bound_turn_id AS BLOB)) <= 256 THEN bound_turn_id ELSE '' END, accepted_timestamp_ms, CASE WHEN length(CAST(queue_slot_id AS BLOB)) <= 256 THEN queue_slot_id ELSE '' END, queue_timestamp_ms, queue_control_seq, has_turn_options
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

#[derive(Default)]
struct ActivationProjection {
    expected: BTreeMap<SessionId, StoreActiveActivation>,
}

impl ActivationProjection {
    #[allow(clippy::too_many_lines)] // Keep each ordered control transition projection together.
    fn apply(&mut self, header: &SessionHeader, record: &AgentControlRecord) -> Result<()> {
        let session_id = header.session_id().clone();
        let Self { expected } = self;
        match record.body() {
            AgentControlRecordBody::ProgramCompletionReserved {
                activation_id,
                run_id,
                ordinal,
            } => {
                rsi_agent_store_protocol::validate_program_completion_sink(
                    header, run_id, *ordinal,
                )?;
                let active = expected
                    .get_mut(&session_id)
                    .ok_or_else(|| StoreError::Corrupt("program sink has no activation".into()))?;
                if active.activation_id != *activation_id
                    || active.completion_reserved_bytes.is_some()
                    || active.completion_to_program
                {
                    return Err(StoreError::Corrupt(
                        "program sink is already reserved or mismatched".into(),
                    ));
                }
                active.completion_to_program = true;
            }

            AgentControlRecordBody::ActivationStarted {
                activation_id,
                parent_session_id,
                root_session_id,
                path,
            } => {
                rsi_agent_store_protocol::validate_activation_lineage(
                    header,
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
                            completion_to_program: false,
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
                    || active.completion_to_program
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
            AgentControlRecordBody::MessageAccepted {
                bound_turn_id: Some(bound),
                ..
            } => {
                if !expected.get(&session_id).is_some_and(|active| {
                    active.turn_id.as_ref() == Some(bound)
                        && matches!(
                            active.phase,
                            StoreActivationPhase::Running | StoreActivationPhase::Parked
                        )
                }) {
                    return Err(StoreError::Corrupt(
                        "canonical steering binding has no current activation Turn".into(),
                    ));
                }
            }
            AgentControlRecordBody::MessageAccepted {
                bound_turn_id: None,
                ..
            }
            | AgentControlRecordBody::MessagePromoted { .. }
            | AgentControlRecordBody::MessageDiscarded { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::DomainStateCommitted { .. }
            | AgentControlRecordBody::MessageSuccessor { .. }
            | AgentControlRecordBody::QueueMutationRecorded { .. }
            | AgentControlRecordBody::ProgramRun { .. } => {}
        }
        Ok(())
    }
    fn finish(self, connection: &Connection, selected: &SessionId) -> Result<()> {
        let Self { expected } = self;

        let mut actual = BTreeMap::new();
        let mut statement = connection
            .prepare(
                "SELECT session_id, activation_id, parent_session_id, turn_id, phase,
                    completion_reserved_bytes, completion_to_program
             FROM active_activations WHERE session_id = ?1",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map([selected.as_str()], |row| {
                Ok((
                    bounded_text(row, 0, 256)?,
                    bounded_text(row, 1, 256)?,
                    optional_text(row, 2, 256)?,
                    optional_text(row, 3, 256)?,
                    bounded_text(row, 4, 32)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, bool>(6)?,
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
                completion_to_program: row.6,
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
}

#[allow(clippy::too_many_lines)] // One canonical scan compares the complete Fact-derived session projection.
fn validate_canonical_fact_prefix(
    connection: &Connection,
    session_id: &SessionId,
    observer: &mut impl CanonicalObserver,
) -> Result<u64> {
    let mut activity = rsi_agent_store_protocol::ActivityProjection::default();
    let expected_digest = connection
        .query_row(
            "SELECT fact_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| bounded_text(row, 0, 64),
        )
        .map_err(sql_error)?;
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
        observer.checkpoint()?;
        let sequence = decode_u64("Fact sequence", row.get::<_, i64>(0).map_err(sql_error)?)?;
        let turn_id = bounded_text(row, 1, 256).map_err(sql_error)?;
        let fact_kind = bounded_text(row, 2, 8).map_err(sql_error)?;
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
        observer.fact(session_id, &fact)?;
        activity.observe_fact(&fact);
        digest = advance_fact_prefix_digest(digest, &fact).map_err(|error| {
            StoreError::Corrupt(format!("stored session Fact is invalid: {error}"))
        })?;
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
                    |row| optional_text(row, 0, 64),
                )
                .optional()
                .map_err(sql_error)?
                .flatten()
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
    Ok(activity.latest())
}

#[derive(Default)]
struct ReadyProjection {
    expected: BTreeMap<(String, String), (String, u64, u64, String)>,
    accepted_roots: BTreeMap<
        (String, String),
        (
            String,
            rsi_agent_session_protocol::MessageDelivery,
            u64,
            u64,
        ),
    >,
}

impl ReadyProjection {
    #[allow(clippy::too_many_lines)] // Keep each ordered control transition projection together.
    fn apply(
        &mut self,
        selected: &SessionId,
        record: &AgentControlRecord,
        slot: Option<&rsi_agent_session_protocol::QueueSlot>,
    ) -> Result<()> {
        let session_id = selected.as_str();
        let Self {
            expected,
            accepted_roots,
        } = self;
        match record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                delivery,
                bound_turn_id: _,
                root_session_id,
                target,
                wake_required,
            } => {
                let slot = slot.expect("canonical acceptance slot");
                let key = (session_id.to_owned(), message.message_id.to_string());
                if accepted_roots
                    .insert(
                        key.clone(),
                        (
                            root_session_id.to_string(),
                            *delivery,
                            slot.control_seq,
                            slot.timestamp_ms,
                        ),
                    )
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "ready-message source repeats one accepted identity".into(),
                    ));
                }
                if !wake_required {
                    return Ok(());
                }
                if expected
                    .insert(
                        key,
                        (
                            root_session_id.to_string(),
                            slot.control_seq,
                            slot.timestamp_ms,
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
                let key = (session_id.to_owned(), message_id.to_string());
                let root_session_id = accepted_roots.get(&key).ok_or_else(|| {
                    StoreError::Corrupt("ready promotion has no accepted message".into())
                })?;
                let (control_seq, timestamp_ms) =
                    if root_session_id.1 == rsi_agent_session_protocol::MessageDelivery::Steer {
                        (root_session_id.2, root_session_id.3)
                    } else {
                        (record.seq(), record.timestamp_ms())
                    };
                if expected
                    .insert(
                        key,
                        (
                            root_session_id.0.clone(),
                            control_seq,
                            timestamp_ms,
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
            AgentControlRecordBody::MessageSuccessor {
                predecessor_id: message_id,
                ..
            }
            | AgentControlRecordBody::MessageClaimed { message_id, .. }
            | AgentControlRecordBody::MessageDiscarded { message_id, .. } => {
                let key = (session_id.to_owned(), message_id.to_string());
                expected.remove(&key);
                accepted_roots.remove(&key);
            }
            AgentControlRecordBody::ActivationStarted { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::ActivationSettled { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::CompletionReserved { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::DomainStateCommitted { .. }
            | AgentControlRecordBody::ProgramRun { .. }
            | AgentControlRecordBody::QueueMutationRecorded { .. }
            | AgentControlRecordBody::ProgramCompletionReserved { .. } => {}
        }
        Ok(())
    }
    fn finish(self, connection: &Connection, selected: &SessionId) -> Result<()> {
        let Self { expected, .. } = self;

        let mut statement = connection.prepare(
            "SELECT session_id, message_id, root_session_id, ready_control_seq, timestamp_ms, target
             FROM ready_messages WHERE session_id = ?1 ORDER BY message_id COLLATE BINARY LIMIT ?2"
        ).map_err(sql_error)?;
        let lookahead = i64::try_from(expected.len() + 1).expect("ready projection is bounded");
        let mut rows = statement
            .query(params![selected.as_str(), lookahead])
            .map_err(sql_error)?;
        let mut expected = expected.into_iter();
        while let Some(row) = rows.next().map_err(sql_error)? {
            #[cfg(test)]
            READY_ROWS.set(READY_ROWS.get() + 1);
            let key = (
                bounded_text(row, 0, 256).map_err(sql_error)?,
                bounded_text(row, 1, 256).map_err(sql_error)?,
            );
            let value = (
                bounded_text(row, 2, 256).map_err(sql_error)?,
                decode_u64("ready control sequence", row.get(3).map_err(sql_error)?)?,
                decode_u64("ready timestamp", row.get(4).map_err(sql_error)?)?,
                bounded_text(row, 5, 16).map_err(sql_error)?,
            );
            if expected.next() != Some((key, value)) {
                return Err(StoreError::Corrupt(
                    "ready-message index differs from the canonical control streams".into(),
                ));
            }
        }
        if expected.next().is_some() {
            return Err(StoreError::Corrupt(
                "ready-message index differs from the canonical control streams".into(),
            ));
        }
        Ok(())
    }
}

/// Canonical replay results consumed by online projection and offline audit.
#[derive(Debug)]
pub(super) struct ControlReplaySummary {
    pub(super) activity_ms: u64,
}

/// Decode each bounded canonical control once and feed all index projections.
#[allow(clippy::too_many_lines)] // Validate the canonical control horizon and every dependent bounded index together.
#[cfg(test)]
pub(super) fn validate_agent_indexes(
    connection: &Connection,
    header: &SessionHeader,
) -> Result<ControlReplaySummary> {
    validate_agent_indexes_observed(connection, header, &mut Unobserved)
}

#[allow(clippy::too_many_lines)] // One canonical pass validates all related projections.
fn validate_agent_indexes_observed(
    connection: &Connection,
    header: &SessionHeader,
    observer: &mut impl CanonicalObserver,
) -> Result<ControlReplaySummary> {
    let selected = header.session_id();
    let mut mailbox = MailboxProjection::default();
    let mut ready = ReadyProjection::default();
    let mut activation = ActivationProjection::default();
    let mut domains = super::domain::Projection::default();
    let mut programs = super::program::Projection::default();
    let mut decoded = 0_u64;
    let mut activity = rsi_agent_store_protocol::ActivityProjection::default();
    let mut digest = EMPTY_CONTROL_PREFIX_DIGEST;
    let mut terminals = 0_u64;
    let mut queue_receipts = 0_usize;
    let mut last_settled_control_seq = 0;
    let mut successor_link = None;
    let mut statement = connection
        .prepare(
            "SELECT length(CAST(control_json AS BLOB)),
                CASE WHEN length(CAST(control_json AS BLOB)) <= ?1 THEN control_json END, seq
         FROM agent_controls WHERE session_id = ?2 ORDER BY seq",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            i64::try_from(MAXIMUM_SESSION_FACT_BYTES).expect("control bound fits SQLite INTEGER"),
            selected.as_str(),
        ])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        observer.checkpoint()?;
        let record: AgentControlRecord = decode_projected_json(
            "Agent control record",
            (
                row.get(0).map_err(sql_error)?,
                row.get(1).map_err(sql_error)?,
            ),
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        decoded += 1;
        observer.control(selected, &record)?;
        activity.observe_control(&record);
        #[cfg(test)]
        CONTROL_DECODES.set(CONTROL_DECODES.get() + 1);
        #[cfg(any(test, feature = "test-support"))]
        CONTROL_OBSERVER.with(|scope| {
            if let Some(counter) = &*scope.borrow() {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        if record.seq() != decoded
            || decode_u64("control row sequence", row.get(2).map_err(sql_error)?)? != decoded
        {
            return Err(StoreError::Corrupt(
                "control JSON sequence differs from its canonical row".into(),
            ));
        }
        digest = advance_control_prefix_digest(digest, &record)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        if validate_terminal_control(connection, selected, &record, digest)? {
            terminals += 1;
        }
        if rsi_agent_store_protocol::needs_program_graph(record.body()) {
            rsi_agent_store_protocol::validate_program_graph(
                &super::program_graph::Graph(connection),
                selected,
                &record,
            )
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        }
        if let AgentControlRecordBody::QueueMutationRecorded { receipt } = record.body() {
            queue_receipts += 1;
            if super::queue::receipt_position(connection, selected, &receipt.operation_id)?
                != Some(record.seq())
            {
                return Err(StoreError::Corrupt(
                    "queue receipt index differs from canonical controls".into(),
                ));
            }
            super::queue::validate_receipt_boundary(connection, selected, &record)?;
        }
        let slot = if matches!(
            record.body(),
            AgentControlRecordBody::MessageAccepted { .. }
        ) {
            Some(super::queue::acceptance_slot_from_link(
                &record,
                successor_link.as_ref(),
            )?)
        } else {
            None
        };
        mailbox.apply(connection, header, &record, slot.as_ref())?;
        if matches!(
            record.body(),
            AgentControlRecordBody::ActivationSettled { .. }
        ) {
            last_settled_control_seq = record.seq();
        }
        ready.apply(selected, &record, slot.as_ref())?;
        activation.apply(header, &record)?;
        domains.apply(connection, selected, &record)?;
        programs.apply(connection, selected, &record)?;
        successor_link = matches!(
            record.body(),
            AgentControlRecordBody::MessageSuccessor { .. }
        )
        .then_some(record);
    }
    let (indexed_terminals, indexed_settlement, indexed_digest) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM turns WHERE session_id = ?1 AND terminal_seq IS NOT NULL),
                last_settled_control_seq, control_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [selected.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, bounded_text(row, 2, 64)?)),
        )
        .map_err(sql_error)?;
    if digest != decode_sha256("control-prefix digest", &indexed_digest)? {
        return Err(StoreError::Corrupt(
            "control-prefix digest differs from the canonical control stream".into(),
        ));
    }
    if last_settled_control_seq != decode_u64("last settlement sequence", indexed_settlement)? {
        return Err(StoreError::Corrupt(
            "last settlement sequence differs from canonical controls".into(),
        ));
    }
    if terminals != decode_u64("terminal index count", indexed_terminals)? {
        return Err(StoreError::Corrupt(
            "terminal index has no unique canonical control marker".into(),
        ));
    }
    if queue_receipts != super::queue::count(connection, selected)? {
        return Err(StoreError::Corrupt(
            "queue receipt index count differs".into(),
        ));
    }
    mailbox.finish(connection, selected)?;
    ready.finish(connection, selected)?;
    activation.finish(connection, selected)?;
    domains.finish(connection, selected)?;
    programs.finish(connection, selected)?;
    Ok(ControlReplaySummary {
        activity_ms: activity.latest(),
    })
}

/// Checks each marker against the sole derived index; counts detect indexed terminals without a marker.
fn validate_terminal_control(
    connection: &Connection,
    session_id: &SessionId,
    record: &AgentControlRecord,
    digest: [u8; 32],
) -> Result<bool> {
    let AgentControlRecordBody::TurnBoundaryRecorded {
        turn_id,
        terminal_fact_seq,
    } = record.body()
    else {
        return Ok(false);
    };
    let matches = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ?1 AND turn_id = ?2
            AND terminal_seq = ?3 AND terminal_control_seq = ?4 AND terminal_control_prefix_sha256 = ?5)",
        params![session_id.as_str(), turn_id.as_str(), sqlite_u64("terminal Fact sequence", *terminal_fact_seq)?,
            sqlite_u64("terminal control sequence", record.seq())?, hex::encode(digest)],
        |row| row.get::<_, bool>(0),
    ).map_err(sql_error)?;
    if !matches {
        return Err(StoreError::Corrupt(
            "terminal index differs from the canonical Fact/control boundary".into(),
        ));
    }
    Ok(true)
}
