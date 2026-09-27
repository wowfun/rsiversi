//! Bounded receipt lookup and immutable queue-slot projections.
use super::{
    AgentControlRecord, AgentControlRecordBody as Body, Connection, OptionalExtension, Result,
    SessionId, StoreError, decode_projected_json, decode_u64, params, sql_error, sqlite_u64,
};
use rsi_agent_session_protocol::{
    MAXIMUM_QUEUE_MUTATION_RECEIPT_BYTES, MAXIMUM_SESSION_FACT_BYTES, QueueMutationReceipt,
    QueueOperationId, QueueSlot,
};

// Includes the enclosing control record's discriminator, sequence and timestamp.
const MAXIMUM_RECEIPT_CONTROL_BYTES: usize = MAXIMUM_QUEUE_MUTATION_RECEIPT_BYTES + 1024;

pub(super) fn read_control(
    connection: &Connection,
    session: &SessionId,
    seq: u64,
    maximum: usize,
) -> Result<AgentControlRecord> {
    let maximum_sql = i64::try_from(maximum)
        .map_err(|_| StoreError::Invalid("queue read bound overflow".into()))?;
    let bytes = connection
        .query_row(
            "SELECT length(CAST(control_json AS BLOB)),
                CASE WHEN length(CAST(control_json AS BLOB)) <= ?3 THEN control_json END
             FROM agent_controls WHERE session_id=?1 AND seq=?2",
            params![
                session.as_str(),
                sqlite_u64("queue control", seq)?,
                maximum_sql
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::Corrupt("queue control is absent".into()))?;
    let record: AgentControlRecord = decode_projected_json("queue control", bytes, maximum)?;
    if record.seq() != seq {
        return Err(StoreError::Corrupt("queue control cursor differs".into()));
    }
    Ok(record)
}

pub(super) fn acceptance_slot(
    connection: &Connection,
    session: &SessionId,
    acceptance: &AgentControlRecord,
) -> Result<QueueSlot> {
    if acceptance.seq() > 1 {
        // Inspect the discriminator in SQLite before projecting any preceding large payload.
        let linked = connection
            .query_row(
                "SELECT json_extract(control_json, '$.type') = 'message_successor'
                 FROM agent_controls WHERE session_id=?1 AND seq=?2",
                params![
                    session.as_str(),
                    sqlite_u64("previous queue control", acceptance.seq() - 1)?
                ],
                |row| row.get::<_, Option<bool>>(0),
            )
            .optional()
            .map_err(sql_error)?
            .flatten()
            .unwrap_or(false);
        if linked {
            let previous = read_control(connection, session, acceptance.seq() - 1, 4096)?;
            return acceptance_slot_from_link(acceptance, Some(&previous));
        }
    }
    acceptance_slot_from_link(acceptance, None)
}

pub(super) fn acceptance_slot_from_link(
    acceptance: &AgentControlRecord,
    previous: Option<&AgentControlRecord>,
) -> Result<QueueSlot> {
    let Body::MessageAccepted { message, .. } = acceptance.body() else {
        return Err(StoreError::Corrupt("queue acceptance is absent".into()));
    };
    if let Some(previous) = previous {
        let Body::MessageSuccessor {
            successor_id, slot, ..
        } = previous.body()
        else {
            return Err(StoreError::Corrupt(
                "queue predecessor is not a successor link".into(),
            ));
        };
        if previous.seq().checked_add(1) != Some(acceptance.seq())
            || successor_id != &message.message_id
        {
            return Err(StoreError::Corrupt(
                "queue successor identity or sequence differs".into(),
            ));
        }
        return Ok(slot.clone());
    }
    Ok(QueueSlot::initial(
        &message.message_id,
        acceptance.timestamp_ms(),
        acceptance.seq(),
    ))
}

pub(super) fn count(connection: &Connection, session: &SessionId) -> Result<usize> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM queue_mutations WHERE session_id=?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let count = usize::try_from(count).map_err(|_| {
        StoreError::Corrupt("queue receipt count is negative or overflowing".into())
    })?;
    Ok(count)
}

