//! Physical Fact encoding and bounded logical reconstruction.

use super::*;

// The acceptance coordinate column avoids parsing unrelated (possibly large)
// Fact JSON to decide whether a join is needed. The JSON reference remains the
// canonical, hashed record; decoding checks the relational projection against it.
macro_rules! fact_rows {
    ($maximum:literal, $tail:literal) => {
        concat!(
            "SELECT CASE WHEN f.acceptance_control_seq IS NULL THEN length(CAST(f.fact_json AS BLOB)) ELSE f.logical_bytes END,
                    length(CAST(f.fact_json AS BLOB)),
                    CASE WHEN length(CAST(f.fact_json AS BLOB)) <= ", $maximum, " THEN f.fact_json END,
                    f.acceptance_control_seq, coalesce(length(CAST(c.control_json AS BLOB)),0),
                    CASE WHEN length(CAST(c.control_json AS BLOB)) <= ", $maximum, " THEN c.control_json END,
                    f.seq, f.turn_id, f.fact_kind, f.session_id
             FROM facts f LEFT JOIN agent_controls c
               ON c.session_id=f.session_id AND c.seq=f.acceptance_control_seq ", $tail
        )
    };
}

pub(super) fn read(
    connection: &Connection,
    session_id: &SessionId,
    sequence: i64,
) -> Result<(SessionFact, String)> {
    let mut statement = connection
        .prepare(fact_rows!("?3", "WHERE f.session_id=?1 AND f.seq=?2"))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            session_id.as_str(),
            sequence,
            i64::try_from(MAXIMUM_SESSION_FACT_BYTES).expect("bounded Fact length")
        ])
        .map_err(sql_error)?;
    let row = rows
        .next()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::Corrupt("absent canonical Fact".into()))?;
    decode(row).map(|(fact, encoded)| (fact, encoded.to_owned()))
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ReferencedFact {
    InputMessageClaimed {
        seq: u64,
        timestamp_ms: u64,
        turn_id: TurnId,
        step_id: StepId,
        accepted_control_seq: u64,
    },
}

// Canonical control replay owns routing and option validation. This read
// projection only needs the accepted identity and payload; SessionFact::new
// validates the logical message before exposing it to consumers.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClaimAcceptance {
    MessageAccepted { seq: u64, message: AgentMessage },
}

pub(super) fn encode(
    fact: &SessionFact,
    acceptance: Option<u64>,
    references: &super::reference_index::Writer<'_>,
) -> Result<String> {
    if let Some(accepted_control_seq) = acceptance {
        let SessionFactBody::InputMessageClaimed {
            turn_id, step_id, ..
        } = fact.body()
        else {
            return Err(StoreError::Invalid(
                "only mailbox claims can reference acceptance".into(),
            ));
        };
        references.fact_encoding(
            fact,
            &ReferencedFact::InputMessageClaimed {
                seq: fact.seq(),
                timestamp_ms: fact.timestamp_ms(),
                turn_id: turn_id.clone(),
                step_id: step_id.clone(),
                accepted_control_seq,
            },
        )
    } else {
        references.fact(fact)
    }
}

pub(super) fn logical_len(row: &rusqlite::Row<'_>) -> Result<usize> {
    let length = usize::try_from(row.get::<_, i64>(0).map_err(sql_error)?)
        .map_err(|_| StoreError::Corrupt("negative logical Fact length".into()))?;
    if length == 0 || length > MAXIMUM_SESSION_FACT_BYTES {
        return Err(StoreError::Corrupt(
            "session Fact exceeds its byte bound".into(),
        ));
    }
    Ok(length)
}

/// Called only after the caller admits this row's logical bytes into its budget.
/// Returns the owned logical value and borrowed physical bytes for coverage hashing.
pub(super) fn decode<'a>(row: &'a rusqlite::Row<'_>) -> Result<(SessionFact, &'a str)> {
    let length = logical_len(row)?;
    let physical = (
        row.get::<_, i64>(1).map_err(sql_error)?,
        super::text::optional_borrowed_text(row, 2, MAXIMUM_SESSION_FACT_BYTES)
            .map_err(sql_error)?,
    );
    let acceptance = row.get::<_, Option<i64>>(3).map_err(sql_error)?;
    let (fact, encoded) = if let Some(acceptance) = acceptance {
        let (record, encoded): (ReferencedFact, _) = super::cas::decode_projected_json_with_source(
            "referenced Fact",
            physical,
            MAXIMUM_SESSION_FACT_BYTES,
        )?;
        let ReferencedFact::InputMessageClaimed {
            seq,
            timestamp_ms,
            turn_id,
            step_id,
            accepted_control_seq,
        } = record;
        if accepted_control_seq != decode_u64("claim acceptance", acceptance)? {
            return Err(StoreError::Corrupt(
                "claim acceptance coordinate differs".into(),
            ));
        }
        let ClaimAcceptance::MessageAccepted {
            seq: control_seq,
            message,
        } = super::cas::decode_projected_json_with_source(
            "claim acceptance",
            (
                row.get::<_, i64>(4).map_err(sql_error)?,
                super::text::optional_borrowed_text(row, 5, MAXIMUM_SESSION_FACT_BYTES)
                    .map_err(sql_error)?,
            ),
            MAXIMUM_SESSION_FACT_BYTES,
        )?
        .0;
        if control_seq != accepted_control_seq {
            return Err(StoreError::Corrupt(
                "claim acceptance sequence differs".into(),
            ));
        }
        let (source, content) = message.into_claimed_parts();
        let fact = SessionFact::new(
            seq,
            timestamp_ms,
            SessionFactBody::InputMessageClaimed {
                turn_id,
                step_id,
                source,
                content,
            },
        )
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        (fact, encoded)
    } else {
        super::cas::decode_projected_json_with_source(
            "session Fact",
            physical,
            MAXIMUM_SESSION_FACT_BYTES,
        )?
    };
    if fact.encoded_len() != length
        || fact.seq() != decode_u64("Fact sequence", row.get(6).map_err(sql_error)?)?
        || fact.body().turn_id().as_str() != bounded_text(row, 7, 256).map_err(sql_error)?
        || fact_index_kind(fact.body()) != bounded_text(row, 8, 8).map_err(sql_error)?
    {
        return Err(StoreError::Corrupt(
            "indexed Fact differs from its stored projection".into(),
        ));
    }
    Ok((fact, encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_fact_text_borrows_the_current_sqlite_row() {
        let expected = SessionFact::new(
            1,
            10,
            SessionFactBody::ModelStarted {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: rsi_agent_session_protocol::EffectId::new("effect").unwrap(),
            },
        )
        .unwrap();
        let json = serde_json::to_string(&expected).unwrap();
        let db = Connection::open_in_memory().unwrap();
        let decoded = db
            .query_row(
                "SELECT length(CAST(?1 AS BLOB)), length(CAST(?1 AS BLOB)), ?1,
                        NULL, 0, NULL, 1, 'turn', 'event', 'session'",
                [&json],
                |row| {
                    let raw = row.get_ref(2)?.as_str().unwrap();
                    let (fact, encoded) = decode(row).unwrap();
                    assert_eq!(encoded.as_ptr(), raw.as_ptr(), "do not copy physical JSON");
                    assert_eq!(encoded, json);
                    Ok(fact)
                },
            )
            .unwrap();
        drop(db);
        assert_eq!(
            decoded, expected,
            "the logical Fact outlives the SQLite row"
        );
    }
}
