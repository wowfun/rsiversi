use super::{
    AgentControlRecord, Connection, Digest, MAXIMUM_STORE_CONTROL_PAGE_BYTES, OptionalExtension,
    Result, Serialize, SessionFact, SessionHeader, SessionId, Sha256, StoreError, bounded_text,
    cas_ownership, decode_u64, maintenance, params, read_session_header_row, sql_error, sqlite_u64,
    validation,
};
use rsi_agent_session_protocol::{DurableReference, ReferenceCollector};
use rsi_agent_store_protocol::{
    ReferenceCodecs, control_references, fact_references, header_references,
};

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
fn digest(encoded: &str) -> String {
    hex::encode(Sha256::digest(encoded.as_bytes()))
}
fn write(
    connection: &Connection,
    id: &SessionId,
    stream: &str,
    seq: u64,
    hash: &str,
    refs: Result<ReferenceCollector>,
    strict: bool,
) -> Result<()> {
    let refs = match refs {
        Ok(refs) => Some(refs),
        Err(StoreError::Unsupported(_)) if !strict => None,
        Err(error) => return Err(error),
    };
    connection.execute("INSERT INTO reference_records(session_id,stream,seq,sha256,complete) VALUES(?1,?2,?3,?4,?5)", params![id.as_str(),stream,sqlite_u64("reference sequence",seq)?,hash,refs.is_some()]).map_err(sql_error)?;
    if let Some(refs) = refs {
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
    }
    Ok(())
}
/// Transaction-local projection writer; the returned JSON is the canonical INSERT payload.
pub(super) struct Writer<'a> {
    connection: &'a Connection,
    id: &'a SessionId,
    codecs: &'a ReferenceCodecs,
    strict: bool,
}
impl<'a> Writer<'a> {
    pub(super) fn new(
        connection: &'a Connection,
        id: &'a SessionId,
        codecs: &'a ReferenceCodecs,
    ) -> Result<Self> {
        available(connection, id)?;
        let strict = connection
            .query_row(
                "SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        Ok(Self {
            connection,
            id,
            codecs,
            strict,
        })
    }
    fn encode(
        &self,
        stream: &str,
        seq: u64,
        value: &impl Serialize,
        refs: Result<ReferenceCollector>,
    ) -> Result<String> {
        let encoded = super::encode_json(stream, value)?;
        write(
            self.connection,
            self.id,
            stream,
            seq,
            &hex::encode(Sha256::digest(encoded.as_bytes())),
            refs,
            self.strict,
        )?;
        Ok(encoded)
    }
    pub(super) fn header(&self, header: &SessionHeader) -> Result<String> {
        self.encode("header", 0, header, header_references(header))
    }
    pub(super) fn fact(&self, fact: &SessionFact) -> Result<String> {
        self.encode("fact", fact.seq(), fact, fact_references(fact))
    }
    pub(super) fn control(&self, control: &AgentControlRecord) -> Result<String> {
        self.encode(
            "control",
            control.seq(),
            control,
            control_references(control, self.codecs),
        )
    }
}
fn verify(
    connection: &Connection,
    id: &SessionId,
    stream: &str,
    seq: u64,
    hash: &str,
    refs: &ReferenceCollector,
) -> Result<()> {
    let row = connection.query_row("SELECT sha256,complete FROM reference_records WHERE session_id=?1 AND stream=?2 AND seq=?3",params![id.as_str(),stream,sqlite_u64("reference sequence",seq)?], |row| Ok((bounded_text(row,0,64)?,row.get::<_,bool>(1)?))).optional().map_err(sql_error)?;
    let Some((stored, complete)) = row else {
        return Err(StoreError::Corrupt(
            "missing canonical reference coverage".into(),
        ));
    };
    if stored != hash {
        return Err(StoreError::Corrupt(
            "reference coverage digest differs from canonical record".into(),
        ));
    }
    if !complete {
        return Err(StoreError::Unsupported(
            "canonical Domain reference coverage requires explicit rebuild".into(),
        ));
    }
    let mut statement = connection.prepare("SELECT target_kind,target_id,target_bytes FROM reference_edges WHERE session_id=?1 AND stream=?2 AND seq=?3 ORDER BY target_kind,target_id").map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            id.as_str(),
            stream,
            sqlite_u64("reference sequence", seq)?
        ])
        .map_err(sql_error)?;
    let mut observed = ReferenceCollector::default();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let kind = bounded_text(row, 0, 7).map_err(sql_error)?;
        let target = bounded_text(row, 1, 256).map_err(sql_error)?;
        let bytes = decode_u64("reference bytes", row.get(2).map_err(sql_error)?)?;
        match kind.as_str() {
            "session" if bytes == 0 => observed
                .session(&SessionId::new(target).map_err(|e| StoreError::Corrupt(e.to_string()))?)
                .map_err(StoreError::Corrupt)?,
            "cas" => observed.cas(&target, bytes).map_err(StoreError::Corrupt)?,
            _ => {
                return Err(StoreError::Corrupt(
                    "invalid reference edge kind or length".into(),
                ));
            }
        }
    }
    if !refs.iter().eq(observed.iter()) {
        return Err(StoreError::Corrupt(
            "canonical reference edges differ from their typed source".into(),
        ));
    }
    Ok(())
}
struct Observer<'a> {
    connection: &'a Connection,
    codecs: &'a ReferenceCodecs,
    records: u64,
}
impl validation::CanonicalObserver for Observer<'_> {
    fn fact(&mut self, id: &SessionId, fact: &SessionFact, encoded: &str) -> Result<()> {
        verify(
            self.connection,
            id,
            "fact",
            fact.seq(),
            &digest(encoded),
            &fact_references(fact)?,
        )?;
        self.records += 1;
        Ok(())
    }
    fn control(
        &mut self,
        id: &SessionId,
        control: &AgentControlRecord,
        encoded: &str,
    ) -> Result<()> {
        verify(
            self.connection,
            id,
            "control",
            control.seq(),
            &digest(encoded),
            &control_references(control, self.codecs)?,
        )?;
        self.records += 1;
        Ok(())
    }
}
pub(super) fn validate_all(connection: &Connection, codecs: &ReferenceCodecs) -> Result<()> {
    let mut statement = connection.prepare("SELECT session_id FROM sessions WHERE session_id NOT IN (SELECT session_id FROM session_tombstones) ORDER BY session_id").map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    let mut observer = Observer {
        connection,
        codecs,
        records: 0,
    };
    while let Some(row) = rows.next().map_err(sql_error)? {
        let id = SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let (header, _, encoded) = validation::read_session_header_source(connection, &id)?;
        verify(
            connection,
            &id,
            "header",
            0,
            &digest(&encoded),
            &header_references(&header)?,
        )?;
        observer.records += 1;
        validation::validate_session_observed(connection, &id, &mut observer)?;
        validation::validate_canonical_fact_prefix(connection, &id, &mut observer)?;
    }
    observer.records += validate_retiring(connection, codecs)?;
    let indexed = connection
        .query_row("SELECT COUNT(*) FROM reference_records", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(sql_error)?;
    if decode_u64("reference record count", indexed)? != observer.records {
        return Err(StoreError::Corrupt(
            "extra canonical reference coverage".into(),
        ));
    }
    let orphans = connection.query_row("SELECT EXISTS(SELECT 1 FROM reference_edges e LEFT JOIN reference_records r ON (e.session_id,e.stream,e.seq)=(r.session_id,r.stream,r.seq) WHERE r.session_id IS NULL)", [], |row| row.get::<_,bool>(0)).map_err(sql_error)?;
    if orphans {
        return Err(StoreError::Corrupt("orphaned reference edge".into()));
    }
    Ok(())
}
pub(super) fn remove(connection: &Connection, id: &str, stream: &str, seq: i64) -> Result<()> {
    connection
        .execute(
            "DELETE FROM reference_edges WHERE session_id=?1 AND stream=?2 AND seq=?3",
            params![id, stream, seq],
        )
        .map_err(sql_error)?;
    connection
        .execute(
            "DELETE FROM reference_records WHERE session_id=?1 AND stream=?2 AND seq=?3",
            params![id, stream, seq],
        )
        .map_err(sql_error)?;
    Ok(())
}

// A retired prefix is intentionally incomplete. Authenticate each surviving
// canonical record and its exact projection without pretending it is replayable.
pub(super) fn validate_retiring(connection: &Connection, codecs: &ReferenceCodecs) -> Result<u64> {
    validate_retiring_operation(connection, codecs, None)
}
pub(super) fn validate_retiring_operation(
    connection: &Connection,
    codecs: &ReferenceCodecs,
    operation: Option<&str>,
) -> Result<u64> {
    validate_retirement_bindings(connection, operation)?;
    if let Some(operation) = operation {
        if maintenance::operation_request(connection, operation)?.is_none() {
            return Err(StoreError::NotFound(operation.into()));
        }
        let strict: bool = connection
            .query_row(
                "SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !strict {
            return Err(StoreError::Corrupt(
                "retirement requires strict reference admission".into(),
            ));
        }
    }
    let mut records = 0;
    for (table, stream, column, seq, maximum) in [
        (
            "sessions",
            "header",
            "header_json",
            "0",
            rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES,
        ),
        (
            "facts",
            "fact",
            "fact_json",
            "seq",
            rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES,
        ),
        (
            "agent_controls",
            "control",
            "control_json",
            "seq",
            MAXIMUM_STORE_CONTROL_PAGE_BYTES,
        ),
    ] {
        let mut statement = connection.prepare(&format!("SELECT session_id,{seq},{column} FROM {table} WHERE session_id IN (SELECT session_id FROM session_tombstones WHERE state='retiring' AND (?1 IS NULL OR operation_id=?1)) ORDER BY session_id,2")).map_err(sql_error)?;
        let mut rows = statement.query([operation]).map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let id = SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;
            let seq = decode_u64(
                "retiring reference sequence",
                row.get(1).map_err(sql_error)?,
            )?;
            let encoded = bounded_text(row, 2, maximum).map_err(sql_error)?;
            verify_encoded(connection, codecs, &id, stream, seq, &encoded)?;
            records += 1;
        }
    }
    let invalid = connection.query_row("SELECT EXISTS(SELECT 1 FROM session_tombstones t JOIN purge_operations p USING(operation_id) WHERE (t.state='purged') != (p.complete=1)) OR EXISTS(SELECT 1 FROM session_tombstones t JOIN sessions s USING(session_id) WHERE t.state='purged')",[],|row| row.get::<_,bool>(0)).map_err(sql_error)?;
    if invalid {
        return Err(StoreError::Corrupt(
            "retirement receipt state differs".into(),
        ));
    }
    let indexed: i64 = connection.query_row("SELECT COUNT(*) FROM reference_records WHERE session_id IN (SELECT session_id FROM session_tombstones WHERE (?1 IS NULL OR operation_id=?1))", [operation], |row| row.get(0)).map_err(sql_error)?;
    if decode_u64("retiring reference count", indexed)? != records {
        return Err(StoreError::Corrupt(
            "extra retiring reference coverage".into(),
        ));
    }
    let orphans: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM reference_edges e LEFT JOIN reference_records r ON (e.session_id,e.stream,e.seq)=(r.session_id,r.stream,r.seq) WHERE r.session_id IS NULL AND e.session_id IN (SELECT session_id FROM session_tombstones WHERE (?1 IS NULL OR operation_id=?1)))", [operation], |row| row.get(0)).map_err(sql_error)?;
    if orphans {
        return Err(StoreError::Corrupt(
            "orphan retiring reference edges".into(),
        ));
    }
    Ok(records)
}

