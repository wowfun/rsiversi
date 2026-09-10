//! Canonical domain positions, revision admission and bounded historical reads.

use super::{
    AgentControlRecord, AgentControlRecordBody, Connection, OptionalExtension, Result, SessionId,
    StoreError, decode_projected_json, decode_u64, params, sql_error, sqlite_u64,
};
use rsi_agent_session_protocol::{
    DomainIdentity, DomainMutationSource, DomainRequestId, DomainRevision, DomainStateCommit,
    MAXIMUM_DOMAIN_BASELINE_BYTES, MAXIMUM_SESSION_DOMAINS,
};
use rsi_agent_store_protocol::{
    StoreDomainHead, StoreDomainState, StoreDomainStatePage, domain_heads_after,
};

// Complete snapshot bytes plus the bounded request/provenance/revision/control wrappers.
const MAXIMUM_DOMAIN_CONTROL_BYTES: usize = MAXIMUM_DOMAIN_BASELINE_BYTES + 16 * 1024;

type HeadRow = (Option<String>, i64, i64, i64, i64, i64);

fn head_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeadRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn decode_head(row: HeadRow) -> Result<StoreDomainHead> {
    let version = u32::try_from(row.3)
        .map_err(|_| StoreError::Corrupt("invalid domain codec version".into()))?;
    Ok(StoreDomainHead {
        identity: DomainIdentity::new(
            row.0.ok_or_else(|| {
                StoreError::Corrupt("domain identity exceeds its stored byte bound".into())
            })?,
            version,
        )
        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        control_seq: decode_u64("domain control sequence", row.1)?,
        update_index: usize::try_from(row.2)
            .map_err(|_| StoreError::Corrupt("invalid domain update index".into()))?,
        revision: DomainRevision::new(decode_u64("domain revision", row.4)?),
        snapshot_bytes: usize::try_from(row.5)
            .map_err(|_| StoreError::Corrupt("invalid domain snapshot size".into()))?,
    })
}

pub(super) fn read_heads(
    connection: &Connection,
    session: &SessionId,
) -> Result<Vec<StoreDomainHead>> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(CAST(domain_id AS BLOB)) <= 256 THEN domain_id END, control_seq, update_index, codec_version, revision, snapshot_bytes
        FROM domain_heads WHERE session_id = ?1 ORDER BY domain_id LIMIT 65",
        )
        .map_err(sql_error)?;
    let heads = statement
        .query_map([session.as_str()], head_row)
        .map_err(sql_error)?
        .map(|row| decode_head(row.map_err(sql_error)?))
        .collect::<Result<Vec<_>>>()?;
    if heads.len() > MAXIMUM_SESSION_DOMAINS {
        return Err(StoreError::Corrupt(
            "domain head count exceeds its bound".into(),
        ));
    }
    Ok(heads)
}

fn read_version(
    connection: &Connection,
    session: &SessionId,
    id: &str,
    horizon: u64,
) -> Result<Option<StoreDomainHead>> {
    connection
        .query_row(
            "SELECT domain_id, control_seq, update_index, codec_version, revision, snapshot_bytes
        FROM domain_versions WHERE session_id = ?1 AND domain_id = ?2 AND control_seq <= ?3
        ORDER BY control_seq DESC LIMIT 1",
            params![session.as_str(), id, sqlite_u64("domain horizon", horizon)?],
            head_row,
        )
        .optional()
        .map_err(sql_error)?
        .map(decode_head)
        .transpose()
}

