use super::{
    AgentControlRecord, Connection, MAXIMUM_STORE_CONTROL_PAGE_BYTES, OptionalExtension, Result,
    SessionFact, SessionHeader, SessionId, StoreError, bounded_text, cas_ownership, decode_u64,
    maintenance, params, read_session_header_row, sql_error, sqlite_u64, validation,
};
use rsi_agent_session_protocol::{DurableReference, ReferenceCollector};
use rsi_agent_store_protocol::{control_references, fact_references, header_references};

pub(super) fn available(connection: &Connection, id: &SessionId) -> Result<()> {
    let retired = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM session_tombstones WHERE session_id=?1)",
            [id.as_str()],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if retired {
        Err(StoreError::NotFound(id.to_string()))
    } else {
        Ok(())
    }
}
fn write(
    connection: &Connection,
    id: &SessionId,
    stream: &str,
    seq: u64,
    refs: Result<ReferenceCollector>,
) -> Result<()> {
    let refs = refs?;
    for edge in refs.iter() {
        let (kind, target, bytes) = match edge {
            DurableReference::Session(target) => {
                available(connection, target)?;
                ("session", target.as_str(), 0)
            }
            DurableReference::Cas { sha256, byte_len } => {
                cas_ownership::available(connection, sha256)?;
                let stored = connection
                    .query_row(
                        "SELECT byte_len FROM cas_objects WHERE sha256=?1",
                        [sha256],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(sql_error)?
                    .ok_or_else(|| StoreError::NotFound(sha256.clone()))?;
                if decode_u64("CAS reference bytes", stored)? != *byte_len {
                    return Err(StoreError::Corrupt("canonical CAS length differs".into()));
                }
                ("cas", sha256.as_str(), *byte_len)
            }
        };
        connection.execute("INSERT INTO reference_edges(session_id,stream,seq,target_kind,target_id,target_bytes) VALUES(?1,?2,?3,?4,?5,?6)", params![id.as_str(),stream,sqlite_u64("reference sequence",seq)?,kind,target,sqlite_u64("reference bytes",bytes)?]).map_err(sql_error)?;
    }
    Ok(())
}
/// Writes the edges of canonical records in their owning transaction.
pub(super) struct Writer<'a> {
    connection: &'a Connection,
    id: &'a SessionId,
}
impl<'a> Writer<'a> {
    pub(super) fn new(connection: &'a Connection, id: &'a SessionId) -> Result<Self> {
        available(connection, id)?;
        Ok(Self { connection, id })
    }
    pub(super) fn header(&self, header: &SessionHeader) -> Result<String> {
        let encoded = super::encode_json("header", header)?;
        write(
            self.connection,
            self.id,
            "header",
            0,
            header_references(header),
        )?;
        Ok(encoded)
    }
    pub(super) fn fact(&self, fact: &SessionFact) -> Result<()> {
        write(
            self.connection,
            self.id,
            "fact",
            fact.seq(),
            fact_references(fact),
        )
    }
    pub(super) fn control(&self, control: &AgentControlRecord) -> Result<()> {
        write(
            self.connection,
            self.id,
            "control",
            control.seq(),
            control_references(control),
        )
    }
}
fn verify(
    connection: &Connection,
    id: &SessionId,
    stream: &str,
    seq: u64,
    refs: &ReferenceCollector,
) -> Result<u64> {
    let mut expected = refs.iter().peekable();
    if expected.peek().is_none() {
        return Ok(0);
    }
    // The key orders Session edges before CAS, matching DurableReference.
    let mut statement = connection
        .prepare_cached(
            "SELECT target_kind,target_id,target_bytes FROM reference_edges
         WHERE session_id=?1 AND stream=?2 AND seq=?3 ORDER BY target_kind DESC,target_id",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            id.as_str(),
            stream,
            sqlite_u64("reference sequence", seq)?
        ])
        .map_err(sql_error)?;
    let mut count = 0;
    for reference in expected {
        let (kind, target, bytes) = match reference {
            DurableReference::Session(target) => ("session", target.as_str(), 0),
            DurableReference::Cas { sha256, byte_len } => ("cas", sha256.as_str(), *byte_len),
        };
        let row = rows
            .next()
            .map_err(sql_error)?
            .ok_or_else(|| StoreError::Corrupt("missing canonical reference edge".into()))?;
        if super::text::borrowed_text(row, 0, 7).map_err(sql_error)? != kind
            || super::text::borrowed_text(row, 1, 256).map_err(sql_error)? != target
            || decode_u64("reference bytes", row.get(2).map_err(sql_error)?)? != bytes
        {
            return Err(StoreError::Corrupt(
                "canonical reference edge differs from its source".into(),
            ));
        }
        count += 1;
    }
    Ok(count)
}
struct Observer<'a> {
    connection: &'a Connection,
    edges: u64,
}
impl validation::CanonicalObserver for Observer<'_> {
    fn header(&mut self, header: &SessionHeader) -> Result<()> {
        self.edges += verify(
            self.connection,
            header.session_id(),
            "header",
            0,
            &header_references(header)?,
        )?;
        Ok(())
    }
    fn fact(&mut self, id: &SessionId, fact: &SessionFact) -> Result<()> {
        self.edges += verify(
            self.connection,
            id,
            "fact",
            fact.seq(),
            &fact_references(fact)?,
        )?;
        Ok(())
    }
    fn control(&mut self, id: &SessionId, control: &AgentControlRecord) -> Result<()> {
        self.edges += verify(
            self.connection,
            id,
            "control",
            control.seq(),
            &control_references(control)?,
        )?;
        Ok(())
    }
}
pub(super) fn validate_all(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("SELECT session_id FROM sessions WHERE session_id NOT IN (SELECT session_id FROM session_tombstones) ORDER BY session_id").map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    let mut observer = Observer {
        connection,
        edges: 0,
    };
    while let Some(row) = rows.next().map_err(sql_error)? {
        let id = SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        validation::validate_session_observed(connection, &id, &mut observer)?;
        validation::validate_canonical_fact_prefix(connection, &id, &mut observer)?;
    }
    observer.finish()
}

