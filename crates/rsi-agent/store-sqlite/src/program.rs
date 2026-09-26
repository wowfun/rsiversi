//! Mechanical run membership and budget indexes; Kernel interprets lifecycle semantics.
use super::{
    AgentControlRecord, AgentControlRecordBody, Connection, MAXIMUM_SESSION_FACT_BYTES,
    OptionalExtension, Result, SessionId, SqliteStore, StoreError, TransactionBehavior,
    bounded_text, decode_projected_json, decode_u64, encode_json, params, sql_error, sqlite_u64,
    validate_session_read_limit,
};
use rsi_agent_session_protocol::{MAXIMUM_PROGRAM_RECORD_BYTES, ProgramRunEvent, ProgramRunId};
use rsi_agent_store_protocol::{
    MAXIMUM_PROGRAM_RECORDS, StoreProgramCursor, StoreProgramHead, StoreProgramPage,
    StoreProgramRecords, program_head_after,
};
const MAXIMUM_HEAD_BYTES: usize = 4096;

const READ_HEAD_SQL: &str = "SELECT length(CAST(head_json AS BLOB)),
    CASE WHEN length(CAST(head_json AS BLOB)) <= ?3 THEN head_json END, terminal, creator_turn_id
    FROM program_runs
    WHERE session_id=?1
    AND run_id=?2";
const OCCUPIED_MAILBOX_SQL: &str = "SELECT (SELECT COUNT(*)
    FROM agent_messages
    WHERE session_id=?1
    AND state='pending')
    + (SELECT COUNT(*)
    FROM active_activations
    WHERE parent_session_id=?1
    AND completion_reserved_bytes IS NOT NULL)";
const HAS_ACTIVE_RUN_SQL: &str = "SELECT EXISTS(SELECT 1
    FROM program_runs
    WHERE session_id=?1
    AND terminal=0)";
const HAS_CREATOR_RUN_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM program_runs WHERE session_id=?1 AND creator_turn_id=?2)";
const INSERT_RUN_SQL: &str =
    "INSERT INTO program_runs(session_id,run_id,creator_turn_id,terminal,head_json)
    VALUES(?1,?2,?3,?4,?5)";
const UPDATE_RUN_SQL: &str = "UPDATE program_runs
    SET terminal=?3,head_json=?4
    WHERE session_id=?1
    AND run_id=?2";
const INSERT_RECORD_SQL: &str =
    "INSERT INTO program_records(session_id,run_id,control_seq,encoded_bytes)
    VALUES(?1,?2,?3,?4)";
const READ_RECORDS_SQL: &str =
    "SELECT p.control_seq,p.encoded_bytes,length(CAST(c.control_json AS BLOB)),
    CASE WHEN length(CAST(c.control_json AS BLOB))<=?3 THEN c.control_json END
    FROM program_records p
    JOIN agent_controls c ON c.session_id=p.session_id
    AND c.seq=p.control_seq
    WHERE p.session_id=?1
    AND p.run_id=?2
    AND p.control_seq>?5
    ORDER BY p.control_seq
    LIMIT ?4";
const ACTIVE_RUNS_AFTER_SQL: &str = "SELECT session_id,run_id
    FROM program_runs
    WHERE terminal=0
    AND (session_id,run_id)>(?1,?2)
    ORDER BY session_id,run_id
    LIMIT ?3";
const ACTIVE_RUNS_FIRST_SQL: &str = "SELECT session_id,run_id
    FROM program_runs
    WHERE terminal=0
    ORDER BY session_id,run_id
    LIMIT ?1";
const READ_RECORD_INDEX_SQL: &str = "SELECT run_id,encoded_bytes
    FROM program_records
    WHERE session_id=?1
    AND control_seq=?2";
const COUNT_RUN_RECORDS_SQL: &str = "SELECT (SELECT COUNT(*)
    FROM program_runs
    WHERE session_id=?1),(SELECT COUNT(*)
    FROM program_records
    WHERE session_id=?1)";