fn request_position(
    connection: &Connection,
    session: &SessionId,
    request: &DomainRequestId,
) -> Result<Option<u64>> {
    connection
        .query_row(
            "SELECT control_seq FROM domain_requests WHERE session_id = ?1 AND request_id = ?2",
            params![session.as_str(), request.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?
        .map(|seq| decode_u64("domain request control sequence", seq))
        .transpose()
}

fn read_control(
    connection: &Connection,
    session: &SessionId,
    seq: u64,
) -> Result<AgentControlRecord> {
    let bytes = connection
        .query_row(
            "SELECT length(CAST(control_json AS BLOB)),
        CASE WHEN length(CAST(control_json AS BLOB)) <= ?3 THEN control_json END
        FROM agent_controls WHERE session_id = ?1 AND seq = ?2",
            params![
                session.as_str(),
                sqlite_u64("domain control sequence", seq)?,
                i64::try_from(MAXIMUM_DOMAIN_CONTROL_BYTES).unwrap()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::Corrupt("domain canonical control is absent".into()))?;
    let record: AgentControlRecord =
        decode_projected_json("domain control", bytes, MAXIMUM_DOMAIN_CONTROL_BYTES)?;
    if record.seq() != seq
        || !matches!(
            record.body(),
            AgentControlRecordBody::DomainStateCommitted { .. }
        )
    {
        return Err(StoreError::Corrupt(
            "domain position differs from its canonical control".into(),
        ));
    }
    Ok(record)
}

pub(super) fn read_request(
    connection: &Connection,
    session: &SessionId,
    request: &DomainRequestId,
) -> Result<Option<AgentControlRecord>> {
    let Some(seq) = request_position(connection, session, request)? else {
        return Ok(None);
    };
    let record = read_control(connection, session, seq)?;
    if !matches!(record.body(), AgentControlRecordBody::DomainStateCommitted { commit } if commit.request_id() == Some(request))
    {
        return Err(StoreError::Corrupt(
            "domain request index differs from its canonical identity".into(),
        ));
    }
    Ok(Some(record))
}

pub(super) fn read_states(
    connection: &Connection,
    session: &SessionId,
    horizon: Option<u64>,
) -> Result<StoreDomainStatePage> {
    let (fact, control) = connection
        .query_row(
            "SELECT durable_seq, control_seq FROM sessions WHERE session_id = ?1",
            [session.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::NotFound(session.to_string()))?;
    let durable_control_seq = decode_u64("control watermark", control)?;
    let selected_control_seq = horizon.unwrap_or(durable_control_seq);
    if selected_control_seq > durable_control_seq {
        return Err(StoreError::Invalid(
            "domain horizon exceeds current control tail".into(),
        ));
    }
    let mut states = Vec::new();
    for current in read_heads(connection, session)? {
        let Some(head) = read_version(
            connection,
            session,
            current.identity.id(),
            selected_control_seq,
        )?
        else {
            continue;
        };
        let record = read_control(connection, session, head.control_seq)?;
        let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
            unreachable!("read_control validated its kind");
        };
        let update = commit.updates().get(head.update_index).ok_or_else(|| {
            StoreError::Corrupt("domain update index exceeds its canonical record".into())
        })?;
        if update.revision() != head.revision {
            return Err(StoreError::Corrupt(
                "domain revision differs from its canonical update".into(),
            ));
        }
        states.push(StoreDomainState {
            head,
            snapshot: update.snapshot().clone(),
        });
    }
    let page = StoreDomainStatePage {
        durable_fact_seq: decode_u64("Fact watermark", fact)?,
        durable_control_seq,
        selected_control_seq,
        states,
    };
    page.validate()?;
    Ok(page)
}

pub(super) fn read_turn_usage(
    connection: &Connection,
    session: &SessionId,
    turn: &rsi_agent_session_protocol::TurnId,
) -> Result<rsi_agent_store_protocol::StoreTurnDomainUsage> {
    let (facts, controls, records, bytes) = connection.query_row(
        "SELECT durable_seq, control_seq,
            (SELECT COUNT(*) FROM domain_requests WHERE session_id = ?1 AND source_turn_id = ?2),
            (SELECT COALESCE(SUM(control_bytes), 0) FROM domain_requests WHERE session_id = ?1 AND source_turn_id = ?2)
        FROM sessions WHERE session_id = ?1",
        params![session.as_str(), turn.as_str()],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
    ).optional().map_err(sql_error)?.ok_or_else(|| StoreError::NotFound(session.to_string()))?;
    Ok(rsi_agent_store_protocol::StoreTurnDomainUsage {
        durable_fact_seq: decode_u64("domain usage Fact tail", facts)?,
        durable_control_seq: decode_u64("domain usage control tail", controls)?,
        records: decode_u64("domain usage records", records)?,
        bytes: decode_u64("domain usage bytes", bytes)?,
    })
}

fn source_turn(commit: &DomainStateCommit) -> Option<&str> {
    match commit.source() {
        DomainMutationSource::Turn { turn_id } => Some(turn_id.as_str()),
        DomainMutationSource::Baseline | DomainMutationSource::Command { .. } => None,
    }
}

pub(super) fn insert(
    connection: &Connection,
    session: &SessionId,
    minimum_fact_seq: u64,
    record: &AgentControlRecord,
    commit: &DomainStateCommit,
) -> Result<()> {
    if let Some(request) = commit.request_id()
        && request_position(connection, session, request)?.is_some()
    {
        return Err(StoreError::DomainRequestConflict {
            request_id: request.to_string(),
        });
    }
    if let DomainMutationSource::Turn { turn_id, .. } = commit.source() {
        let live = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ?1 AND turn_id = ?2
            AND (terminal_seq IS NULL OR terminal_seq >= ?3))",
                params![
                    session.as_str(),
                    turn_id.as_str(),
                    sqlite_u64("minimum append Fact", minimum_fact_seq)?
                ],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if !live {
            return Err(StoreError::Invalid(
                "domain mutation has no open originating Turn".into(),
            ));
        }
    }
    let next = domain_heads_after(&read_heads(connection, session)?, record.seq(), commit)?;
    for head in next.iter().filter(|head| head.control_seq == record.seq()) {
        let parameters = params![
            session.as_str(),
            head.identity.id(),
            sqlite_u64("domain control sequence", head.control_seq)?,
            i64::try_from(head.update_index).unwrap(),
            i64::from(head.identity.version()),
            sqlite_u64("domain revision", head.revision.get())?,
            i64::try_from(head.snapshot_bytes).unwrap()
        ];
        connection.execute("INSERT INTO domain_versions (session_id, domain_id, control_seq, update_index, codec_version, revision, snapshot_bytes)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", parameters).map_err(sql_error)?;
        connection.execute("INSERT INTO domain_heads (session_id, domain_id, control_seq, update_index, codec_version, revision, snapshot_bytes)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(session_id, domain_id) DO UPDATE SET
            control_seq = excluded.control_seq, update_index = excluded.update_index, codec_version = excluded.codec_version,
            revision = excluded.revision, snapshot_bytes = excluded.snapshot_bytes", parameters).map_err(sql_error)?;
    }
    if let Some(request) = commit.request_id() {
        connection.execute("INSERT INTO domain_requests (session_id, request_id, control_seq, source_turn_id, control_bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session.as_str(), request.as_str(), sqlite_u64("domain request sequence", record.seq())?, source_turn(commit), i64::try_from(record.encoded_len()).unwrap()]).map_err(sql_error)?;
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Projection {
    heads: Vec<StoreDomainHead>,
    versions: u64,
    requests: u64,
}

impl Projection {
    pub(super) fn apply(
        &mut self,
        connection: &Connection,
        session: &SessionId,
        record: &AgentControlRecord,
    ) -> Result<()> {
        let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
            return Ok(());
        };
        if let DomainMutationSource::Turn { turn_id, .. } = commit.source() {
            let valid = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ?1 AND turn_id = ?2
                AND (terminal_control_seq IS NULL OR terminal_control_seq > ?3))",
                    params![
                        session.as_str(),
                        turn_id.as_str(),
                        sqlite_u64("domain sequence", record.seq())?
                    ],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sql_error)?;
            if !valid {
                return Err(StoreError::Corrupt(
                    "domain control references an absent or already terminal Turn".into(),
                ));
            }
            if let Some(span) = commit.fact_span() {
                let mut builder = rsi_agent_session_protocol::DomainFactSpanBuilder::default();
                for seq in span.first_seq()..=span.last_seq() {
                    let fact = super::read_indexed_fact(
                        connection,
                        session,
                        sqlite_u64("domain Fact sequence", seq)?,
                    )?;
                    if fact.body().turn_id() != turn_id {
                        return Err(StoreError::Corrupt(
                            "domain request Fact span changed its originating Turn".into(),
                        ));
                    }
                    builder
                        .push(&fact)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                }
                if &builder
                    .finish()
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?
                    != span
                {
                    return Err(StoreError::Corrupt(
                        "domain request Fact span differs from canonical Facts".into(),
                    ));
                }
            }
        }
        let next = domain_heads_after(&self.heads, record.seq(), commit)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        for expected in next.iter().filter(|head| head.control_seq == record.seq()) {
            if read_version(connection, session, expected.identity.id(), record.seq())?.as_ref()
                != Some(expected)
            {
                return Err(StoreError::Corrupt(
                    "domain version index differs from canonical control".into(),
                ));
            }
            self.versions += 1;
        }
        if let Some(request) = commit.request_id() {
            if request_position(connection, session, request)? != Some(record.seq()) {
                return Err(StoreError::Corrupt(
                    "domain request index differs from canonical control".into(),
                ));
            }
            let valid = connection.query_row("SELECT source_turn_id IS ?3 AND control_bytes = ?4 FROM domain_requests WHERE session_id = ?1 AND request_id = ?2",
                params![session.as_str(), request.as_str(), source_turn(commit), i64::try_from(record.encoded_len()).unwrap()],
                |row| row.get::<_, bool>(0)).map_err(sql_error)?;
            if !valid {
                return Err(StoreError::Corrupt(
                    "domain usage index differs from canonical control".into(),
                ));
            }
            self.requests += 1;
        }
        self.heads = next;
        Ok(())
    }
    pub(super) fn finish(self, connection: &Connection, session: &SessionId) -> Result<()> {
        let (versions, requests) = connection
            .query_row(
                "SELECT
            (SELECT COUNT(*) FROM domain_versions WHERE session_id = ?1),
            (SELECT COUNT(*) FROM domain_requests WHERE session_id = ?1)",
                [session.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(sql_error)?;
        if self.heads != read_heads(connection, session)?
            || self.versions != decode_u64("domain version count", versions)?
            || self.requests != decode_u64("domain request count", requests)?
        {
            return Err(StoreError::Corrupt(
                "domain index membership differs from canonical controls".into(),
            ));
        }
        Ok(())
    }
}