impl Observer<'_> {
    fn finish(self) -> Result<()> {
        let connection = self.connection;
        let expected = self.edges + validate_retiring(connection)?;
        let indexed = connection
            .query_row("SELECT COUNT(*) FROM reference_edges", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sql_error)?;
        if decode_u64("reference edge count", indexed)? != expected {
            return Err(StoreError::Corrupt(
                "reference edges without a canonical source".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn verify_database(connection: &Connection) -> Result<()> {
    let mut observer = Observer {
        connection,
        edges: 0,
    };
    validation::validate_database_observed(connection, &mut observer)?;
    observer.finish()
}

// A partially retired history still owns all edges of each surviving record.
fn validate_retiring(connection: &Connection) -> Result<u64> {
    validate_retirement_bindings(connection, None)?;
    let mut edges = 0;
    for (table, stream, column, seq, maximum) in [
        (
            "sessions",
            "header",
            "length(CAST(header_json AS BLOB)),header_json",
            "0",
            rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES,
        ),
        (
            "agent_controls",
            "control",
            "control_bytes,control_payload",
            "seq",
            MAXIMUM_STORE_CONTROL_PAGE_BYTES,
        ),
    ] {
        let mut statement = connection.prepare(&format!("SELECT session_id,{seq},{column} FROM {table} WHERE session_id IN (SELECT session_id FROM session_tombstones WHERE state='retiring') ORDER BY session_id,2")).map_err(sql_error)?;
        let mut rows = statement.query([]).map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let id = SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;
            let seq = decode_u64(
                "retiring reference sequence",
                row.get(1).map_err(sql_error)?,
            )?;
            let encoded = super::record_codec::payload(row, 2, maximum).map_err(sql_error)?;
            edges += verify_encoded(connection, &id, stream, seq, &encoded)?;
        }
    }
    {
        let mut statement = connection.prepare(fact_rows!("?1", "WHERE f.session_id IN (SELECT session_id FROM session_tombstones WHERE state='retiring') ORDER BY f.session_id,f.seq")).map_err(sql_error)?;
        let mut rows = statement
            .query(params![
                i64::try_from(super::MAXIMUM_SESSION_FACT_BYTES).expect("bounded Fact length")
            ])
            .map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let id = SessionId::new(bounded_text(row, 9, 256).map_err(sql_error)?)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            let fact = super::fact_storage::decode(row)?;
            edges += verify_fact(connection, &id, &fact)?;
        }
    }
    Ok(edges)
}

pub(super) fn verify_encoded(
    connection: &Connection,
    id: &SessionId,
    stream: &str,
    seq: u64,
    encoded: &str,
) -> Result<u64> {
    let refs = if stream == "header" {
        let value: SessionHeader =
            serde_json::from_str(encoded).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        header_references(&value)?
    } else {
        let value: AgentControlRecord =
            serde_json::from_str(encoded).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if value.seq() != seq {
            return Err(StoreError::Corrupt(
                "retiring control sequence differs".into(),
            ));
        }
        control_references(&value)?
    };
    verify(connection, id, stream, seq, &refs)
}

pub(super) fn verify_fact(
    connection: &Connection,
    id: &SessionId,
    fact: &SessionFact,
) -> Result<u64> {
    verify(connection, id, "fact", fact.seq(), &fact_references(fact)?)
}

struct Rebuilder<'a> {
    connection: &'a Connection,
    records: u64,
}
impl validation::CanonicalObserver for Rebuilder<'_> {
    fn header(&mut self, header: &SessionHeader) -> Result<()> {
        write(
            self.connection,
            header.session_id(),
            "header",
            0,
            header_references(header),
        )?;
        self.records += 1;
        Ok(())
    }
    fn fact(&mut self, id: &SessionId, fact: &SessionFact) -> Result<()> {
        write(
            self.connection,
            id,
            "fact",
            fact.seq(),
            fact_references(fact),
        )?;
        self.records += 1;
        Ok(())
    }
    fn control(&mut self, id: &SessionId, control: &AgentControlRecord) -> Result<()> {
        write(
            self.connection,
            id,
            "control",
            control.seq(),
            control_references(control),
        )?;
        self.records += 1;
        Ok(())
    }
}
pub(super) fn rebuild(connection: &Connection) -> Result<u64> {
    validate_retirement_bindings(connection, None)?;
    connection
        .execute("DELETE FROM reference_edges", [])
        .map_err(sql_error)?;
    let mut rebuilder = Rebuilder {
        connection,
        records: 0,
    };
    let mut cursor = None;
    loop {
        let ids = validation::session_id_page(connection, cursor.as_ref())?;
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            validation::validate_session_observed(connection, id, &mut rebuilder)?;
            validation::validate_canonical_fact_prefix(connection, id, &mut rebuilder)?;
        }
        cursor = ids.last().cloned();
    }
    Ok(rebuilder.records)
}

