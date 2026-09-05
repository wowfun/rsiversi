use super::*;

pub(super) const LIST_READY_MESSAGES_AFTER_SQL: &str =
    "SELECT session_id, message_id, ready_control_seq, timestamp_ms, target
     FROM ready_messages
     WHERE root_session_id = ?1
       AND (timestamp_ms, session_id, ready_control_seq) > (?2, ?3, ?4)
     ORDER BY timestamp_ms, session_id, ready_control_seq
     LIMIT ?5";
const LIST_READY_MESSAGES_FIRST_SQL: &str =
    "SELECT session_id, message_id, ready_control_seq, timestamp_ms, target
     FROM ready_messages
     WHERE root_session_id = ?1
     ORDER BY timestamp_ms, session_id, ready_control_seq
     LIMIT ?5";
pub(super) const LIST_AGENT_CHILDREN_AFTER_SQL: &str =
    "SELECT session_id, path_json, task_name FROM agent_nodes
     WHERE parent_session_id = ?1 AND session_id > ?2
     ORDER BY session_id LIMIT ?3";
const LIST_AGENT_CHILDREN_FIRST_SQL: &str =
    "SELECT session_id, path_json, task_name FROM agent_nodes
     WHERE parent_session_id = ?1
     ORDER BY session_id LIMIT ?3";
pub(super) const LIST_WAITING_ACTIVATIONS_AFTER_SQL: &str =
    "SELECT session_id FROM active_activations
     WHERE phase = 'waiting' AND session_id > ?1
     ORDER BY session_id LIMIT ?2";
const LIST_WAITING_ACTIVATIONS_FIRST_SQL: &str = "SELECT session_id FROM active_activations
     WHERE phase = 'waiting'
     ORDER BY session_id LIMIT ?2";
pub(super) const LIST_READY_ROOTS_AFTER_SQL: &str =
    "SELECT DISTINCT root_session_id FROM ready_messages
     WHERE root_session_id > ?1
     ORDER BY root_session_id LIMIT ?2";
const LIST_READY_ROOTS_FIRST_SQL: &str = "SELECT DISTINCT root_session_id FROM ready_messages
     ORDER BY root_session_id LIMIT ?2";

