//! Bounded canonical counterparts for shared Program graph validation.
use super::{
    AgentControlRecord, Connection, MAXIMUM_SESSION_FACT_BYTES, MessageId, Result, SessionHeader,
    SessionId, StoreAgentMessage, StoreError, decode_projected_json, params,
    read_session_header_row, sql_error, sqlite_u64, validation,
};
use rsi_agent_store_protocol::{ProgramGraphQuery, ProgramGraphRead};
pub(super) const ACTIVATION_COUNTERPART_SQL: &str = "
    SELECT length(CAST(control_json AS BLOB)),
           CASE WHEN length(CAST(control_json AS BLOB))<=?7 THEN control_json END
    FROM agent_controls
    WHERE session_id=?1 AND ?2 IS NULL AND ?4 IS NULL AND ?5 IS NULL
      AND json_extract(control_json,'$.type')=?3
      AND json_extract(control_json,'$.activation_id')=?6
    LIMIT 2";
pub(super) struct Graph<'a>(pub &'a Connection);
impl ProgramGraphRead for Graph<'_> {
    fn header(&self, session: &SessionId) -> Result<SessionHeader> {
        read_session_header_row(self.0, session).map(|(header, _)| header)
    }
    fn message(&self, session: &SessionId, message: &MessageId) -> Result<StoreAgentMessage> {
        validation::read_indexed_agent_message(self.0, session, message)
    }
    fn control(
        &self,
        session: &SessionId,
        query: ProgramGraphQuery<'_>,
    ) -> Result<AgentControlRecord> {
        let (kind, run, event, ordinal, activation) = match query {
            ProgramGraphQuery::Admission(run, ordinal) => (
                "program_run",
                Some(run.as_str()),
                Some("child_admitted"),
                Some(ordinal),
                None,
            ),
            ProgramGraphQuery::Start(run, ordinal) => (
                "program_run",
                Some(run.as_str()),
                Some("child_started"),
                Some(ordinal),
                None,
            ),
            ProgramGraphQuery::Terminal(run) => (
                "program_run",
                Some(run.as_str()),
                Some("terminal"),
                None,
                None,
            ),
            ProgramGraphQuery::Sink(id) => (
                "program_completion_reserved",
                None,
                None,
                None,
                Some(id.as_str()),
            ),
            ProgramGraphQuery::Settlement(id) => {
                ("activation_settled", None, None, None, Some(id.as_str()))
            }
        };
        let sql = if run.is_some() {
            "SELECT length(CAST(c.control_json AS BLOB)),
                    CASE WHEN length(CAST(c.control_json AS BLOB))<=?7 THEN c.control_json END
             FROM program_records p JOIN agent_controls c
               ON c.session_id=p.session_id AND c.seq=p.control_seq
             WHERE p.session_id=?1 AND p.run_id=?2
               AND json_extract(c.control_json,'$.type')=?3
               AND (?4 IS NULL OR json_extract(c.control_json,'$.event.event')=?4)
               AND (?5 IS NULL OR json_extract(c.control_json,'$.event.ordinal')=?5)
               AND ?6 IS NULL
             LIMIT 2"
        } else {
            ACTIVATION_COUNTERPART_SQL
        };
        let mut statement = self.0.prepare(sql).map_err(sql_error)?;
        let mut rows = statement
            .query(params![
                session.as_str(),
                run,
                kind,
                event,
                ordinal,
                activation,
                sqlite_u64("control bound", MAXIMUM_SESSION_FACT_BYTES as u64)?
            ])
            .map_err(sql_error)?;
        let row = rows.next().map_err(sql_error)?.ok_or_else(missing)?;
        let record: AgentControlRecord = decode_projected_json(
            "Program counterpart",
            (
                row.get(0).map_err(sql_error)?,
                row.get(1).map_err(sql_error)?,
            ),
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        if rows.next().map_err(sql_error)?.is_some() || !query.matches(record.body()) {
            return Err(missing());
        }
        Ok(record)
    }
}
fn missing() -> StoreError {
    StoreError::Invalid("Program graph counterpart is absent, duplicated or inconsistent".into())
}
