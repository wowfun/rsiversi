//! Canonical domain positions, revision admission and bounded historical reads.

use super::{
    AgentControlRecord, AgentControlRecordBody, Connection, OptionalExtension, Result, SessionId,
    StoreError, decode_projected_json, decode_u64, params, sql_error, sqlite_u64,
};
use rsi_agent_session_protocol::{
    DomainIdentity, DomainRequestId, DomainRevision, DomainStateCommit,
    MAXIMUM_DOMAIN_CONTROL_BYTES, MAXIMUM_SESSION_DOMAINS,
};
use rsi_agent_store_protocol::{
    StoreDomainHead, StoreDomainState, StoreDomainStatePage, domain_heads_after,
};
use std::collections::{BTreeMap, BTreeSet};

type HeadRow = (Option<String>, i64, i64, i64, i64, i64);

fn head_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeadRow> {
    Ok((
        super::optional_text(row, 0, 256)?,
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
            NAMED_HEAD_SQL,
            params![session.as_str(), id, sqlite_u64("domain horizon", horizon)?],
            head_row,
        )
        .optional()
        .map_err(sql_error)?
        .map(decode_head)
        .transpose()
}

const NAMED_HEAD_SQL: &str = "SELECT domain_id, control_seq, update_index, codec_version, revision, snapshot_bytes
FROM (SELECT * FROM domain_versions WHERE session_id = ?1 AND domain_id = ?2 AND control_seq <= ?3 ORDER BY control_seq DESC LIMIT 1) d
WHERE NOT EXISTS(SELECT 1 FROM domain_removals r WHERE r.session_id=d.session_id AND r.domain_id=d.domain_id AND r.control_seq > d.control_seq AND r.control_seq <= ?3)";

fn generation_position(
    connection: &Connection,
    session: &SessionId,
    horizon: u64,
) -> Result<Option<u64>> {
    connection.query_row(
        "SELECT control_seq FROM generation_selections WHERE session_id = ?1 AND control_seq <= ?2 ORDER BY control_seq DESC LIMIT 1",
        params![session.as_str(), sqlite_u64("generation horizon", horizon)?],
        |row| row.get::<_, i64>(0),
    ).optional().map_err(sql_error)?.map(|seq| decode_u64("generation position", seq)).transpose()
}

// CROSS JOIN keeps distinct names outermost: each version lookup is an exact
// primary-key seek instead of scanning revisions to match the recursive result.
const HISTORICAL_HEADS_SQL: &str = "WITH RECURSIVE names(domain_id) AS (
    SELECT MIN(domain_id) FROM domain_versions WHERE session_id = ?1
    UNION ALL
    SELECT (SELECT MIN(domain_id) FROM domain_versions WHERE session_id = ?1 AND domain_id > names.domain_id)
    FROM names WHERE domain_id IS NOT NULL
)
SELECT d.domain_id, d.control_seq, d.update_index, d.codec_version, d.revision, d.snapshot_bytes
FROM names CROSS JOIN domain_versions d ON d.session_id = ?1 AND d.domain_id = names.domain_id
AND d.control_seq = (SELECT MAX(v.control_seq) FROM domain_versions v WHERE v.session_id = ?1 AND v.domain_id = names.domain_id AND v.control_seq <= ?2)
WHERE NOT EXISTS(SELECT 1 FROM domain_removals r WHERE r.session_id = d.session_id AND r.domain_id = d.domain_id AND r.control_seq > d.control_seq AND r.control_seq <= ?2)
ORDER BY d.domain_id LIMIT 65";