pub(super) fn verify_encoded(
    connection: &Connection,
    codecs: &ReferenceCodecs,
    id: &SessionId,
    stream: &str,
    seq: u64,
    encoded: &str,
) -> Result<()> {
    let refs = match stream {
        "header" => {
            let value: SessionHeader =
                serde_json::from_str(encoded).map_err(|e| StoreError::Corrupt(e.to_string()))?;
            header_references(&value)?
        }
        "fact" => {
            let value: SessionFact =
                serde_json::from_str(encoded).map_err(|e| StoreError::Corrupt(e.to_string()))?;
            if value.seq() != seq {
                return Err(StoreError::Corrupt("retiring Fact sequence differs".into()));
            }
            fact_references(&value)?
        }
        _ => {
            let value: AgentControlRecord =
                serde_json::from_str(encoded).map_err(|e| StoreError::Corrupt(e.to_string()))?;
            if value.seq() != seq {
                return Err(StoreError::Corrupt(
                    "retiring control sequence differs".into(),
                ));
            }
            control_references(&value, codecs)?
        }
    };
    verify(connection, id, stream, seq, &digest(encoded), &refs)
}

struct Rebuilder<'a> {
    connection: &'a Connection,
    codecs: &'a ReferenceCodecs,
    records: u64,
}
impl validation::CanonicalObserver for Rebuilder<'_> {
    fn fact(&mut self, id: &SessionId, fact: &SessionFact, encoded: &str) -> Result<()> {
        write(
            self.connection,
            id,
            "fact",
            fact.seq(),
            &digest(encoded),
            fact_references(fact),
            true,
        )?;
        self.records += 1;
        Ok(())
    }
    fn control(
        &mut self,
        id: &SessionId,
        control: &AgentControlRecord,
        encoded: &str,
    ) -> Result<()> {
        write(
            self.connection,
            id,
            "control",
            control.seq(),
            &digest(encoded),
            control_references(control, self.codecs),
            true,
        )?;
        self.records += 1;
        Ok(())
    }
}
pub(super) fn rebuild(connection: &Connection, codecs: &ReferenceCodecs) -> Result<u64> {
    connection
        .execute("DELETE FROM reference_edges", [])
        .map_err(sql_error)?;
    connection
        .execute("DELETE FROM reference_records", [])
        .map_err(sql_error)?;
    connection.execute("INSERT INTO reference_policy(id,strict) VALUES(1,1) ON CONFLICT(id) DO UPDATE SET strict=1",[]).map_err(sql_error)?;
    let mut rebuilder = Rebuilder {
        connection,
        codecs,
        records: 0,
    };
    let mut cursor = None;
    loop {
        let ids = validation::session_id_page(connection, cursor.as_ref())?;
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            let (header, _, encoded) = validation::read_session_header_source(connection, id)?;
            write(
                connection,
                id,
                "header",
                0,
                &digest(&encoded),
                header_references(&header),
                true,
            )?;
            rebuilder.records += 1;
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
