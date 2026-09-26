//! Bounded mechanical positions for canonical program records.
use crate::{Result, StoreError};
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, MAXIMUM_PROGRAM_RECORD_BYTES,
    PROGRAM_TERMINAL_RESERVE_BYTES, ProgramRunEvent, ProgramRunId, SessionId, TurnId,
};
use serde::{Deserialize, Serialize};
/// Record count ceiling including reserved closure events.
pub const MAXIMUM_PROGRAM_RECORDS: u32 = 4096;
/// One receipt per admitted child, cancellation request and final terminal record.
const PROGRAM_CLOSURE_RECORDS: u32 = rsi_agent_session_protocol::MAXIMUM_PROGRAM_CHILDREN + 2;
/// Exact mechanical run-index row; never live execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreProgramHead {
    /// Identity within the owning Session.
    pub run_id: ProgramRunId,
    /// Creator Turn, unique across all runs in the Session.
    pub creator_turn_id: TurnId,
    /// Acceptance control sequence.
    pub first_control_seq: u64,
    /// Most recent control sequence for this run.
    pub last_control_seq: u64,
    /// Cumulative canonical control bytes, including closure records.
    pub encoded_bytes: u64,
    /// Cumulative count of run controls.
    pub record_count: u32,
    /// Whether the final terminal control is present.
    pub terminal: bool,
}
/// Computes the next mechanical index row from one canonical event.
pub fn program_head_after(
    session: &SessionId,
    previous: Option<&StoreProgramHead>,
    record: &AgentControlRecord,
) -> Result<StoreProgramHead> {
    let AgentControlRecordBody::ProgramRun { run_id, event } = record.body() else {
        return Err(invalid("program index requires a program control"));
    };
    let mut head = match (previous, event) {
        (None, ProgramRunEvent::Accepted { descriptor }) if &descriptor.session_id == session => {
            StoreProgramHead {
                run_id: run_id.clone(),
                creator_turn_id: descriptor.creator_turn_id.clone(),
                first_control_seq: record.seq(),
                last_control_seq: 0,
                encoded_bytes: 0,
                record_count: 0,
                terminal: false,
            }
        }
        (Some(old), event)
            if !old.terminal
                && &old.run_id == run_id
                && !matches!(event, ProgramRunEvent::Accepted { .. })
                && old.last_control_seq < record.seq() =>
        {
            old.clone()
        }
        _ => {
            return Err(invalid(
                "program record membership or terminal boundary is inconsistent",
            ));
        }
    };
    head.last_control_seq = record.seq();
    head.record_count = head
        .record_count
        .checked_add(1)
        .ok_or_else(|| invalid("program record count overflow"))?;
    head.encoded_bytes = head
        .encoded_bytes
        .checked_add(record.encoded_len() as u64)
        .ok_or_else(|| invalid("program record bytes overflow"))?;
    let (bytes, count) = if event.is_closure() {
        (MAXIMUM_PROGRAM_RECORD_BYTES, MAXIMUM_PROGRAM_RECORDS)
    } else {
        (
            MAXIMUM_PROGRAM_RECORD_BYTES - PROGRAM_TERMINAL_RESERVE_BYTES,
            MAXIMUM_PROGRAM_RECORDS - PROGRAM_CLOSURE_RECORDS,
        )
    };
    if head.encoded_bytes > bytes || head.record_count > count {
        return Err(invalid("program records exceed their reserved budget"));
    }
    head.terminal = matches!(event, ProgramRunEvent::Terminal { .. });
    Ok(head)
}
/// Bounded canonical records selected by their run index in one Store snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreProgramRecords {
    /// Mechanical head at the same snapshot as the selected suffix.
    pub head: StoreProgramHead,
    /// Canonical run controls strictly after the requested cursor.
    pub records: Vec<AgentControlRecord>,
}
impl StoreProgramRecords {
    /// Recomputes the index and encoded-byte budget from canonical records.
    pub fn validate(&self, session: &SessionId, run: &ProgramRunId) -> Result<()> {
        self.validate_after(session, run, None)
    }
    /// Validates a complete indexed suffix against a previously validated head.
    pub fn validate_after(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
        previous: Option<&StoreProgramHead>,
    ) -> Result<()> {
        if self.records.len() > MAXIMUM_PROGRAM_RECORDS as usize || &self.head.run_id != run {
            return Err(invalid("program page exceeds its record identity bound"));
        }
        let mut head = previous.cloned();
        for record in &self.records {
            head = Some(program_head_after(session, head.as_ref(), record)?);
        }
        if head.as_ref() != Some(&self.head) {
            return Err(invalid("program index differs from canonical records"));
        }
        Ok(())
    }
}
/// Stable lexical key for startup enumeration of unfinished runs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StoreProgramCursor {
    /// Owning durable Session.
    pub session_id: SessionId,
    /// Identity within the owning Session.
    pub run_id: ProgramRunId,
}
/// One bounded page of unfinished runs; canonical records still require validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreProgramPage {
    /// Unfinished runs in lexical Session/run order.
    pub runs: Vec<StoreProgramCursor>,
    /// Whether another entry exists after this bounded page.
    pub has_more: bool,
}
fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.into())
}
/// Lexical pending-notice key, independent of ready versus next-Step routing.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StoreProgramNotice {
    /// Owning durable Session.
    pub session_id: SessionId,
    /// Exact pending Program completion message.
    pub message_id: rsi_agent_session_protocol::MessageId,
}
/// Bounded startup selection through the dedicated pending Program notice index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreProgramNoticePage {
    /// Pending notices in lexical Session/message order.
    pub notices: Vec<StoreProgramNotice>,
    /// Whether another entry exists after this bounded page.
    pub has_more: bool,
}