#[test]
fn historical_membership_seeks_names_and_cuts_without_scanning_revisions() {
    let connection = Connection::open_in_memory().unwrap();
    // Query-work fixture contains only derived metadata, not canonical controls.
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    for (_, sql) in crate::EXPECTED_TABLES
        .iter()
        .filter(|(name, _)| matches!(*name, "domain_versions" | "domain_removals"))
    {
        connection.execute_batch(sql).unwrap();
    }
    for (_, sql) in crate::EXPECTED_INDEXES.iter().filter(|(name, _)| {
        matches!(
            *name,
            "domain_versions_by_control" | "domain_removals_by_control"
        )
    }) {
        connection.execute_batch(sql).unwrap();
    }
    for revisions in [128, 8192] {
        connection
            .execute("DELETE FROM domain_versions", [])
            .unwrap();
        connection
            .execute("DELETE FROM domain_removals", [])
            .unwrap();
        for name in ["a", "b", "c", "d"] {
            connection.execute("WITH RECURSIVE n(seq) AS (VALUES(1) UNION ALL SELECT seq+1 FROM n WHERE seq<?1)
                INSERT INTO domain_versions SELECT 'session',?2,seq,0,1,seq,10 FROM n", params![revisions, name]).unwrap();
        }
        connection
            .execute(
                "INSERT INTO domain_removals (session_id, domain_id, control_seq, codec_version) VALUES ('session', 'b', ?1, 1)",
                [revisions + 1],
            )
            .unwrap();
        for horizon in [revisions / 2, revisions + 1] {
            for name in ["a", "b"] {
                let mut named = connection.prepare(NAMED_HEAD_SQL).unwrap();
                let found = named
                    .query_row(params!["session", name, horizon], |row| {
                        row.get::<_, i64>(1)
                    })
                    .optional()
                    .unwrap();
                assert_eq!(
                    found,
                    if name == "b" && horizon > revisions {
                        None
                    } else {
                        Some(horizon.min(revisions))
                    }
                );
                assert!(
                    named.get_status(rusqlite::StatementStatus::VmStep) < 150,
                    "named lookup scanned revision history"
                );
            }
            let mut statement = connection.prepare(HISTORICAL_HEADS_SQL).unwrap();
            let rows = statement
                .query_map(params!["session", horizon], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let names = if horizon <= revisions {
                vec!["a", "b", "c", "d"]
            } else {
                vec!["a", "c", "d"]
            };
            assert_eq!(
                rows,
                names
                    .into_iter()
                    .map(|name| (name.to_owned(), horizon.min(revisions)))
                    .collect::<Vec<_>>()
            );
            assert!(
                statement.get_status(rusqlite::StatementStatus::VmStep) < 1000,
                "historical membership work grew with unselected revisions"
            );
        }
    }
}

fn historical_heads(
    connection: &Connection,
    session: &SessionId,
    horizon: u64,
) -> Result<Vec<StoreDomainHead>> {
    let mut statement = connection
        .prepare(HISTORICAL_HEADS_SQL)
        .map_err(sql_error)?;
    statement
        .query_map(
            params![session.as_str(), sqlite_u64("domain horizon", horizon)?],
            head_row,
        )
        .map_err(sql_error)?
        .map(|row| decode_head(row.map_err(sql_error)?))
        .collect()
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
    decode_control(seq, bytes)
}

fn decode_control(seq: u64, bytes: (i64, Option<String>)) -> Result<AgentControlRecord> {
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

fn visit_controls(
    connection: &Connection,
    session: &SessionId,
    positions: &BTreeSet<u64>,
    mut visit: impl FnMut(&AgentControlRecord) -> Result<()>,
) -> Result<()> {
    if positions.is_empty() {
        return Ok(());
    }
    // Callers select at most the bounded Domain heads plus one generation record.
    let placeholders = std::iter::repeat_n("?", positions.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT seq, length(CAST(control_json AS BLOB)),
        CASE WHEN length(CAST(control_json AS BLOB)) <= ?1 THEN control_json END
        FROM agent_controls WHERE session_id = ?2 AND seq IN ({placeholders}) ORDER BY seq"
    );
    let mut parameters = vec![
        rusqlite::types::Value::Integer(i64::try_from(MAXIMUM_DOMAIN_CONTROL_BYTES).unwrap()),
        rusqlite::types::Value::Text(session.to_string()),
    ];
    for seq in positions {
        parameters.push(rusqlite::types::Value::Integer(sqlite_u64(
            "domain control sequence",
            *seq,
        )?));
    }
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(parameters))
        .map_err(sql_error)?;
    for seq in positions {
        let row = rows
            .next()
            .map_err(sql_error)?
            .ok_or_else(|| StoreError::Corrupt("domain canonical control is absent".into()))?;
        if decode_u64("domain control sequence", row.get(0).map_err(sql_error)?)? != *seq {
            return Err(StoreError::Corrupt(
                "domain canonical control is absent".into(),
            ));
        }
        let record = decode_control(
            *seq,
            (
                row.get(1).map_err(sql_error)?,
                row.get(2).map_err(sql_error)?,
            ),
        )?;
        visit(&record)?;
    }
    Ok(())
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

pub(super) fn read_watermarks(
    connection: &Connection,
    session: &SessionId,
) -> Result<rsi_agent_store_protocol::StoreDomainWatermarks> {
    watermarks_at(connection, session, None)
}

fn watermarks_at(
    connection: &Connection,
    session: &SessionId,
    horizon: Option<u64>,
) -> Result<rsi_agent_store_protocol::StoreDomainWatermarks> {
    let (fact, control, domain) = connection.query_row("SELECT durable_seq,control_seq,MAX(COALESCE((SELECT MAX(control_seq) FROM domain_versions WHERE session_id=?1 AND control_seq <= COALESCE(?2,9223372036854775807)),0),COALESCE((SELECT MAX(control_seq) FROM domain_removals WHERE session_id=?1 AND control_seq <= COALESCE(?2,9223372036854775807)),0)) FROM sessions WHERE session_id=?1 AND session_id NOT IN (SELECT session_id FROM session_tombstones)", params![session.as_str(), horizon.map(|cut| sqlite_u64("Domain horizon", cut)).transpose()?], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?))).optional().map_err(sql_error)?.ok_or_else(|| StoreError::NotFound(session.to_string()))?;
    Ok(rsi_agent_store_protocol::StoreDomainWatermarks {
        fact_seq: decode_u64("Fact cut", fact)?,
        control_seq: decode_u64("control cut", control)?,
        domain_seq: decode_u64("Domain cut", domain)?,
    })
}

pub(super) fn read_states(
    connection: &Connection,
    session: &SessionId,
    horizon: Option<u64>,
    domain: Option<&str>,
) -> Result<StoreDomainStatePage> {
    let watermarks = watermarks_at(connection, session, horizon)?;
    let durable_control_seq = watermarks.control_seq;
    let selected_control_seq = horizon.unwrap_or(durable_control_seq);
    if selected_control_seq > durable_control_seq {
        return Err(StoreError::Invalid(
            "domain horizon exceeds current control tail".into(),
        ));
    }
    let domain_control_seq = watermarks.domain_seq;
    let mut states = Vec::new();
    let generation = if domain.is_none() {
        generation_position(connection, session, selected_control_seq)?
    } else {
        None
    };
    let heads = match domain {
        Some(id) => read_version(connection, session, id, selected_control_seq)?
            .into_iter()
            .collect(),
        None if selected_control_seq == durable_control_seq => read_heads(connection, session)?,
        None => historical_heads(connection, session, selected_control_seq)?,
    };
    if heads.len() > MAXIMUM_SESSION_DOMAINS {
        return Err(StoreError::Corrupt(
            "domain head count exceeds its bound".into(),
        ));
    }
    let mut grouped = BTreeMap::<_, Vec<_>>::new();
    for head in heads {
        grouped.entry(head.control_seq).or_default().push(head);
    }
    let positions = grouped.keys().copied().chain(generation).collect();
    let mut selection = None;
    visit_controls(connection, session, &positions, |record| {
        if Some(record.seq()) == generation {
            selection =
                Some(rsi_agent_store_protocol::StoreGenerationSelection::from_record(record)?);
        }
        for head in grouped.remove(&record.seq()).unwrap_or_default() {
            let snapshot = head.snapshot_from(record)?.clone();
            states.push(StoreDomainState { head, snapshot });
        }
        Ok(())
    })?;
    states.sort_unstable_by(|left, right| left.head.identity.id().cmp(right.head.identity.id()));
    let page = StoreDomainStatePage {
        durable_fact_seq: watermarks.fact_seq,
        durable_control_seq,
        selected_control_seq,
        domain_control_seq,
        generation: selection,
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
        FROM (SELECT * FROM sessions WHERE session_id NOT IN (SELECT session_id FROM session_tombstones)) AS sessions WHERE session_id = ?1",
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
    commit
        .source()
        .turn_id()
        .map(rsi_agent_session_protocol::TurnId::as_str)
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
    if let Some(turn_id) = commit.source().turn_id() {
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
    let heads = read_heads(connection, session)?;
    let next = domain_heads_after(&heads, record.seq(), commit)?;
    if commit.source().is_generation() {
        connection
            .execute(
                "INSERT INTO generation_selections(session_id, control_seq) VALUES (?1, ?2)",
                params![
                    session.as_str(),
                    sqlite_u64("generation position", record.seq())?
                ],
            )
            .map_err(sql_error)?;
    }
    for removed in commit.removed() {
        connection
            .execute(
                "DELETE FROM domain_heads WHERE session_id=?1 AND domain_id=?2",
                params![session.as_str(), removed.id()],
            )
            .map_err(sql_error)?;
        connection.execute("INSERT INTO domain_removals(session_id,domain_id,control_seq,codec_version) VALUES(?1,?2,?3,?4)", params![session.as_str(), removed.id(), sqlite_u64("removal cut", record.seq())?, removed.version()]).map_err(sql_error)?;
    }
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
    generations: u64,
    removals: u64,
}

impl Projection {
    #[expect(
        clippy::too_many_lines,
        reason = "One canonical control validates its batched versions, removals and selection before advancing the projection"
    )]
    pub(super) fn apply(
        &mut self,
        connection: &Connection,
        session: &SessionId,
        record: &AgentControlRecord,
    ) -> Result<()> {
        let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
            return Ok(());
        };
        if let Some(turn_id) = commit.source().turn_id() {
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
        if commit.source().is_generation() {
            if generation_position(connection, session, record.seq())? != Some(record.seq()) {
                return Err(StoreError::Corrupt(
                    "generation index differs from canonical control".into(),
                ));
            }
            self.generations += 1;
        }
        let expected: Vec<_> = next
            .iter()
            .filter(|head| head.control_seq == record.seq())
            .cloned()
            .collect();
        let mut statement = connection.prepare("SELECT domain_id,control_seq,update_index,codec_version,revision,snapshot_bytes FROM domain_versions WHERE session_id=?1 AND control_seq=?2 ORDER BY domain_id LIMIT 65").map_err(sql_error)?;
        let indexed = statement
            .query_map(
                params![session.as_str(), sqlite_u64("domain cut", record.seq())?],
                head_row,
            )
            .map_err(sql_error)?
            .map(|row| decode_head(row.map_err(sql_error)?))
            .collect::<Result<Vec<_>>>()?;
        if indexed != expected {
            return Err(StoreError::Corrupt(
                "domain version index differs from canonical control".into(),
            ));
        }
        self.versions += expected.len() as u64;
        let mut statement = connection.prepare("SELECT domain_id,codec_version FROM domain_removals WHERE session_id=?1 AND control_seq=?2 ORDER BY domain_id LIMIT 65").map_err(sql_error)?;
        let removed = statement
            .query_map(
                params![session.as_str(), sqlite_u64("removal cut", record.seq())?],
                |row| Ok((super::optional_text(row, 0, 256)?, row.get::<_, u32>(1)?)),
            )
            .map_err(sql_error)?
            .map(|row| {
                let (id, version) = row.map_err(sql_error)?;
                DomainIdentity::new(
                    id.ok_or_else(|| StoreError::Corrupt("invalid removal identity".into()))?,
                    version,
                )
                .map_err(|error| StoreError::Corrupt(error.to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        if removed != commit.removed() {
            return Err(StoreError::Corrupt(
                "domain removal index differs from canonical control".into(),
            ));
        }
        self.removals += removed.len() as u64;
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
        let (versions, requests, generations, removals) = connection
            .query_row(
                "SELECT
            (SELECT COUNT(*) FROM domain_versions WHERE session_id = ?1),
            (SELECT COUNT(*) FROM domain_requests WHERE session_id = ?1),
            (SELECT COUNT(*) FROM generation_selections WHERE session_id = ?1),
            (SELECT COUNT(*) FROM domain_removals WHERE session_id = ?1)",
                [session.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(sql_error)?;
        if self.heads != read_heads(connection, session)?
            || self.versions != decode_u64("domain version count", versions)?
            || self.requests != decode_u64("domain request count", requests)?
            || self.generations != decode_u64("generation count", generations)?
            || self.removals != decode_u64("removal count", removals)?
        {
            return Err(StoreError::Corrupt(
                "domain index membership differs from canonical controls".into(),
            ));
        }
        Ok(())
    }
}