pub(super) fn receipt_position(
    connection: &Connection,
    session: &SessionId,
    operation: &QueueOperationId,
) -> Result<Option<u64>> {
    let seq: Option<i64> = connection
        .query_row(
            "SELECT control_seq FROM queue_mutations WHERE session_id=?1 AND operation_id=?2",
            params![session.as_str(), operation.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    seq.map(|seq| decode_u64("queue receipt cursor", seq))
        .transpose()
}

pub(super) fn read(
    connection: &Connection,
    session: &SessionId,
    operation: &QueueOperationId,
) -> Result<Option<QueueMutationReceipt>> {
    let Some(seq) = receipt_position(connection, session, operation)? else {
        return Ok(None);
    };
    let record = read_control(connection, session, seq, MAXIMUM_RECEIPT_CONTROL_BYTES)?;
    let Body::QueueMutationRecorded { receipt } = record.body() else {
        return Err(StoreError::Corrupt(
            "queue receipt index points to another control".into(),
        ));
    };
    if &receipt.operation_id != operation {
        return Err(StoreError::Corrupt(
            "queue receipt operation differs".into(),
        ));
    }
    receipt
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(Some(receipt.clone()))
}

pub(super) fn insert(
    connection: &Connection,
    session: &SessionId,
    receipt: &QueueMutationReceipt,
) -> Result<()> {
    receipt
        .validate()
        .map_err(|error| StoreError::Invalid(error.to_string()))?;
    let inserted = connection
        .execute(
            "INSERT INTO queue_mutations (session_id,operation_id,control_seq) VALUES (?1,?2,?3)
             ON CONFLICT(session_id,operation_id) DO NOTHING",
            params![
                session.as_str(),
                receipt.operation_id.as_str(),
                sqlite_u64("queue receipt cursor", receipt.control_seq)?
            ],
        )
        .map_err(sql_error)?;
    if inserted == 0 {
        return Err(StoreError::Invalid(
            "queue receipt operation already exists".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_successor(
    connection: &Connection,
    session: &SessionId,
    predecessor: &super::StoreAgentMessage,
    link: &AgentControlRecord,
) -> Result<()> {
    let acceptance = read_control(
        connection,
        session,
        link.seq() + 1,
        MAXIMUM_SESSION_FACT_BYTES,
    )?;
    let receipt = read_control(
        connection,
        session,
        link.seq() + 2,
        MAXIMUM_RECEIPT_CONTROL_BYTES,
    )?;
    rsi_agent_store_protocol::validate_queue_successor(predecessor, link, &acceptance, &receipt)?;
    rsi_agent_store_protocol::validate_queue_suffix(&[link.clone(), acceptance, receipt])
}

pub(super) fn validate_receipt_boundary(
    connection: &Connection,
    session: &SessionId,
    record: &AgentControlRecord,
) -> Result<()> {
    use rsi_agent_session_protocol::{AgentMessageSource, QueueMutationOutcome};
    let Body::QueueMutationRecorded { receipt } = record.body() else {
        return Ok(());
    };
    let span = match receipt.outcome {
        QueueMutationOutcome::Rejected { .. } => 0,
        QueueMutationOutcome::Withdrawn => 1,
        _ => 2,
    };
    let mut records = Vec::with_capacity(span + 1);
    let start = record
        .seq()
        .checked_sub(span as u64)
        .filter(|seq| *seq > 0)
        .ok_or_else(|| StoreError::Corrupt("queue receipt has no mutation boundary".into()))?;
    for seq in start..record.seq() {
        records.push(read_control(
            connection,
            session,
            seq,
            MAXIMUM_SESSION_FACT_BYTES,
        )?);
    }
    records.push(record.clone());
    rsi_agent_store_protocol::validate_queue_suffix(&records)?;
    if matches!(receipt.outcome, QueueMutationOutcome::Withdrawn) {
        let predecessor = super::validation::read_indexed_agent_message(
            connection,
            session,
            &receipt.expected_message_id,
        )?;
        if predecessor.queue_slot.id != receipt.slot_id
            || predecessor.message.source != AgentMessageSource::Human
        {
            return Err(StoreError::Corrupt(
                "withdraw receipt has a foreign slot or source".into(),
            ));
        }
    }
    Ok(())
}