#[async_trait]
#[allow(clippy::too_many_lines)] // The trait implementation keeps each Store seam explicit.
impl SessionStore for SqliteStore {
    async fn append(&self, batch: AppendBatch) -> Result<AppendCommit> {
        batch.validate()?;
        let session_id = batch.session_id.clone();
        if !self.touch_validated_session(&session_id)?
            && (batch.header.is_none() || self.session_exists(&session_id).await?)
        {
            self.ensure_session_validated(&session_id).await?;
        }
        let commit = self
            .with_writer(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sql_error)?;
                admit_append(&transaction, &batch)?;
                for fact in &batch.facts {
                    insert_fact(&transaction, &batch.session_id, fact)?;
                }
                let durable_seq = advance_watermark(&transaction, &batch)?;
                transaction.commit().map_err(sql_error)?;
                Ok(AppendCommit { durable_seq })
            })
            .await?;
        self.mark_session_validated(session_id)?;
        Ok(commit)
    }

    async fn commit_agent(&self, commit: AtomicAgentCommit) -> Result<AtomicAgentCommitResult> {
        commit.validate()?;
        for append in &commit.sessions {
            if !self.touch_validated_session(&append.session_id)?
                && (append.header.is_none() || self.session_exists(&append.session_id).await?)
            {
                self.ensure_session_validated(&append.session_id).await?;
            }
        }
        let touched = commit
            .sessions
            .iter()
            .map(|append| append.session_id.clone())
            .collect::<Vec<_>>();
        let result = self
            .with_writer(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sql_error)?;
                validate_sqlite_agent_guards(&transaction, &commit)?;
                let mut sessions = Vec::with_capacity(commit.sessions.len());
                for append in commit.sessions {
                    sessions.push(apply_atomic_sqlite_append(&transaction, append)?);
                }
                transaction.commit().map_err(sql_error)?;
                Ok(AtomicAgentCommitResult { sessions })
            })
            .await?;
        for session_id in touched {
            self.mark_session_validated(session_id)?;
        }
        Ok(result)
    }

    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let projection = connection
                .query_row(
                    "SELECT length(CAST(header_json AS BLOB)),
                            CASE WHEN length(CAST(header_json AS BLOB)) <= ?2
                                 THEN header_json END
                     FROM sessions WHERE session_id = ?1",
                    params![
                        session_id.as_str(),
                        i64::try_from(MAXIMUM_SESSION_HEADER_BYTES)
                            .expect("session header bound fits SQLite INTEGER")
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
            decode_projected_json("session header", projection, MAXIMUM_SESSION_HEADER_BYTES)
        })
        .await
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage> {
        validate_read_limit(limit)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let durable_seq = transaction
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            if after_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "Fact cursor exceeds the durable tail".into(),
                ));
            }
            let page = {
                let mut statement = transaction
                    .prepare(
                        "SELECT length(CAST(fact_json AS BLOB)),
                            CASE WHEN length(CAST(fact_json AS BLOB)) <= ?4
                                 THEN fact_json END
                     FROM facts
                     WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(
                        params![
                            session_id.as_str(),
                            sqlite_u64("Fact cursor", after_seq)?,
                            i64::try_from(limit).map_err(|_| {
                                StoreError::Invalid("read limit exceeds SQLite".into())
                            })?,
                            i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                                .expect("session Fact bound fits SQLite INTEGER"),
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                    )
                    .map_err(sql_error)?;
                let mut facts = Vec::new();
                let mut encoded_bytes = 0_usize;
                for row in rows {
                    let projection = row.map_err(sql_error)?;
                    let fact: SessionFact = decode_projected_json(
                        "session Fact",
                        projection,
                        MAXIMUM_SESSION_FACT_BYTES,
                    )?;
                    let projected = encoded_bytes
                        .checked_add(fact.encoded_len())
                        .ok_or_else(|| StoreError::Corrupt("Fact page size overflow".into()))?;
                    if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                        break;
                    }
                    encoded_bytes = projected;
                    facts.push(fact);
                }
                let page = StoreFactPage {
                    after_seq,
                    facts,
                    durable_seq,
                };
                page.validate()?;
                page
            };
            transaction.commit().map_err(sql_error)?;
            Ok(page)
        })
        .await
    }

    async fn read_controls(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreControlPage> {
        validate_read_limit(limit)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let durable_seq = transaction
                .query_row(
                    "SELECT control_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
                .and_then(|value| decode_u64("control sequence", value))?;
            if after_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "control cursor exceeds the durable tail".into(),
                ));
            }
            let mut statement = transaction
                .prepare(
                    "SELECT length(CAST(control_json AS BLOB)),
                            CASE WHEN length(CAST(control_json AS BLOB)) <= ?4
                                 THEN control_json END
                     FROM agent_controls
                     WHERE session_id = ?1 AND seq > ?2 AND seq <= ?5
                     ORDER BY seq LIMIT ?3",
                )
                .map_err(sql_error)?;
            let mut rows = statement
                .query(params![
                    session_id.as_str(),
                    sqlite_u64("control cursor", after_seq)?,
                    i64::try_from(limit)
                        .map_err(|_| StoreError::Invalid("read limit exceeds SQLite".into()))?,
                    i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                        .expect("control bound fits SQLite INTEGER"),
                    sqlite_u64("control watermark", durable_seq)?,
                ])
                .map_err(sql_error)?;
            let mut records = Vec::new();
            let mut encoded_bytes = 0_usize;
            while let Some(row) = rows.next().map_err(sql_error)? {
                let record: AgentControlRecord = decode_projected_json(
                    "Agent control record",
                    (
                        row.get::<_, i64>(0).map_err(sql_error)?,
                        row.get::<_, Option<String>>(1).map_err(sql_error)?,
                    ),
                    MAXIMUM_SESSION_FACT_BYTES,
                )?;
                let projected = encoded_bytes
                    .checked_add(record.encoded_len())
                    .ok_or_else(|| StoreError::Corrupt("control page size overflow".into()))?;
                if !records.is_empty() && projected > MAXIMUM_STORE_CONTROL_PAGE_BYTES {
                    break;
                }
                encoded_bytes = projected;
                records.push(record);
            }
            drop(rows);
            drop(statement);
            let page = StoreControlPage {
                after_seq,
                records,
                durable_seq,
            };
            page.validate()?;
            transaction.commit().map_err(sql_error)?;
            Ok(page)
        })
        .await
    }

    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> Result<StoreBackwardFactPage> {
        validate_read_limit(limit)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let durable_seq = transaction
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            let maximum_before = durable_seq
                .checked_add(1)
                .ok_or_else(|| StoreError::Corrupt("durable sequence is exhausted".into()))?;
            let before_seq = if exclusive_before_seq == 0 {
                maximum_before
            } else {
                exclusive_before_seq
            };
            if before_seq > maximum_before {
                return Err(StoreError::Invalid(
                    "backward Fact cursor exceeds one past the durable tail".into(),
                ));
            }
            let page = {
                let mut statement = transaction
                    .prepare(
                        "SELECT length(CAST(fact_json AS BLOB)),
                                CASE WHEN length(CAST(fact_json AS BLOB)) <= ?4
                                     THEN fact_json END
                         FROM facts
                         WHERE session_id = ?1 AND seq < ?2
                         ORDER BY seq DESC LIMIT ?3",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(
                        params![
                            session_id.as_str(),
                            sqlite_u64("backward Fact cursor", before_seq)?,
                            i64::try_from(limit).map_err(|_| {
                                StoreError::Invalid("read limit exceeds SQLite".into())
                            })?,
                            i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                                .expect("session Fact bound fits SQLite INTEGER"),
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                    )
                    .map_err(sql_error)?;
                let mut facts = Vec::new();
                let mut encoded_bytes = 0_usize;
                for row in rows {
                    let fact: SessionFact = decode_projected_json(
                        "session Fact",
                        row.map_err(sql_error)?,
                        MAXIMUM_SESSION_FACT_BYTES,
                    )?;
                    let projected =
                        encoded_bytes
                            .checked_add(fact.encoded_len())
                            .ok_or_else(|| {
                                StoreError::Corrupt("backward Fact page size overflow".into())
                            })?;
                    if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                        break;
                    }
                    encoded_bytes = projected;
                    facts.push(fact);
                }
                facts.reverse();
                let has_more = facts.first().is_some_and(|fact| fact.seq() > 1);
                let page = StoreBackwardFactPage {
                    before_seq,
                    facts,
                    durable_seq,
                    has_more,
                };
                page.validate()?;
                page
            };
            transaction.commit().map_err(sql_error)?;
            Ok(page)
        })
        .await
    }

    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage> {
        validate_read_limit(limit)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        let turn_id = turn_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let durable_seq = transaction
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            if after_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "turn Fact cursor exceeds the durable tail".into(),
                ));
            }
            let turn_exists = transaction
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM turns WHERE session_id = ?1 AND turn_id = ?2
                     )",
                    params![session_id.as_str(), turn_id.as_str()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sql_error)?;
            if !turn_exists {
                return Err(StoreError::TurnNotFound {
                    session: session_id.to_string(),
                    turn: turn_id.to_string(),
                });
            }
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("turn read limit exceeds SQLite".into()))?;
            let page = {
                let mut statement = transaction
                    .prepare(
                        "SELECT length(CAST(fact_json AS BLOB)),
                            CASE WHEN length(CAST(fact_json AS BLOB)) <= ?5
                                 THEN fact_json END
                     FROM facts
                     WHERE session_id = ?1 AND turn_id = ?2 AND seq > ?3
                     ORDER BY seq LIMIT ?4",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(
                        params![
                            session_id.as_str(),
                            turn_id.as_str(),
                            sqlite_u64("turn Fact cursor", after_seq)?,
                            sqlite_limit,
                            i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                                .expect("session Fact bound fits SQLite INTEGER"),
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                    )
                    .map_err(sql_error)?;
                let mut facts = Vec::new();
                let mut encoded_bytes = 0_usize;
                let mut has_more = false;
                for row in rows {
                    if facts.len() == limit {
                        has_more = true;
                        break;
                    }
                    let projection = row.map_err(sql_error)?;
                    let fact: SessionFact = decode_projected_json(
                        "session Fact",
                        projection,
                        MAXIMUM_SESSION_FACT_BYTES,
                    )?;
                    let projected =
                        encoded_bytes
                            .checked_add(fact.encoded_len())
                            .ok_or_else(|| {
                                StoreError::Corrupt("turn Fact page size overflow".into())
                            })?;
                    if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                        has_more = true;
                        break;
                    }
                    encoded_bytes = projected;
                    facts.push(fact);
                }
                let page = StoreTurnFactPage {
                    turn_id,
                    after_seq,
                    facts,
                    durable_seq,
                    has_more,
                };
                page.validate()?;
                page
            };
            transaction.commit().map_err(sql_error)?;
            Ok(page)
        })
        .await
    }

    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<StoreTurnBoundary> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        let turn_id = turn_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let indexed = transaction
                .query_row(
                    "SELECT session.durable_seq, turn.accepted_seq, turn.terminal_seq
                     FROM sessions AS session
                     LEFT JOIN turns AS turn
                       ON turn.session_id = session.session_id AND turn.turn_id = ?2
                     WHERE session.session_id = ?1",
                    params![session_id.as_str(), turn_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
            let durable_seq = decode_u64("durable sequence", indexed.0)?;
            let accepted_seq = indexed.1.ok_or_else(|| StoreError::TurnNotFound {
                session: session_id.to_string(),
                turn: turn_id.to_string(),
            })?;
            let accepted = read_indexed_fact(&transaction, &session_id, accepted_seq)?;
            let terminal = indexed
                .2
                .map(|seq| read_indexed_fact(&transaction, &session_id, seq))
                .transpose()?;
            let boundary = StoreTurnBoundary::new(turn_id, accepted, terminal, durable_seq)?;
            transaction.commit().map_err(sql_error)?;
            Ok(boundary)
        })
        .await
    }

    async fn resolve_fork_boundary(
        &self,
        session_id: &SessionId,
        invoking_turn_id: &TurnId,
        selection: ForkTurnSelection,
    ) -> Result<StoreForkBoundary> {
        selection
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        let invoking_turn_id = invoking_turn_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let invoking_accepted_seq = transaction
                .query_row(
                    "SELECT accepted_seq FROM turns WHERE session_id = ?1 AND turn_id = ?2",
                    params![session_id.as_str(), invoking_turn_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::TurnNotFound {
                    session: session_id.to_string(),
                    turn: invoking_turn_id.to_string(),
                })
                .and_then(|value| decode_u64("invoking acceptance sequence", value))?;
            let available_completed_turns = transaction
                .query_row(
                    "SELECT COUNT(*) FROM turns
                     WHERE session_id = ?1 AND terminal_seq IS NOT NULL AND terminal_seq < ?2",
                    params![
                        session_id.as_str(),
                        sqlite_u64("invoking acceptance sequence", invoking_accepted_seq)?,
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sql_error)
                .and_then(|value| decode_u64("completed turn count", value))?;
            let effective_turns = match selection {
                ForkTurnSelection::None => 0,
                ForkTurnSelection::All => available_completed_turns,
                ForkTurnSelection::Last(count) => available_completed_turns.min(count),
            };
            let (resolved_after_seq, resolved_terminal_seq, terminal_prefix_sha256) =
                if effective_turns == 0 {
                    (0, 0, hex::encode(EMPTY_FACT_PREFIX_DIGEST))
                } else {
                    let (sequence, digest) = transaction
                    .query_row(
                        "SELECT terminal_seq, terminal_prefix_sha256 FROM turns
                         WHERE session_id = ?1 AND terminal_seq IS NOT NULL AND terminal_seq < ?2
                         ORDER BY terminal_seq DESC LIMIT 1",
                        params![
                            session_id.as_str(),
                            sqlite_u64("invoking acceptance sequence", invoking_accepted_seq)?,
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                    )
                    .map_err(sql_error)?;
                    let resolved_terminal_seq =
                        decode_u64("fork terminal sequence", sequence)?;
                    let first_accepted = transaction
                        .query_row(
                            "SELECT accepted_seq FROM (
                               SELECT accepted_seq, terminal_seq FROM turns
                               WHERE session_id = ?1 AND terminal_seq IS NOT NULL AND terminal_seq < ?2
                               ORDER BY terminal_seq DESC LIMIT ?3
                             ) ORDER BY terminal_seq ASC LIMIT 1",
                            params![
                                session_id.as_str(),
                                sqlite_u64(
                                    "invoking acceptance sequence",
                                    invoking_accepted_seq,
                                )?,
                                sqlite_u64("effective fork turn count", effective_turns)?,
                            ],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(sql_error)
                        .and_then(|value| decode_u64("first inherited acceptance", value))?;
                    let resolved_after_seq = first_accepted.checked_sub(1).ok_or_else(|| {
                        StoreError::Corrupt("first inherited acceptance is zero".into())
                    })?;
                    let interval_turns = transaction
                        .query_row(
                            "SELECT COUNT(*) FROM turns
                             WHERE session_id = ?1 AND accepted_seq > ?2 AND accepted_seq <= ?3",
                            params![
                                session_id.as_str(),
                                sqlite_u64("fork interval start", resolved_after_seq)?,
                                sqlite_u64("fork interval end", resolved_terminal_seq)?,
                            ],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(sql_error)
                        .and_then(|value| decode_u64("fork interval turn count", value))?;
                    let interval_fact_turns = transaction
                        .query_row(
                            "SELECT COUNT(DISTINCT turn_id) FROM facts
                             WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3",
                            params![
                                session_id.as_str(),
                                sqlite_u64("fork interval start", resolved_after_seq)?,
                                sqlite_u64("fork interval end", resolved_terminal_seq)?,
                            ],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(sql_error)
                        .and_then(|value| {
                            decode_u64("fork interval Fact turn count", value)
                        })?;
                    if interval_turns != effective_turns
                        || interval_fact_turns != effective_turns
                    {
                        return Err(StoreError::Invalid(
                            "fork selection does not form a balanced contiguous completed-turn interval"
                                .into(),
                        ));
                    }
                    validate_sha256("terminal-prefix digest", &digest)?;
                    (resolved_after_seq, resolved_terminal_seq, digest)
                };
            let boundary = StoreForkBoundary {
                resolved_after_seq,
                resolved_terminal_seq,
                terminal_prefix_sha256,
                effective_turns,
            };
            transaction.commit().map_err(sql_error)?;
            Ok(boundary)
        })
        .await
    }

    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> Result<StoreOpenTurnPage> {
        validate_read_limit(limit)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let durable_seq = transaction
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            if after_accepted_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "open-turn cursor exceeds the durable tail".into(),
                ));
            }
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("open-turn limit exceeds SQLite".into()))?;
            let page = {
                let mut statement = transaction
                    .prepare(
                        "SELECT turn_id, accepted_seq FROM turns
                     WHERE session_id = ?1 AND terminal_seq IS NULL
                       AND accepted_seq > ?2
                     ORDER BY accepted_seq LIMIT ?3",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(
                        params![
                            session_id.as_str(),
                            sqlite_u64("open-turn cursor", after_accepted_seq)?,
                            sqlite_limit,
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .map_err(sql_error)?;
                let mut turns = Vec::with_capacity(limit + 1);
                for row in rows {
                    let (turn_id, accepted_seq) = row.map_err(sql_error)?;
                    turns.push(StoreOpenTurn {
                        turn_id: TurnId::new(turn_id)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                        accepted_seq: decode_u64("turn acceptance sequence", accepted_seq)?,
                    });
                }
                let has_more = turns.len() > limit;
                turns.truncate(limit);
                let page = StoreOpenTurnPage {
                    after_accepted_seq,
                    turns,
                    durable_seq,
                    has_more,
                };
                page.validate()?;
                page
            };
            transaction.commit().map_err(sql_error)?;
            Ok(page)
        })
        .await
    }

    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<rsi_agent_store_protocol::StoreSessionPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("session read limit exceeds SQLite".into()))?;
            let mut sessions = Vec::with_capacity(limit + 1);
            if let Some(after) = &after {
                let mut statement = connection
                    .prepare(
                        "SELECT session_id FROM sessions
                         WHERE session_id > ?1 ORDER BY session_id LIMIT ?2",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(params![after.as_str(), sqlite_limit], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(sql_error)?;
                for row in rows {
                    let value = row.map_err(sql_error)?;
                    sessions.push(
                        SessionId::new(value)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            } else {
                let mut statement = connection
                    .prepare("SELECT session_id FROM sessions ORDER BY session_id LIMIT ?1")
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map([sqlite_limit], |row| row.get::<_, String>(0))
                    .map_err(sql_error)?;
                for row in rows {
                    let value = row.map_err(sql_error)?;
                    sessions.push(
                        SessionId::new(value)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            }
            let has_more = sessions.len() > limit;
            sessions.truncate(limit);
            let page = rsi_agent_store_protocol::StoreSessionPage {
                after,
                sessions,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn list_recent_sessions(
        &self,
        after: Option<&StoreRecentSessionCursor>,
        limit: usize,
    ) -> Result<StoreRecentSessionPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)?;
        let after = after.cloned();
        let validated_sessions = Arc::clone(&self.validated_sessions);
        #[cfg(test)]
        let validation_runs = Arc::clone(&self.validation_runs);
        let page = self
            .with_reader(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Deferred)
                    .map_err(sql_error)?;
                let sqlite_limit = i64::try_from(limit + 1)
                    .map_err(|_| StoreError::Invalid("session read limit exceeds SQLite".into()))?;
                let mut projections = Vec::with_capacity(limit + 1);
                if let Some(after) = &after {
                    let mut statement = transaction
                        .prepare(
                            "SELECT session_id, created_at_ms FROM sessions
                         WHERE (created_at_ms, session_id) < (?1, ?2)
                         ORDER BY created_at_ms DESC, session_id DESC LIMIT ?3",
                        )
                        .map_err(sql_error)?;
                    let rows = statement
                        .query_map(
                            params![
                                sqlite_u64("recent-session cursor timestamp", after.created_at_ms)?,
                                after.session_id.as_str(),
                                sqlite_limit,
                            ],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                        )
                        .map_err(sql_error)?;
                    for row in rows {
                        let (session_id, created_at_ms) = row.map_err(sql_error)?;
                        projections.push((session_id, created_at_ms));
                    }
                } else {
                    let mut statement = transaction
                        .prepare(
                            "SELECT session_id, created_at_ms FROM sessions
                         ORDER BY created_at_ms DESC, session_id DESC LIMIT ?1",
                        )
                        .map_err(sql_error)?;
                    let rows = statement
                        .query_map([sqlite_limit], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                        })
                        .map_err(sql_error)?;
                    for row in rows {
                        let (session_id, created_at_ms) = row.map_err(sql_error)?;
                        projections.push((session_id, created_at_ms));
                    }
                }
                let has_more = projections.len() > limit;
                projections.truncate(limit);
                let mut sessions = Vec::with_capacity(projections.len());
                for (encoded_session_id, created_at_ms) in projections {
                    let session_id = SessionId::new(encoded_session_id)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                    let cached = validated_sessions
                        .lock()
                        .map_err(|_| {
                            StoreError::Io("validated-session cache mutex was poisoned".into())
                        })?
                        .touch(&session_id);
                    let header = if cached {
                        read_session_header_row(&transaction, &session_id)?.0
                    } else {
                        #[cfg(test)]
                        validation_runs.fetch_add(1, Ordering::Relaxed);
                        validate_session(&transaction, &session_id)?
                    };
                    if header.created_at_ms()
                        != decode_u64("session creation timestamp", created_at_ms)?
                    {
                        return Err(StoreError::Corrupt(
                            "recent-session ordering timestamp differs from its durable header"
                                .into(),
                        ));
                    }
                    sessions.push(StoreRecentSession { header });
                }
                let page = StoreRecentSessionPage {
                    after,
                    sessions,
                    has_more,
                };
                page.validate()?;
                transaction.commit().map_err(sql_error)?;
                Ok(page)
            })
            .await?;
        for session in &page.sessions {
            self.mark_session_validated(session.header.session_id().clone())?;
        }
        Ok(page)
    }

    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<rsi_agent_store_protocol::StoreSessionPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("session read limit exceeds SQLite".into()))?;
            let mut sessions = Vec::with_capacity(limit + 1);
            if let Some(after) = &after {
                let mut statement = connection
                    .prepare(
                        "SELECT DISTINCT session_id FROM turns
                         WHERE terminal_seq IS NULL AND session_id > ?1
                         ORDER BY session_id LIMIT ?2",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(params![after.as_str(), sqlite_limit], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(sql_error)?;
                for row in rows {
                    sessions.push(
                        SessionId::new(row.map_err(sql_error)?)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            } else {
                let mut statement = connection
                    .prepare(
                        "SELECT DISTINCT session_id FROM turns
                         WHERE terminal_seq IS NULL
                         ORDER BY session_id LIMIT ?1",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map([sqlite_limit], |row| row.get::<_, String>(0))
                    .map_err(sql_error)?;
                for row in rows {
                    sessions.push(
                        SessionId::new(row.map_err(sql_error)?)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            }
            let has_more = sessions.len() > limit;
            sessions.truncate(limit);
            let page = rsi_agent_store_protocol::StoreSessionPage {
                after,
                sessions,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn list_ready_messages(
        &self,
        root_session_id: &SessionId,
        after: Option<&StoreReadyMessageCursor>,
        limit: usize,
    ) -> Result<StoreReadyMessagePage> {
        validate_session_read_limit(limit)?;
        let root_session_id = root_session_id.clone();
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sql = if after.is_some() {
                LIST_READY_MESSAGES_AFTER_SQL
            } else {
                LIST_READY_MESSAGES_FIRST_SQL
            };
            let mut statement = connection.prepare(sql).map_err(sql_error)?;
            let fetch_limit = limit
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("ready read limit overflowed".into()))?;
            let (after_timestamp, after_session, after_seq) =
                after.as_ref().map_or((None, None, None), |cursor| {
                    (
                        Some(sqlite_u64("ready timestamp", cursor.timestamp_ms)),
                        Some(cursor.session_id.as_str()),
                        Some(sqlite_u64("ready control sequence", cursor.control_seq)),
                    )
                });
            let rows = statement
                .query_map(
                    params![
                        root_session_id.as_str(),
                        after_timestamp.transpose()?,
                        after_session,
                        after_seq.transpose()?,
                        i64::try_from(fetch_limit).map_err(|_| {
                            StoreError::Invalid("ready read limit exceeds SQLite".into())
                        })?,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .map_err(sql_error)?;
            let mut messages = rows
                .map(|row| decode_ready_message(row.map_err(sql_error)?))
                .collect::<Result<Vec<_>>>()?;
            let has_more = messages.len() > limit;
            messages.truncate(limit);
            let page = StoreReadyMessagePage {
                after,
                messages,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn read_agent_mailbox(
        &self,
        session_id: &SessionId,
        selected_message_id: Option<&MessageId>,
    ) -> Result<StoreAgentMailbox> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        let selected_message_id = selected_message_id.cloned();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let (durable_fact_seq, durable_control_seq) = transaction
                .query_row(
                    "SELECT durable_seq, control_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
            let durable_fact_seq = decode_u64("mailbox durable Fact sequence", durable_fact_seq)?;
            let durable_control_seq =
                decode_u64("mailbox durable control sequence", durable_control_seq)?;
            let selected = selected_message_id
                .as_ref()
                .map(|message_id| {
                    transaction
                        .query_row(
                            "SELECT length(CAST(message_json AS BLOB)),
                                    CASE WHEN length(CAST(message_json AS BLOB)) <= ?3
                                         THEN message_json END,
                                    message_source, root_session_id, target, wake_required,
                                    accepted_control_seq, state,
                                    length(CAST(state_json AS BLOB)),
                                    CASE WHEN length(CAST(state_json AS BLOB)) <= ?4
                                         THEN state_json END
                             FROM agent_messages
                             WHERE session_id = ?1 AND message_id = ?2",
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
                        .map_err(sql_error)
                        .and_then(|row| row.map(decode_indexed_message).transpose())
                })
                .transpose()?
                .flatten();
            let pending_count = transaction
                .query_row(
                    "SELECT COUNT(*) FROM agent_messages
                     WHERE session_id = ?1 AND state = 'pending'",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sql_error)
                .and_then(|count| {
                    usize::try_from(count).map_err(|_| {
                        StoreError::Corrupt("mailbox pending count is outside usize".into())
                    })
                })?;
            let mut statement = transaction
                .prepare(
                    "SELECT length(CAST(message_json AS BLOB)),
                            CASE WHEN length(CAST(message_json AS BLOB)) <= ?2
                                 THEN message_json END,
                            message_source, root_session_id, target, wake_required,
                            accepted_control_seq, state,
                            length(CAST(state_json AS BLOB)),
                            CASE WHEN length(CAST(state_json AS BLOB)) <= ?3
                                 THEN state_json END
                     FROM agent_messages
                     WHERE session_id = ?1 AND state = 'pending'
                     ORDER BY accepted_control_seq",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        session_id.as_str(),
                        i64::try_from(MAXIMUM_STORE_MAILBOX_PAGE_BYTES)
                            .expect("mailbox page bound fits SQLite INTEGER"),
                        i64::try_from(MAXIMUM_INDEXED_MESSAGE_STATE_BYTES)
                            .expect("message state bound fits SQLite INTEGER"),
                    ],
                    indexed_message_row,
                )
                .map_err(sql_error)?;
            let mut pending = Vec::new();
            let mut encoded_bytes = 0_usize;
            for row in rows {
                let entry = decode_indexed_message(row.map_err(sql_error)?)?;
                let projected = encoded_bytes
                    .checked_add(entry.encoded_message_bytes)
                    .ok_or_else(|| StoreError::Corrupt("mailbox byte count overflowed".into()))?;
                if projected > MAXIMUM_STORE_MAILBOX_PAGE_BYTES {
                    break;
                }
                encoded_bytes = projected;
                pending.push(entry);
            }
            let mailbox = StoreAgentMailbox {
                selected,
                pending,
                pending_count,
                durable_control_seq,
                durable_fact_seq,
            };
            mailbox.validate()?;
            drop(statement);
            transaction.commit().map_err(sql_error)?;
            Ok(mailbox)
        })
        .await
    }

    async fn read_agent_mailbox_summary(
        &self,
        session_id: &SessionId,
    ) -> Result<StoreAgentMailboxSummary> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let (durable_fact_seq, durable_control_seq, pending_count) = transaction
                .query_row(
                    "SELECT durable_seq, control_seq,
                            (SELECT COUNT(*) FROM agent_messages
                             WHERE session_id = ?1 AND state = 'pending')
                     FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
            let mut statement = transaction
                .prepare(
                    "SELECT message_id FROM agent_messages
                     WHERE session_id = ?1 AND state = 'pending'
                       AND target = 'next_step' AND wake_required = 0
                       AND message_source = 'completion'
                     ORDER BY accepted_control_seq LIMIT ?2",
                )
                .map_err(sql_error)?;
            let pending_next_step_completion_message_ids = statement
                .query_map(
                    params![
                        session_id.as_str(),
                        i64::try_from(
                            rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES + 1
                        )
                        .expect("pending-message overflow probe fits SQLite INTEGER"),
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(sql_error)?
                .map(|row| {
                    MessageId::new(row.map_err(sql_error)?)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            let summary = StoreAgentMailboxSummary {
                pending_count: usize::try_from(pending_count).map_err(|_| {
                    StoreError::Corrupt("mailbox pending count is outside usize".into())
                })?,
                pending_next_step_completion_message_ids,
                durable_control_seq: decode_u64(
                    "mailbox durable control sequence",
                    durable_control_seq,
                )?,
                durable_fact_seq: decode_u64("mailbox durable Fact sequence", durable_fact_seq)?,
            };
            summary.validate()?;
            drop(statement);
            transaction.commit().map_err(sql_error)?;
            Ok(summary)
        })
        .await
    }

    async fn read_workspace_context_state(
        &self,
        session_id: &SessionId,
    ) -> Result<StoreWorkspaceContextState> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let (instructions_sha256, skill_catalog_sha256, durable_fact_seq) = connection
                .query_row(
                    "SELECT workspace_instructions_sha256,
                            workspace_skill_catalog_sha256, durable_seq
                     FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)?)),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
            let state = StoreWorkspaceContextState {
                instructions_sha256,
                skill_catalog_sha256,
                durable_fact_seq: decode_u64(
                    "workspace-context durable Fact sequence",
                    durable_fact_seq,
                )?,
            };
            state.validate()?;
            Ok(state)
        })
        .await
    }

    async fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreAgentChildPage> {
        validate_session_read_limit(limit)?;
        let parent_session_id = parent_session_id.clone();
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sql = if after.is_some() {
                LIST_AGENT_CHILDREN_AFTER_SQL
            } else {
                LIST_AGENT_CHILDREN_FIRST_SQL
            };
            let mut statement = connection.prepare(sql).map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        parent_session_id.as_str(),
                        after.as_ref().map(SessionId::as_str),
                        i64::try_from(limit + 1).map_err(|_| {
                            StoreError::Invalid("Agent-child read limit exceeds SQLite".into())
                        })?,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(sql_error)?;
            let mut children = rows
                .map(|row| {
                    let (session_id, path, task_name) = row.map_err(sql_error)?;
                    Ok(StoreAgentChild {
                        session_id: SessionId::new(session_id)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                        path: decode_json("Agent path", &path)?,
                        task_name,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let has_more = children.len() > limit;
            children.truncate(limit);
            let page = StoreAgentChildPage {
                after,
                children,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn read_descendant_control_snapshot(
        &self,
        parent_session_id: &SessionId,
    ) -> Result<StoreDescendantControlSnapshot> {
        self.ensure_session_validated(parent_session_id).await?;
        let parent_session_id = parent_session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let mut statement = transaction
                .prepare(
                    "WITH RECURSIVE descendants(session_id) AS (
                         SELECT session_id FROM agent_nodes WHERE parent_session_id = ?1
                         UNION
                         SELECT child.session_id
                         FROM agent_nodes AS child
                         JOIN descendants AS parent
                           ON child.parent_session_id = parent.session_id
                     )
                     SELECT descendants.session_id,
                            (SELECT control_seq FROM sessions
                             WHERE session_id = descendants.session_id)
                     FROM descendants
                     ORDER BY descendants.session_id
                     LIMIT ?2",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        parent_session_id.as_str(),
                        i64::try_from(MAXIMUM_DURABLE_AGENT_TREE_NODES)
                            .expect("durable Agent-tree bound fits SQLite INTEGER"),
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(sql_error)?;
            let descendants = rows
                .map(|row| {
                    let (session_id, durable_control_seq) = row.map_err(sql_error)?;
                    Ok(StoreDescendantControlWatermark {
                        session_id: SessionId::new(session_id)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                        durable_control_seq: decode_u64(
                            "descendant control sequence",
                            durable_control_seq,
                        )?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            drop(statement);
            let snapshot = StoreDescendantControlSnapshot { descendants };
            snapshot.validate()?;
            transaction.commit().map_err(sql_error)?;
            Ok(snapshot)
        })
        .await
    }

    async fn active_activation(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoreActiveActivation>> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            connection
                .query_row(
                    "SELECT activation_id, parent_session_id, turn_id, phase,
                            completion_reserved_bytes
                     FROM active_activations WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?
                .map(
                    |(activation_id, parent_session_id, turn_id, phase, reserved)| {
                        Ok(StoreActiveActivation {
                            activation_id: rsi_agent_session_protocol::ActivationId::new(
                                activation_id,
                            )
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                            parent_session_id: parent_session_id
                                .map(SessionId::new)
                                .transpose()
                                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                            turn_id: turn_id
                                .map(TurnId::new)
                                .transpose()
                                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                            phase: match phase.as_str() {
                                "running" => StoreActivationPhase::Running,
                                "parked" => StoreActivationPhase::Parked,
                                "waiting" => StoreActivationPhase::WaitingForDescendants,
                                _ => {
                                    return Err(StoreError::Corrupt(
                                        "active activation phase is invalid".into(),
                                    ));
                                }
                            },
                            completion_reserved_bytes: reserved
                                .map(|value| decode_u64("completion reservation bytes", value))
                                .transpose()?,
                        })
                    },
                )
                .transpose()
        })
        .await
    }

    async fn completion_reservation_count(&self, parent_session_id: &SessionId) -> Result<usize> {
        let parent_session_id = parent_session_id.clone();
        self.with_reader(move |connection| {
            let count = connection
                .query_row(
                    "SELECT COUNT(*) FROM active_activations
                     WHERE parent_session_id = ?1
                       AND completion_reserved_bytes IS NOT NULL",
                    [parent_session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sql_error)?;
            usize::try_from(count).map_err(|_| {
                StoreError::Corrupt("completion reservation count exceeds usize".into())
            })
        })
        .await
    }

    async fn list_waiting_activations(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreWaitingActivationPage> {
        validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sql = if after.is_some() {
                LIST_WAITING_ACTIVATIONS_AFTER_SQL
            } else {
                LIST_WAITING_ACTIVATIONS_FIRST_SQL
            };
            let mut statement = connection.prepare(sql).map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        after.as_ref().map(SessionId::as_str),
                        i64::try_from(limit + 1).map_err(|_| {
                            StoreError::Invalid("waiting-activation limit exceeds SQLite".into())
                        })?,
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(sql_error)?;
            let mut sessions = rows
                .map(|row| {
                    SessionId::new(row.map_err(sql_error)?)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            let has_more = sessions.len() > limit;
            sessions.truncate(limit);
            let page = StoreWaitingActivationPage {
                after,
                sessions,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn list_ready_roots(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreReadyRootPage> {
        validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sql = if after.is_some() {
                LIST_READY_ROOTS_AFTER_SQL
            } else {
                LIST_READY_ROOTS_FIRST_SQL
            };
            let mut statement = connection.prepare(sql).map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        after.as_ref().map(SessionId::as_str),
                        i64::try_from(limit + 1).map_err(|_| {
                            StoreError::Invalid("ready-root read limit exceeds SQLite".into())
                        })?,
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(sql_error)?;
            let mut roots = rows
                .map(|row| {
                    SessionId::new(row.map_err(sql_error)?)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            let has_more = roots.len() > limit;
            roots.truncate(limit);
            let page = StoreReadyRootPage {
                after,
                roots,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoredContextCheckpoint>> {
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(sql_error)?
                .is_some();
            if !exists {
                return Err(StoreError::NotFound(session_id.to_string()));
            }
            let projection = transaction
                .query_row(
                    "SELECT c.header_fingerprint, c.through_seq, c.fact_prefix_sha256,
                            length(c.checkpoint_bytes),
                            CASE WHEN length(c.checkpoint_bytes) <= ?2
                                 THEN c.checkpoint_bytes END,
                            length(CAST(s.header_json AS BLOB)),
                            CASE WHEN length(CAST(s.header_json AS BLOB)) <= ?3
                                 THEN s.header_json END,
                            s.durable_seq
                     FROM context_checkpoints c
                     JOIN sessions s ON s.session_id = c.session_id
                     WHERE c.session_id = ?1",
                    params![
                        session_id.as_str(),
                        i64::try_from(MAXIMUM_CONTEXT_CHECKPOINT_BYTES)
                            .expect("checkpoint bound fits SQLite INTEGER"),
                        i64::try_from(MAXIMUM_SESSION_HEADER_BYTES)
                            .expect("session header bound fits SQLite INTEGER"),
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Option<Vec<u8>>>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?;
            let checkpoint = projection.map(decode_context_checkpoint).transpose()?;
            transaction.commit().map_err(sql_error)?;
            Ok(checkpoint)
        })
        .await
    }

    async fn write_context_checkpoint(&self, write: WriteContextCheckpoint) -> Result<()> {
        write.validate()?;
        self.ensure_session_validated(&write.session_id).await?;
        self.with_writer(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let (actual, fact_prefix_sha256, header_encoded_len, header_json) = transaction
                .query_row(
                    "SELECT durable_seq, fact_prefix_sha256,
                            length(CAST(header_json AS BLOB)),
                            CASE WHEN length(CAST(header_json AS BLOB)) <= ?2
                                 THEN header_json END
                     FROM sessions WHERE session_id = ?1",
                    params![
                        write.session_id.as_str(),
                        i64::try_from(MAXIMUM_SESSION_HEADER_BYTES)
                            .expect("session header bound fits SQLite INTEGER"),
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(write.session_id.to_string()))
                .and_then(|(value, digest, header_len, header)| {
                    Ok((
                        decode_u64("durable sequence", value)?,
                        digest,
                        header_len,
                        header,
                    ))
                })?;
            if actual != write.expected_durable_seq {
                return Err(StoreError::Conflict {
                    expected: write.expected_durable_seq,
                    actual,
                });
            }
            let header: SessionHeader = decode_projected_json(
                "session header",
                (header_encoded_len, header_json),
                MAXIMUM_SESSION_HEADER_BYTES,
            )?;
            if write.checkpoint.header_fingerprint
                != header.fingerprint().map_err(|error| {
                    StoreError::Corrupt(format!("stored session header is invalid: {error}"))
                })?
            {
                return Err(StoreError::Invalid(
                    "checkpoint header fingerprint differs from the durable session".into(),
                ));
            }
            validate_sha256("Fact-prefix digest", &fact_prefix_sha256)?;
            if write.checkpoint.fact_prefix_sha256 != fact_prefix_sha256 {
                return Err(StoreError::Invalid(
                    "checkpoint Fact-prefix digest differs from the durable session".into(),
                ));
            }
            transaction
                .execute(
                    "INSERT INTO context_checkpoints
                         (session_id, header_fingerprint, through_seq,
                          fact_prefix_sha256, checkpoint_bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(session_id) DO UPDATE SET
                         header_fingerprint = excluded.header_fingerprint,
                         through_seq = excluded.through_seq,
                         fact_prefix_sha256 = excluded.fact_prefix_sha256,
                         checkpoint_bytes = excluded.checkpoint_bytes",
                    params![
                        write.session_id.as_str(),
                        write.checkpoint.header_fingerprint,
                        sqlite_u64("checkpoint sequence", write.checkpoint.through_seq)?,
                        write.checkpoint.fact_prefix_sha256,
                        write.checkpoint.bytes.as_ref(),
                    ],
                )
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)
        })
        .await
    }

    async fn put_cas(&self, bytes: Arc<[u8]>) -> Result<CasObjectRef> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_STORE_CAS_BYTES {
            return Err(StoreError::Invalid(
                "CAS bytes must be nonempty and bounded".into(),
            ));
        }
        let cas_dir = Arc::clone(&self.cas_dir);
        let cas_staging_dir = Arc::clone(&self.cas_staging_dir);
        let reference = self
            .with_cas(move || {
                let reference = CasObjectRef {
                    sha256: hex::encode(Sha256::digest(&bytes)),
                    byte_len: u64::try_from(bytes.len())
                        .map_err(|_| StoreError::Invalid("CAS length exceeds u64".into()))?,
                };
                reference.validate()?;
                install_cas(&cas_dir, &cas_staging_dir, &reference.sha256, &bytes)?;
                Ok(reference)
            })
            .await?;
        self.with_writer(move |connection| {
            if let Some(existing) = connection
                .query_row(
                    "SELECT byte_len FROM cas_objects WHERE sha256 = ?1",
                    [&reference.sha256],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
            {
                if decode_u64("CAS byte length", existing)? != reference.byte_len {
                    return Err(StoreError::Corrupt(
                        "CAS metadata conflicts with existing digest".into(),
                    ));
                }
            } else {
                connection
                    .execute(
                        "INSERT INTO cas_objects (sha256, byte_len) VALUES (?1, ?2)",
                        params![
                            &reference.sha256,
                            sqlite_u64("CAS byte length", reference.byte_len)?,
                        ],
                    )
                    .map_err(sql_error)?;
            }
            Ok(reference)
        })
        .await
    }

    async fn read_cas(&self, object: &CasObjectRef) -> Result<Arc<[u8]>> {
        object.validate()?;
        let object = object.clone();
        let verified = object.clone();
        self.with_reader(move |connection| {
            let byte_len = connection
                .query_row(
                    "SELECT byte_len FROM cas_objects WHERE sha256 = ?1",
                    [&object.sha256],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(object.sha256.clone()))?;
            if decode_u64("CAS byte length", byte_len)? != object.byte_len {
                return Err(StoreError::Corrupt(
                    "CAS reference disagrees with SQLite metadata".into(),
                ));
            }
            Ok(())
        })
        .await?;
        let cas_dir = Arc::clone(&self.cas_dir);
        self.with_cas(move || {
            let bytes = read_cas_file(&cas_dir, &verified.sha256)?;
            if u64::try_from(bytes.len())
                .map_err(|_| StoreError::Corrupt("CAS body length exceeds u64".into()))?
                != verified.byte_len
            {
                return Err(StoreError::Corrupt(
                    "CAS body length disagrees with metadata".into(),
                ));
            }
            Ok(Arc::from(bytes))
        })
        .await
    }
}
