//! Indexed reads of canonical message feedback. A removal is the latest change with no value.

use super::*;
use rsi_agent_session_protocol::{MessageFeedbackChange, SavedMessageFeedback};
use std::num::NonZeroU64;

pub(super) fn read(
    db: &Connection,
    session: &SessionId,
    message: NonZeroU64,
    through: u64,
) -> Result<Option<SavedMessageFeedback>> {
    // SQLite INTEGER cannot address this coordinate, so it has no stored head.
    let Ok(message_seq) = i64::try_from(message.get()) else {
        return Ok(None);
    };
    let encoded = db.query_row(
        "SELECT length(CAST(control_json AS BLOB)),
                CASE WHEN length(CAST(control_json AS BLOB))<=?4 THEN control_json END, seq
         FROM agent_controls WHERE session_id=?1 AND json_extract(control_json,'$.change.message_seq')=?2
           AND json_extract(control_json,'$.type')='message_feedback_changed' AND seq<=?3
         ORDER BY seq DESC LIMIT 1",
        params![session.as_str(), message_seq, sqlite_u64("feedback cursor", through)?,
                i64::try_from(MAXIMUM_SESSION_FACT_BYTES).expect("bounded control length")],
        |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)?)),
    ).optional().map_err(sql_error)?;
    let Some((length, json, seq)) = encoded else {
        return Ok(None);
    };
    let record: AgentControlRecord = decode_projected_json(
        "message feedback",
        (length, json),
        MAXIMUM_SESSION_FACT_BYTES,
    )?;
    let version = NonZeroU64::new(record.seq()).expect("validated control");
    match record.into_body() {
        AgentControlRecordBody::MessageFeedbackChanged { change }
            if change.message_seq == message
                && decode_u64("feedback sequence", seq)? == version.get() =>
        {
            Ok(change
                .value
                .map(|value| SavedMessageFeedback { version, value }))
        }
        _ => Err(StoreError::Corrupt(
            "feedback index disagrees with its canonical record".into(),
        )),
    }
}

pub(super) fn validate_change(
    db: &Connection,
    session: &SessionId,
    change: &MessageFeedbackChange,
    previous_seq: u64,
    fact_cut: u64,
) -> Result<()> {
    let current = read(db, session, change.message_seq, previous_seq)?;
    rsi_agent_store_protocol::validate_feedback_append(change, current.as_ref(), || {
        Ok(change.message_seq.get() <= fact_cut
            && read_indexed_fact(
                db,
                session,
                sqlite_u64("feedback target", change.message_seq.get())?,
            )?
            .body()
            .is_feedback_target())
    })
}