pub(super) fn validate_retirement_bindings(
    connection: &Connection,
    operation: Option<&str>,
) -> Result<()> {
    let sql = if operation.is_some() {
        "SELECT operation_id FROM purge_operations WHERE operation_id=?1"
    } else {
        "SELECT operation_id FROM purge_operations WHERE ?1 IS NULL ORDER BY operation_id"
    };
    let mut operations = connection.prepare(sql).map_err(sql_error)?;
    let mut rows = operations.query([operation]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let operation = bounded_text(row, 0, 256).map_err(sql_error)?;
        let request = maintenance::operation_request(connection, &operation)?
            .ok_or_else(|| StoreError::Corrupt("missing retirement binding".into()))?;
        let invalid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM session_tombstones t JOIN purge_operations p USING(operation_id) WHERE t.operation_id=?1 AND (t.state='purged') != (p.complete=1)) OR EXISTS(SELECT 1 FROM session_tombstones t JOIN sessions s USING(session_id) WHERE t.operation_id=?1 AND t.state='purged')", [&operation], |row| row.get(0)).map_err(sql_error)?;
        if invalid {
            return Err(StoreError::Corrupt(
                "retirement receipt state differs".into(),
            ));
        }
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM session_tombstones WHERE operation_id=?1",
                [&operation],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sql_error)?;
        if usize::try_from(count).ok() != Some(request.targets.len()) {
            return Err(StoreError::Corrupt("retirement selection differs".into()));
        }
        let locations = maintenance::operation_locations(connection, &request)?;
        for (target, location) in request.targets.into_iter().zip(locations) {
            let bound=connection.query_row("SELECT EXISTS(SELECT 1 FROM session_tombstones WHERE operation_id=?1 AND session_id=?2)",params![operation,target.session_id.as_str()],|row| row.get::<_,bool>(0)).map_err(sql_error)?;
            if !bound {
                return Err(StoreError::Corrupt(
                    "retirement tombstone binding differs".into(),
                ));
            }
            match read_session_header_row(connection, &target.session_id) {
                Ok((header, tail))
                    if header
                        .fingerprint()
                        .map_err(|e| StoreError::Corrupt(e.to_string()))?
                        == target.header_sha256
                        && tail == target.fact_seq
                        && header.coordinates().location() == &location => {}
                Err(StoreError::NotFound(_)) => {}
                Ok(_) => {
                    return Err(StoreError::Corrupt(
                        "retiring Header differs from selection".into(),
                    ));
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}
