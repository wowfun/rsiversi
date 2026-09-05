use super::{
    ActivationId, AgentCommitWatermark, AgentControlRecord, AgentControlRecordBody, AgentMessage,
    AgentMessageSource, AppendBatch, AtomicAgentCommit, AtomicSessionAppend, Connection,
    EMPTY_CONTROL_PREFIX_DIGEST, EMPTY_FACT_PREFIX_DIGEST, InputMessageSource,
    MAXIMUM_INDEXED_MESSAGE_STATE_BYTES, MAXIMUM_SESSION_FACT_BYTES,
    MAXIMUM_STORE_MAILBOX_PAGE_BYTES, MessageDiscardReason, MessageId, MessageTarget,
    OptionalExtension, Result, SessionFact, SessionFactBody, SessionHeader, SessionId, StepId,
    StoreAgentMessage, StoreAgentMessageState, StoreError, StoreFactTurnRole, StoreReadyMessage,
    Transaction, TurnId, advance_control_prefix_digest, advance_fact_prefix_digest,
    decode_projected_json, decode_sha256, decode_u64, encode_json, fact_index_kind, params,
    read_session_header_row, sql_error, sqlite_u64, validate_message_claim_fact,
};

pub(super) fn validate_sqlite_agent_guards(
    transaction: &Transaction<'_>,
    commit: &AtomicAgentCommit,
) -> Result<()> {
    for guard in &commit.required_active_activations {
        let matches = transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM active_activations
                   WHERE session_id = ?1 AND activation_id = ?2
                 )",
                params![guard.session_id.as_str(), guard.activation_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if !matches {
            return Err(StoreError::ActivationGuardConflict {
                session: guard.session_id.to_string(),
            });
        }
    }
    for session_id in &commit.quiescent_sessions {
        let busy = transaction
            .query_row(
                "SELECT
                   EXISTS(SELECT 1 FROM active_activations WHERE session_id = ?1)
                   OR EXISTS(
                     SELECT 1 FROM turns
                     WHERE session_id = ?1 AND terminal_seq IS NULL
                   )
                   OR EXISTS(SELECT 1 FROM ready_messages WHERE session_id = ?1)",
                [session_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if busy {
            return Err(StoreError::SessionNotQuiescent {
                session: session_id.to_string(),
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // The complete per-Session compare-and-append transaction must remain one audit unit.
pub(super) fn apply_atomic_sqlite_append(
    transaction: &Transaction<'_>,
    append: AtomicSessionAppend,
) -> Result<AgentCommitWatermark> {
    let existing = transaction
        .query_row(
            "SELECT durable_seq, control_seq FROM sessions WHERE session_id = ?1",
            [append.session_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (actual_fact, actual_control) = existing
        .map(|(fact, control)| {
            Ok((
                decode_u64("durable sequence", fact)?,
                decode_u64("control sequence", control)?,
            ))
        })
        .transpose()?
        .unwrap_or((0, 0));
    if actual_fact != append.expected_fact_seq {
        return Err(StoreError::Conflict {
            expected: append.expected_fact_seq,
            actual: actual_fact,
        });
    }
    if actual_control != append.expected_control_seq {
        return Err(StoreError::ControlConflict {
            session: append.session_id.to_string(),
            expected: append.expected_control_seq,
            actual: actual_control,
        });
    }
    match (existing, append.header.as_ref()) {
        (None, Some(header)) => {
            transaction
                .execute(
                    "INSERT INTO sessions
                        (session_id, created_at_ms, header_json, durable_seq, fact_prefix_sha256,
                         control_seq, control_prefix_sha256)
                     VALUES (?1, ?2, ?3, 0, ?4, 0, ?5)",
                    params![
                        append.session_id.as_str(),
                        sqlite_u64("session creation timestamp", header.created_at_ms())?,
                        encode_json("session header", header)?,
                        hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                        hex::encode(EMPTY_CONTROL_PREFIX_DIGEST),
                    ],
                )
                .map_err(sql_error)?;
            insert_agent_node(transaction, header)?;
        }
        (None, None) => return Err(StoreError::NotFound(append.session_id.to_string())),
        (Some(_), Some(_)) => {
            return Err(StoreError::Invalid(
                "existing session cannot replace its immutable Header".into(),
            ));
        }
        (Some(_), None) => {}
    }

    let mut fact_digest = transaction
        .query_row(
            "SELECT fact_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [append.session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)
        .and_then(|digest| decode_sha256("Fact-prefix digest", &digest))?;
    for fact in &append.facts {
        insert_fact(transaction, &append.session_id, fact)?;
        fact_digest = advance_fact_prefix_digest(fact_digest, fact)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        if matches!(fact.body(), SessionFactBody::TurnTerminal { .. }) {
            let changed = transaction
                .execute(
                    "UPDATE turns SET terminal_prefix_sha256 = ?1
                     WHERE session_id = ?2 AND turn_id = ?3 AND terminal_seq = ?4",
                    params![
                        hex::encode(fact_digest),
                        append.session_id.as_str(),
                        fact.body().turn_id().as_str(),
                        sqlite_u64("turn terminal sequence", fact.seq())?,
                    ],
                )
                .map_err(sql_error)?;
            if changed != 1 {
                return Err(StoreError::Corrupt(
                    "SQLite lost an atomic terminal-prefix predicate".into(),
                ));
            }
        }
    }
    let durable_fact_seq = append.facts.last().map_or(actual_fact, SessionFact::seq);
    let minimum_entered_fact_seq = append
        .expected_fact_seq
        .checked_add(1)
        .ok_or_else(|| StoreError::Invalid("Fact sequence is exhausted".into()))?;

    let mut control_digest = transaction
        .query_row(
            "SELECT control_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [append.session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)
        .and_then(|digest| decode_sha256("control-prefix digest", &digest))?;
    for record in &append.controls {
        insert_control(
            transaction,
            &append.session_id,
            minimum_entered_fact_seq,
            record,
        )?;
        control_digest = advance_control_prefix_digest(control_digest, record)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
    }
    let durable_control_seq = append
        .controls
        .last()
        .map_or(actual_control, AgentControlRecord::seq);
    let changed = transaction
        .execute(
            "UPDATE sessions
             SET durable_seq = ?1, fact_prefix_sha256 = ?2,
                 control_seq = ?3, control_prefix_sha256 = ?4
             WHERE session_id = ?5 AND durable_seq = ?6 AND control_seq = ?7",
            params![
                sqlite_u64("durable sequence", durable_fact_seq)?,
                hex::encode(fact_digest),
                sqlite_u64("control sequence", durable_control_seq)?,
                hex::encode(control_digest),
                append.session_id.as_str(),
                sqlite_u64("expected sequence", append.expected_fact_seq)?,
                sqlite_u64("expected control sequence", append.expected_control_seq)?,
            ],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(StoreError::Corrupt(
            "SQLite lost an atomic Agent commit predicate".into(),
        ));
    }
    Ok(AgentCommitWatermark {
        session_id: append.session_id,
        durable_fact_seq,
        durable_control_seq,
    })
}

pub(super) fn insert_control(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    minimum_entered_fact_seq: u64,
    record: &AgentControlRecord,
) -> Result<()> {
    ControlIndexer {
        transaction,
        session_id,
        minimum_entered_fact_seq,
        record,
    }
    .insert()
}

pub(super) struct ControlIndexer<'a, 'connection> {
    transaction: &'a Transaction<'connection>,
    session_id: &'a SessionId,
    minimum_entered_fact_seq: u64,
    record: &'a AgentControlRecord,
}

impl ControlIndexer<'_, '_> {
    fn insert(&self) -> Result<()> {
        self.transaction
            .execute(
                "INSERT INTO agent_controls (session_id, seq, control_json) VALUES (?1, ?2, ?3)",
                params![
                    self.session_id.as_str(),
                    sqlite_u64("control sequence", self.record.seq())?,
                    encode_json("Agent control record", self.record)?,
                ],
            )
            .map_err(sql_error)?;
        match self.record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                root_session_id,
                target,
                wake_required,
            } => self.insert_message_accepted(message, root_session_id, *target, *wake_required),
            AgentControlRecordBody::MessagePromoted { message_id } => {
                self.insert_message_promoted(message_id)
            }
            AgentControlRecordBody::MessageClaimed {
                message_id,
                activation_id,
                turn_id,
                step_id,
                entered_fact_seq,
            } => self.insert_message_claimed(
                message_id,
                activation_id,
                turn_id,
                step_id,
                *entered_fact_seq,
            ),
            AgentControlRecordBody::MessageDiscarded { message_id, reason } => {
                self.insert_message_discarded(message_id, *reason)
            }
            AgentControlRecordBody::ActivationStarted {
                activation_id,
                parent_session_id,
                root_session_id,
                path,
            } => {
                let (header, _) = read_session_header_row(self.transaction, self.session_id)?;
                rsi_agent_store_protocol::validate_activation_lineage(
                    &header,
                    root_session_id,
                    parent_session_id.as_ref(),
                    path,
                )?;
                self.insert_activation_started(activation_id, parent_session_id.as_ref())
            }
            AgentControlRecordBody::ActivationWaitingForDescendants { activation_id } => {
                self.insert_activation_waiting(activation_id)
            }
            AgentControlRecordBody::CompletionReserved {
                activation_id,
                parent_session_id,
                maximum_bytes,
            } => self.insert_completion_reserved(activation_id, parent_session_id, *maximum_bytes),
            AgentControlRecordBody::ActivationSettled { activation_id, .. } => {
                self.insert_activation_settled(activation_id)
            }
            AgentControlRecordBody::WaitParked {
                activation_id,
                turn_id,
                ..
            } => self.insert_wait_parked(activation_id, turn_id),
            AgentControlRecordBody::WaitResumed {
                activation_id,
                turn_id,
                ..
            } => self.insert_wait_resumed(activation_id, turn_id),
        }
    }

    fn insert_message_accepted(
        &self,
        message: &AgentMessage,
        root_session_id: &SessionId,
        target: MessageTarget,
        wake_required: bool,
    ) -> Result<()> {
        let expected_root = derived_session_root(self.transaction, self.session_id)?;
        if root_session_id != &expected_root {
            return Err(StoreError::Invalid(
                "Agent message root differs from its target Session root".into(),
            ));
        }
        let duplicate = self
            .transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM agent_messages
                   WHERE session_id = ?1 AND message_id = ?2
                 )",
                params![self.session_id.as_str(), message.message_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if duplicate {
            return Err(StoreError::Corrupt(
                "mailbox index repeats a message identity".into(),
            ));
        }
        let pending = self
            .transaction
            .query_row(
                "SELECT COUNT(*) FROM agent_messages
                 WHERE session_id = ?1 AND state = 'pending'",
                [self.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)?;
        let reservations = self
            .transaction
            .query_row(
                "SELECT COUNT(*) FROM active_activations
                 WHERE parent_session_id = ?1 AND completion_reserved_bytes IS NOT NULL",
                [self.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)?;
        if pending.saturating_add(reservations)
            >= i64::try_from(rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES)
                .expect("pending-message bound fits SQLite INTEGER")
        {
            return Err(StoreError::Invalid(
                "mailbox exceeds its pending-message bound".into(),
            ));
        }
        self.transaction
            .execute(
                "INSERT INTO agent_messages
                    (session_id, message_id, accepted_control_seq, root_session_id,
                     message_source, message_json, target, wake_required, state, state_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
                params![
                    self.session_id.as_str(),
                    message.message_id.as_str(),
                    sqlite_u64("accepted control sequence", self.record.seq())?,
                    root_session_id.as_str(),
                    message_source_name(&message.source),
                    encode_json("Agent message", message)?,
                    message_target_name(target),
                    wake_required,
                    encode_json("Agent message state", &StoreAgentMessageState::Pending)?,
                ],
            )
            .map_err(sql_error)?;
        if !wake_required {
            return Ok(());
        }
        self.transaction
            .execute(
                "INSERT INTO ready_messages
                    (root_session_id, session_id, message_id, ready_control_seq,
                     timestamp_ms, target)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    root_session_id.as_str(),
                    self.session_id.as_str(),
                    message.message_id.as_str(),
                    sqlite_u64("accepted control sequence", self.record.seq())?,
                    sqlite_u64("message timestamp", self.record.timestamp_ms())?,
                    message_target_name(target),
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    fn insert_message_promoted(&self, message_id: &MessageId) -> Result<()> {
        let indexed = self
            .transaction
            .query_row(
                "SELECT root_session_id, target, wake_required, state,
                        length(CAST(message_json AS BLOB)),
                        CASE WHEN length(CAST(message_json AS BLOB)) <= ?3
                             THEN message_json END
                 FROM agent_messages WHERE session_id = ?1 AND message_id = ?2",
                params![
                    self.session_id.as_str(),
                    message_id.as_str(),
                    i64::try_from(MAXIMUM_STORE_MAILBOX_PAGE_BYTES)
                        .expect("mailbox page bound fits SQLite INTEGER"),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                StoreError::Corrupt("mailbox promotion has no indexed message".into())
            })?;
        let message = decode_projected_json::<AgentMessage>(
            "Agent message",
            (indexed.4, indexed.5),
            MAXIMUM_STORE_MAILBOX_PAGE_BYTES,
        )?;
        if indexed.1 != "next_step"
            || indexed.2
            || indexed.3 != "pending"
            || !matches!(message.source, AgentMessageSource::Completion { .. })
        {
            return Err(StoreError::Corrupt(
                "mailbox promotion requires pending non-waking next-Step completion".into(),
            ));
        }
        let changed = self
            .transaction
            .execute(
                "UPDATE agent_messages SET target = 'next_turn', wake_required = 1
                 WHERE session_id = ?1 AND message_id = ?2
                   AND state = 'pending' AND target = 'next_step' AND wake_required = 0",
                params![self.session_id.as_str(), message_id.as_str()],
            )
            .map_err(sql_error)?;
        ensure_changed(
            changed,
            "message promotion disagrees with the mailbox index",
        )?;
        self.transaction
            .execute(
                "INSERT INTO ready_messages
                    (root_session_id, session_id, message_id, ready_control_seq,
                     timestamp_ms, target)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'next_turn')",
                params![
                    indexed.0,
                    self.session_id.as_str(),
                    message_id.as_str(),
                    sqlite_u64("promotion control sequence", self.record.seq())?,
                    sqlite_u64("promotion timestamp", self.record.timestamp_ms())?,
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    fn insert_message_claimed(
        &self,
        message_id: &MessageId,
        activation_id: &ActivationId,
        turn_id: &TurnId,
        step_id: &StepId,
        entered_fact_seq: u64,
    ) -> Result<()> {
        let message_projection = self
            .transaction
            .query_row(
                "SELECT length(CAST(message_json AS BLOB)),
                        CASE WHEN length(CAST(message_json AS BLOB)) <= ?3
                             THEN message_json END
                 FROM agent_messages
                 WHERE session_id = ?1 AND message_id = ?2 AND state = 'pending'",
                params![
                    self.session_id.as_str(),
                    message_id.as_str(),
                    i64::try_from(MAXIMUM_STORE_MAILBOX_PAGE_BYTES)
                        .expect("mailbox page bound fits SQLite INTEGER"),
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                StoreError::Corrupt("mailbox claim has no pending indexed message".into())
            })?;
        let message = decode_projected_json::<AgentMessage>(
            "Agent message",
            message_projection,
            MAXIMUM_STORE_MAILBOX_PAGE_BYTES,
        )?;
        let fact_projection = self
            .transaction
            .query_row(
                "SELECT length(CAST(fact_json AS BLOB)),
                        CASE WHEN length(CAST(fact_json AS BLOB)) <= ?3 THEN fact_json END
                 FROM facts WHERE session_id = ?1 AND seq = ?2",
                params![
                    self.session_id.as_str(),
                    sqlite_u64("entered Fact sequence", entered_fact_seq)?,
                    i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                        .expect("session Fact bound fits SQLite INTEGER"),
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let fact = fact_projection
            .map(|projection| {
                decode_projected_json::<SessionFact>(
                    "session Fact",
                    projection,
                    MAXIMUM_SESSION_FACT_BYTES,
                )
            })
            .transpose()?;
        validate_message_claim_fact(
            &message,
            turn_id,
            step_id,
            self.minimum_entered_fact_seq,
            fact.as_ref(),
        )?;
        self.transaction
            .execute(
                "DELETE FROM ready_messages WHERE session_id = ?1 AND message_id = ?2",
                params![self.session_id.as_str(), message_id.as_str()],
            )
            .map_err(sql_error)?;
        let state = StoreAgentMessageState::Claimed {
            activation_id: activation_id.clone(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
            entered_fact_seq,
        };
        let changed = self
            .transaction
            .execute(
                "UPDATE agent_messages SET state = 'claimed', state_json = ?1
                 WHERE session_id = ?2 AND message_id = ?3 AND state = 'pending'",
                params![
                    encode_json("Agent message state", &state)?,
                    self.session_id.as_str(),
                    message_id.as_str(),
                ],
            )
            .map_err(sql_error)?;
        ensure_changed(changed, "message claim disagrees with the mailbox index")?;
        let changed = self
            .transaction
            .execute(
                "UPDATE active_activations SET turn_id = ?1
                 WHERE session_id = ?2 AND activation_id = ?3
                   AND (turn_id IS NULL OR turn_id = ?1)",
                params![
                    turn_id.as_str(),
                    self.session_id.as_str(),
                    activation_id.as_str()
                ],
            )
            .map_err(sql_error)?;
        ensure_changed(
            changed,
            "message claim disagrees with its active activation",
        )
    }

    fn insert_message_discarded(
        &self,
        message_id: &MessageId,
        reason: MessageDiscardReason,
    ) -> Result<()> {
        self.transaction
            .execute(
                "DELETE FROM ready_messages WHERE session_id = ?1 AND message_id = ?2",
                params![self.session_id.as_str(), message_id.as_str()],
            )
            .map_err(sql_error)?;
        let state = StoreAgentMessageState::Discarded {
            reason,
            control_seq: self.record.seq(),
        };
        let changed = self
            .transaction
            .execute(
                "UPDATE agent_messages SET state = 'discarded', state_json = ?1
                 WHERE session_id = ?2 AND message_id = ?3 AND state = 'pending'",
                params![
                    encode_json("Agent message state", &state)?,
                    self.session_id.as_str(),
                    message_id.as_str(),
                ],
            )
            .map_err(sql_error)?;
        ensure_changed(changed, "message discard disagrees with the mailbox index")
    }

    fn insert_activation_started(
        &self,
        activation_id: &ActivationId,
        parent_session_id: Option<&SessionId>,
    ) -> Result<()> {
        let active = self
            .transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM active_activations WHERE session_id = ?1
                 )",
                [self.session_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if active {
            return Err(StoreError::Corrupt(
                "activation start follows an unsettled activation".into(),
            ));
        }
        self.transaction
            .execute(
                "INSERT INTO active_activations
                    (session_id, activation_id, parent_session_id, turn_id, phase,
                     completion_reserved_bytes)
                 VALUES (?1, ?2, ?3, NULL, 'running', NULL)",
                params![
                    self.session_id.as_str(),
                    activation_id.as_str(),
                    parent_session_id.map(SessionId::as_str),
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    fn insert_activation_waiting(&self, activation_id: &ActivationId) -> Result<()> {
        let changed = self
            .transaction
            .execute(
                "UPDATE active_activations SET phase = 'waiting'
                 WHERE session_id = ?1 AND activation_id = ?2 AND phase = 'running'",
                params![self.session_id.as_str(), activation_id.as_str()],
            )
            .map_err(sql_error)?;
        ensure_changed(
            changed,
            "activation wait does not match a running activation",
        )
    }

    fn insert_completion_reserved(
        &self,
        activation_id: &ActivationId,
        parent_session_id: &SessionId,
        maximum_bytes: u64,
    ) -> Result<()> {
        let occupied = self
            .transaction
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM agent_messages
                     WHERE session_id = ?1 AND state = 'pending') +
                    (SELECT COUNT(*) FROM active_activations
                     WHERE parent_session_id = ?1
                       AND completion_reserved_bytes IS NOT NULL)",
                [parent_session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)?;
        if occupied
            >= i64::try_from(rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES)
                .expect("pending-message bound fits SQLite INTEGER")
        {
            return Err(StoreError::Invalid(
                "parent mailbox has no completion-reservation capacity".into(),
            ));
        }
        let changed = self
            .transaction
            .execute(
                "UPDATE active_activations SET completion_reserved_bytes = ?1
                 WHERE session_id = ?2 AND activation_id = ?3
                   AND parent_session_id = ?4
                   AND completion_reserved_bytes IS NULL",
                params![
                    sqlite_u64("completion reservation bytes", maximum_bytes)?,
                    self.session_id.as_str(),
                    activation_id.as_str(),
                    parent_session_id.as_str(),
                ],
            )
            .map_err(sql_error)?;
        ensure_changed(
            changed,
            "completion reservation disagrees with its active child",
        )
    }

    fn insert_activation_settled(&self, activation_id: &ActivationId) -> Result<()> {
        let changed = self
            .transaction
            .execute(
                "DELETE FROM active_activations
                 WHERE session_id = ?1 AND activation_id = ?2
                   AND (parent_session_id IS NULL
                        OR completion_reserved_bytes IS NOT NULL)",
                params![self.session_id.as_str(), activation_id.as_str()],
            )
            .map_err(sql_error)?;
        ensure_changed(
            changed,
            "activation settlement disagrees with its active reservation",
        )
    }

    fn insert_wait_parked(&self, activation_id: &ActivationId, turn_id: &TurnId) -> Result<()> {
        let changed = self
            .transaction
            .execute(
                "UPDATE active_activations SET phase = 'parked'
                 WHERE session_id = ?1 AND activation_id = ?2
                   AND turn_id = ?3 AND phase = 'running'",
                params![
                    self.session_id.as_str(),
                    activation_id.as_str(),
                    turn_id.as_str()
                ],
            )
            .map_err(sql_error)?;
        ensure_changed(changed, "parked wait disagrees with its running activation")
    }

    fn insert_wait_resumed(&self, activation_id: &ActivationId, turn_id: &TurnId) -> Result<()> {
        let changed = self
            .transaction
            .execute(
                "UPDATE active_activations SET phase = 'running'
                 WHERE session_id = ?1 AND activation_id = ?2
                   AND turn_id = ?3 AND phase = 'parked'",
                params![
                    self.session_id.as_str(),
                    activation_id.as_str(),
                    turn_id.as_str()
                ],
            )
            .map_err(sql_error)?;
        ensure_changed(changed, "resumed wait disagrees with its parked activation")
    }
}

pub(super) fn ensure_changed(changed: usize, mismatch: &str) -> Result<()> {
    if changed != 1 {
        return Err(StoreError::Corrupt(mismatch.into()));
    }
    Ok(())
}

pub(super) const fn message_target_name(target: MessageTarget) -> &'static str {
    match target {
        MessageTarget::NextTurn => "next_turn",
        MessageTarget::NextStep => "next_step",
    }
}

pub(super) const fn message_source_name(source: &AgentMessageSource) -> &'static str {
    match source {
        AgentMessageSource::Human => "human",
        AgentMessageSource::Agent { .. } => "agent",
        AgentMessageSource::Completion { .. } => "completion",
    }
}

pub(super) type IndexedMessageRow = (
    i64,
    Option<String>,
    String,
    String,
    String,
    bool,
    i64,
    String,
    i64,
    Option<String>,
);

pub(super) fn indexed_message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedMessageRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

pub(super) fn decode_indexed_message(row: IndexedMessageRow) -> Result<StoreAgentMessage> {
    let encoded_message_bytes = usize::try_from(row.0)
        .map_err(|_| StoreError::Corrupt("Agent message has a negative byte length".into()))?;
    let message = decode_projected_json::<AgentMessage>(
        "Agent message",
        (row.0, row.1),
        MAXIMUM_STORE_MAILBOX_PAGE_BYTES,
    )?;
    if row.2 != message_source_name(&message.source) {
        return Err(StoreError::Corrupt(
            "mailbox source discriminator differs from its typed message".into(),
        ));
    }
    let target = match row.4.as_str() {
        "next_turn" => MessageTarget::NextTurn,
        "next_step" => MessageTarget::NextStep,
        _ => return Err(StoreError::Corrupt("mailbox target is invalid".into())),
    };
    let state = decode_projected_json::<StoreAgentMessageState>(
        "Agent message state",
        (row.8, row.9),
        MAXIMUM_INDEXED_MESSAGE_STATE_BYTES,
    )?;
    let expected_state = match &state {
        StoreAgentMessageState::Pending => "pending",
        StoreAgentMessageState::Claimed { .. } => "claimed",
        StoreAgentMessageState::Discarded { .. } => "discarded",
    };
    if row.7 != expected_state {
        return Err(StoreError::Corrupt(
            "mailbox state discriminator differs from its typed state".into(),
        ));
    }
    Ok(StoreAgentMessage {
        message,
        encoded_message_bytes,
        root_session_id: SessionId::new(row.3)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        target,
        wake_required: row.5,
        accepted_control_seq: decode_u64("message acceptance control sequence", row.6)?,
        state,
    })
}

pub(super) fn decode_ready_message(
    row: (String, String, i64, i64, String),
) -> Result<StoreReadyMessage> {
    let target = match row.4.as_str() {
        "next_turn" => MessageTarget::NextTurn,
        "next_step" => MessageTarget::NextStep,
        _ => {
            return Err(StoreError::Corrupt(
                "ready message target is invalid".into(),
            ));
        }
    };
    Ok(StoreReadyMessage {
        session_id: SessionId::new(row.0)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        message_id: rsi_agent_session_protocol::MessageId::new(row.1)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        control_seq: decode_u64("ready control sequence", row.2)?,
        timestamp_ms: decode_u64("ready timestamp", row.3)?,
        target,
    })
}

pub(super) fn admit_append(transaction: &Transaction<'_>, batch: &AppendBatch) -> Result<()> {
    let existing = transaction
        .query_row(
            "SELECT durable_seq FROM sessions WHERE session_id = ?1",
            [batch.session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    let actual = existing
        .map(|value| decode_u64("durable_seq", value))
        .transpose()?
        .unwrap_or(0);
    if actual != batch.expected_seq {
        return Err(StoreError::Conflict {
            expected: batch.expected_seq,
            actual,
        });
    }
    match (existing, batch.header.as_ref()) {
        (None, Some(header)) => transaction
            .execute(
                "INSERT INTO sessions
                    (session_id, created_at_ms, header_json, durable_seq, fact_prefix_sha256,
                     control_seq, control_prefix_sha256)
                 VALUES (?1, ?2, ?3, 0, ?4, 0, ?5)",
                params![
                    batch.session_id.as_str(),
                    sqlite_u64("session creation timestamp", header.created_at_ms())?,
                    encode_json("session header", header)?,
                    hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                    hex::encode(EMPTY_CONTROL_PREFIX_DIGEST),
                ],
            )
            .map(|_| ())
            .map_err(sql_error),
        (None, None) => Err(StoreError::NotFound(batch.session_id.as_str().into())),
        (Some(_), Some(_)) => Err(StoreError::Invalid(
            "existing session cannot replace its immutable header".into(),
        )),
        (Some(_), None) => Ok(()),
    }?;
    if existing.is_none()
        && let Some(header) = &batch.header
    {
        insert_agent_node(transaction, header)?;
    }
    Ok(())
}

pub(super) fn insert_agent_node(
    transaction: &Transaction<'_>,
    header: &SessionHeader,
) -> Result<()> {
    let Some(origin) = header.fork_origin() else {
        return Ok(());
    };
    let expected_root = derived_session_root(transaction, &origin.parent_session_id)?;
    if origin.root_session_id != expected_root {
        return Err(StoreError::Invalid(
            "Agent child root differs from its parent's durable root".into(),
        ));
    }
    let child_count = transaction
        .query_row(
            "SELECT COUNT(*) FROM agent_nodes WHERE root_session_id = ?1",
            [origin.root_session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql_error)?;
    if child_count.saturating_add(1)
        >= i64::try_from(rsi_agent_session_protocol::MAXIMUM_DURABLE_AGENT_TREE_NODES)
            .expect("Agent tree bound fits SQLite INTEGER")
    {
        return Err(StoreError::Invalid(
            "Agent tree exceeds its durable node bound".into(),
        ));
    }
    let path_json = encode_json("Agent path", &origin.path)?;
    let path_exists = transaction
        .query_row(
            "SELECT 1 FROM agent_nodes WHERE root_session_id = ?1 AND path_json = ?2",
            params![origin.root_session_id.as_str(), &path_json],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql_error)?
        .is_some();
    if path_exists {
        return Err(StoreError::Invalid(
            "Agent tree path is already present".into(),
        ));
    }
    let task_exists = transaction
        .query_row(
            "SELECT 1 FROM agent_nodes WHERE parent_session_id = ?1 AND task_name = ?2",
            params![origin.parent_session_id.as_str(), &origin.task_name],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql_error)?
        .is_some();
    if task_exists {
        return Err(StoreError::Invalid(
            "Agent task name is already present below its parent".into(),
        ));
    }
    transaction
        .execute(
            "INSERT INTO agent_nodes
                (session_id, root_session_id, parent_session_id, path_json, task_name)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                header.session_id().as_str(),
                origin.root_session_id.as_str(),
                origin.parent_session_id.as_str(),
                path_json,
                &origin.task_name,
            ],
        )
        .map(|_| ())
        .map_err(sql_error)
}

pub(super) fn derived_session_root(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<SessionId> {
    let (header, _) = read_session_header_row(connection, session_id)?;
    Ok(header.fork_origin().map_or_else(
        || session_id.clone(),
        |origin| origin.root_session_id.clone(),
    ))
}

pub(super) fn insert_fact(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    fact: &SessionFact,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO facts (session_id, seq, turn_id, fact_kind, fact_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id.as_str(),
                sqlite_u64("fact sequence", fact.seq())?,
                fact.body().turn_id().as_str(),
                fact_index_kind(fact.body()),
                encode_json("session Fact", fact)?,
            ],
        )
        .map_err(sql_error)?;
    update_turn_index(transaction, session_id, fact)?;
    update_workspace_context_index(transaction, session_id, fact)
}

pub(super) fn update_workspace_context_index(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    fact: &SessionFact,
) -> Result<()> {
    let SessionFactBody::InputMessageEntered { source, .. } = fact.body() else {
        return Ok(());
    };
    let (column, digest) = match source {
        InputMessageSource::AgentInstructions { sha256, .. } => {
            ("workspace_instructions_sha256", sha256)
        }
        InputMessageSource::SkillCatalog { sha256 } => ("workspace_skill_catalog_sha256", sha256),
        InputMessageSource::Human { .. }
        | InputMessageSource::Agent { .. }
        | InputMessageSource::Completion { .. }
        | InputMessageSource::UserSkillInvocation { .. } => return Ok(()),
    };
    let changed = transaction
        .execute(
            &format!("UPDATE sessions SET {column} = ?1 WHERE session_id = ?2"),
            params![digest, session_id.as_str()],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(StoreError::Corrupt(
            "workspace-context index lost its owning Session".into(),
        ));
    }
    Ok(())
}

pub(super) fn update_turn_index(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    fact: &SessionFact,
) -> Result<()> {
    let role = rsi_agent_store_protocol::store_fact_turn_role(fact.body());
    let changed = match role {
        StoreFactTurnRole::Acceptance => transaction
            .execute(
                "INSERT OR IGNORE INTO turns
                 (session_id, turn_id, accepted_seq, terminal_seq, terminal_prefix_sha256)
                 VALUES (?1, ?2, ?3, NULL, NULL)",
                params![
                    session_id.as_str(),
                    fact.body().turn_id().as_str(),
                    sqlite_u64("turn acceptance sequence", fact.seq())?,
                ],
            )
            .map_err(sql_error)?,
        StoreFactTurnRole::Terminal => transaction
            .execute(
                "UPDATE turns SET terminal_seq = ?1
                 WHERE session_id = ?2 AND turn_id = ?3 AND terminal_seq IS NULL",
                params![
                    sqlite_u64("turn terminal sequence", fact.seq())?,
                    session_id.as_str(),
                    fact.body().turn_id().as_str(),
                ],
            )
            .map_err(sql_error)?,
        StoreFactTurnRole::Event => transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM turns
                   WHERE session_id = ?1 AND turn_id = ?2 AND terminal_seq IS NULL
                 )",
                params![session_id.as_str(), fact.body().turn_id().as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map(usize::from)
            .map_err(sql_error)?,
    };
    if changed == 1 {
        return Ok(());
    }
    Err(StoreError::Corrupt(role.rejected_message().into()))
}

pub(super) fn advance_watermark(transaction: &Transaction<'_>, batch: &AppendBatch) -> Result<u64> {
    let durable_seq = batch
        .facts
        .last()
        .expect("validated append is nonempty")
        .seq();
    let previous = transaction
        .query_row(
            "SELECT fact_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [batch.session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)?;
    let mut fact_prefix_digest = decode_sha256("Fact-prefix digest", &previous)?;
    for fact in &batch.facts {
        fact_prefix_digest = advance_fact_prefix_digest(fact_prefix_digest, fact)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        if matches!(fact.body(), SessionFactBody::TurnTerminal { .. }) {
            let changed = transaction
                .execute(
                    "UPDATE turns SET terminal_prefix_sha256 = ?1
                     WHERE session_id = ?2 AND turn_id = ?3 AND terminal_seq = ?4",
                    params![
                        hex::encode(fact_prefix_digest),
                        batch.session_id.as_str(),
                        fact.body().turn_id().as_str(),
                        sqlite_u64("turn terminal sequence", fact.seq())?,
                    ],
                )
                .map_err(sql_error)?;
            if changed != 1 {
                return Err(StoreError::Corrupt(
                    "SQLite lost a terminal-prefix update predicate".into(),
                ));
            }
        }
    }
    let changed = transaction
        .execute(
            "UPDATE sessions SET durable_seq = ?1, fact_prefix_sha256 = ?2
             WHERE session_id = ?3 AND durable_seq = ?4",
            params![
                sqlite_u64("durable sequence", durable_seq)?,
                hex::encode(fact_prefix_digest),
                batch.session_id.as_str(),
                sqlite_u64("expected sequence", batch.expected_seq)?,
            ],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(StoreError::Corrupt(
            "SQLite lost a transaction-local append predicate".into(),
        ));
    }
    Ok(durable_seq)
}