fn head(
    connection: &Connection,
    session: &SessionId,
    run: &ProgramRunId,
) -> Result<Option<StoreProgramHead>> {
    let raw = connection
        .query_row(
            READ_HEAD_SQL,
            params![
                session.as_str(),
                run.as_str(),
                sqlite_u64("program index bound", MAXIMUM_HEAD_BYTES as u64)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                    bounded_text(
                        row,
                        3,
                        rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES,
                    )?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    raw.map(|(length, json, terminal, creator)| {
        let head: StoreProgramHead =
            decode_projected_json("program index", (length, json), MAXIMUM_HEAD_BYTES)?;
        if &head.run_id != run
            || head.terminal != terminal
            || head.creator_turn_id.as_str() != creator
            || head.record_count == 0
            || head.record_count > MAXIMUM_PROGRAM_RECORDS
            || head.encoded_bytes > MAXIMUM_PROGRAM_RECORD_BYTES
            || head.first_control_seq == 0
            || head.last_control_seq < head.first_control_seq
        {
            return Err(StoreError::Corrupt(
                "program index identity or budget mismatch".into(),
            ));
        }
        Ok(head)
    })
    .transpose()
}
pub(super) fn insert(
    connection: &Connection,
    session: &SessionId,
    record: &AgentControlRecord,
) -> Result<()> {
    let AgentControlRecordBody::ProgramRun { run_id, event } = record.body() else {
        return Err(StoreError::Invalid(
            "program index requires its canonical event".into(),
        ));
    };
    let previous = head(connection, session, run_id)?;
    let next = program_head_after(session, previous.as_ref(), record)?;
    if matches!(event, ProgramRunEvent::Accepted { .. }) {
        let occupied = connection
            .query_row(OCCUPIED_MAILBOX_SQL, [session.as_str()], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sql_error)?;
        if occupied
            >= sqlite_u64(
                "program index bound",
                rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES as u64,
            )?
        {
            return Err(StoreError::Invalid(
                "program completion notice has no reserved mailbox slot".into(),
            ));
        }
        let active = connection
            .query_row(HAS_ACTIVE_RUN_SQL, [session.as_str()], |row| {
                row.get::<_, bool>(0)
            })
            .map_err(sql_error)?;
        let repeated_creator = connection
            .query_row(
                HAS_CREATOR_RUN_SQL,
                params![session.as_str(), next.creator_turn_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if active || repeated_creator {
            return Err(StoreError::Invalid(
                "program acceptance repeats a creator Turn or overlaps an unfinished run".into(),
            ));
        }
        connection
            .execute(
                INSERT_RUN_SQL,
                params![
                    session.as_str(),
                    run_id.as_str(),
                    next.creator_turn_id.as_str(),
                    next.terminal,
                    encode_json("program head", &next)?
                ],
            )
            .map_err(sql_error)?;
    } else {
        connection
            .execute(
                UPDATE_RUN_SQL,
                params![
                    session.as_str(),
                    run_id.as_str(),
                    next.terminal,
                    encode_json("program head", &next)?
                ],
            )
            .map_err(sql_error)?;
    }
    connection
        .execute(
            INSERT_RECORD_SQL,
            params![
                session.as_str(),
                run_id.as_str(),
                sqlite_u64("program control", record.seq())?,
                sqlite_u64("program index bound", record.encoded_len() as u64)?
            ],
        )
        .map_err(sql_error)?;
    Ok(())
}
impl SqliteStore {
    pub(super) async fn program_records(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
    ) -> Result<Option<StoreProgramRecords>> {
        self.program_records_after(session, run, 0).await
    }
    pub(super) async fn program_records_after(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
        after: u64,
    ) -> Result<Option<StoreProgramRecords>> {
        self.ensure_session_validated(session).await?;
        let session = session.clone();
        let run = run.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let Some(head) = head(&transaction, &session, &run)? else {
                return Ok(None);
            };
            let records = {
                let mut statement = transaction.prepare(READ_RECORDS_SQL).map_err(sql_error)?;
                let mut rows = statement
                    .query(params![
                        session.as_str(),
                        run.as_str(),
                        sqlite_u64("program index bound", MAXIMUM_SESSION_FACT_BYTES as u64)?,
                        i64::from(MAXIMUM_PROGRAM_RECORDS) + 1,
                        sqlite_u64("program cursor", after)?
                    ])
                    .map_err(sql_error)?;
                let mut records = Vec::new();
                let mut bytes = 0u64;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    let length =
                        decode_u64("program control length", row.get(2).map_err(sql_error)?)?;
                    bytes = bytes
                        .checked_add(length)
                        .ok_or_else(|| StoreError::Corrupt("program byte overflow".into()))?;
                    if bytes > MAXIMUM_PROGRAM_RECORD_BYTES
                        || records.len() >= MAXIMUM_PROGRAM_RECORDS as usize
                    {
                        return Err(StoreError::Corrupt("program records exceed bounds".into()));
                    }
                    let record: AgentControlRecord = decode_projected_json(
                        "program control",
                        (
                            row.get(2).map_err(sql_error)?,
                            row.get(3).map_err(sql_error)?,
                        ),
                        MAXIMUM_SESSION_FACT_BYTES,
                    )?;
                    if record.seq()
                        != decode_u64("program control sequence", row.get(0).map_err(sql_error)?)?
                        || length
                            != decode_u64("indexed program length", row.get(1).map_err(sql_error)?)?
                    {
                        return Err(StoreError::Corrupt(
                            "program control coordinates differ".into(),
                        ));
                    }
                    records.push(record);
                }
                records
            };
            let page = StoreProgramRecords { head, records };
            if after == 0 {
                page.validate(&session, &run)?;
            }
            transaction.commit().map_err(sql_error)?;
            Ok(Some(page))
        })
        .await
    }
    pub(super) async fn active_programs(
        &self,
        after: Option<&StoreProgramCursor>,
        limit: usize,
    ) -> Result<StoreProgramPage> {
        validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_reader(move |connection| {
            let sql = if after.is_some() {
                ACTIVE_RUNS_AFTER_SQL
            } else {
                ACTIVE_RUNS_FIRST_SQL
            };
            let mut statement = connection.prepare(sql).map_err(sql_error)?;
            let mut rows = if let Some(after) = after {
                statement.query(params![
                    after.session_id.as_str(),
                    after.run_id.as_str(),
                    sqlite_u64("program index bound", (limit + 1) as u64)?
                ])
            } else {
                statement.query([sqlite_u64("program index bound", (limit + 1) as u64)?])
            }
            .map_err(sql_error)?;
            let mut runs = Vec::new();
            while let Some(row) = rows.next().map_err(sql_error)? {
                let session = bounded_text(
                    row,
                    0,
                    rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES,
                )
                .map_err(sql_error)?;
                let run = bounded_text(
                    row,
                    1,
                    rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES,
                )
                .map_err(sql_error)?;
                runs.push(StoreProgramCursor {
                    session_id: SessionId::new(session)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    run_id: ProgramRunId::new(run)
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                });
            }
            let has_more = runs.len() > limit;
            runs.truncate(limit);
            Ok(StoreProgramPage { runs, has_more })
        })
        .await
    }
}
#[derive(Default)]
pub(super) struct Projection {
    active: Option<StoreProgramHead>,
    runs: u64,
    records: u64,
}
impl Projection {
    pub(super) fn apply(
        &mut self,
        connection: &Connection,
        session: &SessionId,
        record: &AgentControlRecord,
    ) -> Result<()> {
        let AgentControlRecordBody::ProgramRun { run_id, event } = record.body() else {
            return Ok(());
        };
        let next = program_head_after(session, self.active.as_ref(), record)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if matches!(event, ProgramRunEvent::Accepted { .. }) {
            self.runs += 1;
        }
        let indexed = connection
            .query_row(
                READ_RECORD_INDEX_SQL,
                params![
                    session.as_str(),
                    sqlite_u64("program sequence", record.seq())?
                ],
                |row| {
                    Ok((
                        bounded_text(
                            row,
                            0,
                            rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES,
                        )?,
                        row.get::<_, i64>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_error)?;
        if indexed
            != Some((
                run_id.to_string(),
                sqlite_u64("program index bound", record.encoded_len() as u64)?,
            ))
        {
            return Err(StoreError::Corrupt(
                "program membership differs from canonical controls".into(),
            ));
        }
        self.records += 1;
        if next.terminal {
            if head(connection, session, run_id)?.as_ref() != Some(&next) {
                return Err(StoreError::Corrupt("terminal program index differs".into()));
            }
            self.active = None;
        } else {
            self.active = Some(next);
        }
        Ok(())
    }
    pub(super) fn finish(self, connection: &Connection, session: &SessionId) -> Result<()> {
        if let Some(active) = self.active
            && head(connection, session, &active.run_id)?.as_ref() != Some(&active)
        {
            return Err(StoreError::Corrupt("active program index differs".into()));
        }
        let (runs, records) = connection
            .query_row(COUNT_RUN_RECORDS_SQL, [session.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sql_error)?;
        if decode_u64("program run count", runs)? != self.runs
            || decode_u64("program record count", records)? != self.records
        {
            return Err(StoreError::Corrupt(
                "program index cardinality differs".into(),
            ));
        }
        Ok(())
    }
}
