use crate::{
    Capabilities, Completion, ConversationId, Error, Limits, Page, Record, RecordKind, Result,
    Snapshot, Status,
    types::{self, MAX_SESSIONS, METADATA_BYTES, PAGE_BYTES},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::Path;

const ID: u32 = 0x5253_4143;
const SCHEMA: &str = "CREATE TABLE sessions (id TEXT PRIMARY KEY, metadata BLOB NOT NULL CHECK(length(metadata)=16384), generation INTEGER NOT NULL CHECK(generation>=0), visible_epoch INTEGER NOT NULL CHECK(visible_epoch>0), write_epoch INTEGER NOT NULL CHECK(write_epoch>0), epoch_counter INTEGER NOT NULL CHECK(epoch_counter>=visible_epoch AND epoch_counter>=write_epoch), sequence INTEGER NOT NULL CHECK(sequence>=0), bytes INTEGER NOT NULL CHECK(bytes>=16384)) STRICT;
CREATE TABLE observations (session TEXT NOT NULL REFERENCES sessions(id), seq INTEGER NOT NULL CHECK(seq>0), epoch INTEGER NOT NULL CHECK(epoch>0), kind TEXT NOT NULL CHECK(kind IN ('user','update','permission')), payload BLOB NOT NULL CHECK(length(payload)<=1048576), PRIMARY KEY(session,seq)) STRICT;
CREATE INDEX observations_epoch ON observations(session,epoch,seq);";

pub(super) fn sql(error: &rusqlite::Error) -> Error {
    match error {
        rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DiskFull => {
            Error::Quota
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            Error::Corrupt
        }
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..) => Error::Corrupt,
        _ => Error::Io,
    }
}

pub(super) fn open(path: &Path, limits: Limits) -> Result<Connection> {
    let empty = std::fs::metadata(path).map_err(|_| Error::Io)?.len() == 0;
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| sql(&error))?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sql(&error))?;
    let identity: u32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|error| sql(&error))?;
    if !empty && (version != 1 || identity != ID) {
        return Err(Error::Corrupt);
    }
    if !empty {
        validate_schema(&connection)?;
    }
    connection
        .busy_timeout(std::time::Duration::ZERO)
        .map_err(|error| sql(&error))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=TRUNCATE; PRAGMA synchronous=FULL;",
        )
        .map_err(|error| sql(&error))?;
    // Allow a capped database, its rollback image and journal/page overhead within
    // the owner disk ceiling. Logical quotas reserve terminal metadata separately.
    connection
        .pragma_update(None, "max_page_count", integer(limits.owner_bytes / 12288)?)
        .map_err(|error| sql(&error))?;
    if empty {
        let transaction = connection.transaction().map_err(|error| sql(&error))?;
        transaction
            .execute_batch(SCHEMA)
            .map_err(|error| sql(&error))?;
        transaction
            .pragma_update(None, "application_id", ID)
            .map_err(|error| sql(&error))?;
        transaction
            .pragma_update(None, "user_version", 1)
            .map_err(|error| sql(&error))?;
        transaction.commit().map_err(|error| sql(&error))?;
    }
    validate_schema(&connection)?;
    let ids = {
        let mut statement = connection
            .prepare("SELECT id FROM sessions ORDER BY id LIMIT 4097")
            .map_err(|error| sql(&error))?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| sql(&error))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| sql(&error))?
    };
    if ids.len() > MAX_SESSIONS {
        return Err(Error::Corrupt);
    }
    validate_counters(&connection, limits)?;
    // The exclusive connection owns this derived counter; temporary triggers keep
    // it consistent with committed, rolled-back and failed mutations alike.
    connection.execute_batch("CREATE TEMP TABLE accounting(bytes INTEGER NOT NULL); INSERT INTO accounting SELECT coalesce(sum(bytes),0) FROM sessions; CREATE TEMP TRIGGER account_insert AFTER INSERT ON main.sessions BEGIN UPDATE accounting SET bytes=bytes+new.bytes; END; CREATE TEMP TRIGGER account_update AFTER UPDATE OF bytes ON main.sessions BEGIN UPDATE accounting SET bytes=bytes+new.bytes-old.bytes; END;").map_err(|error| sql(&error))?;
    for id in ids {
        let id = ConversationId::new(id).map_err(|_| Error::Corrupt)?;
        let transaction = connection.transaction().map_err(|error| sql(&error))?;
        discard_replay(&transaction, &id)?;
        let mut snapshot = get(&transaction, &id)?;
        if matches!(
            snapshot.status,
            Status::Starting | Status::Running | Status::Loading | Status::Ready
        ) {
            snapshot.status = Status::Unknown;
            save(&transaction, &snapshot)?;
            transaction
                .execute(
                    "UPDATE sessions SET write_epoch=visible_epoch WHERE id=?",
                    [id.as_str()],
                )
                .map_err(|error| sql(&error))?;
        }
        transaction.commit().map_err(|error| sql(&error))?;
    }
    Ok(connection)
}

fn validate_counters(connection: &Connection, limits: Limits) -> Result<()> {
    let mut statement = connection.prepare("SELECT s.sequence,s.bytes,s.epoch_counter,count(o.seq),coalesce(max(o.seq),0),16384+coalesce(sum(length(o.payload)+128),0),coalesce(max(o.epoch),1),coalesce(max(length(o.payload)),0) FROM sessions s LEFT JOIN observations o ON o.session=s.id GROUP BY s.id LIMIT 4097").map_err(|error| sql(&error))?;
    let mut rows = statement.query([]).map_err(|error| sql(&error))?;
    let mut total = 0_usize;
    while let Some(row) = rows.next().map_err(|error| sql(&error))? {
        let sequence = unsigned(row, 0).map_err(|error| sql(&error))?;
        let retained = size(row, 1).map_err(|error| sql(&error))?;
        if sequence < unsigned(row, 3).map_err(|error| sql(&error))?
            || sequence < unsigned(row, 4).map_err(|error| sql(&error))?
            || retained != size(row, 5).map_err(|error| sql(&error))?
            || unsigned(row, 2).map_err(|error| sql(&error))?
                < unsigned(row, 6).map_err(|error| sql(&error))?
            || size(row, 7).map_err(|error| sql(&error))? > rsi_acp_protocol::MAX_FRAME_BYTES
        {
            return Err(Error::Corrupt);
        }
        total = total.checked_add(retained).ok_or(Error::Corrupt)?;
        if retained > limits.session_bytes || total > limits.owner_bytes {
            return Err(Error::Quota);
        }
    }
    let orphan: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM observations o LEFT JOIN sessions s ON s.id=o.session WHERE s.id IS NULL)", [], |row| row.get(0)).map_err(|error| sql(&error))?;
    if orphan {
        return Err(Error::Corrupt);
    }
    Ok(())
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let expected = Connection::open_in_memory().map_err(|error| sql(&error))?;
    expected
        .execute_batch(SCHEMA)
        .map_err(|error| sql(&error))?;
    let schema = |connection: &Connection| -> Result<Vec<(String, String)>> {
        let mut statement = connection
            .prepare(
                "SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name LIMIT 5",
            )
            .map_err(|error| sql(&error))?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|error| sql(&error))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|error| sql(&error))
    };
    if schema(connection)? != schema(&expected)? {
        return Err(Error::Corrupt);
    }
    Ok(())
}

fn metadata(snapshot: &Snapshot) -> Result<Vec<u8>> {
    snapshot.validate()?;
    let mut bytes = types::encode(snapshot, METADATA_BYTES)?;
    bytes.resize(METADATA_BYTES, b' ');
    Ok(bytes)
}
fn save(connection: &Connection, snapshot: &Snapshot) -> Result<()> {
    connection
        .execute(
            "UPDATE sessions SET metadata=? WHERE id=?",
            params![metadata(snapshot)?, snapshot.id.as_str()],
        )
        .map_err(|error| sql(&error))?;
    Ok(())
}
pub(super) fn get(connection: &Connection, id: &ConversationId) -> Result<Snapshot> {
    let value: Option<(usize, Vec<u8>, u64, u64)> = connection.query_row("SELECT length(metadata),substr(metadata,1,16384),generation,visible_epoch FROM sessions WHERE id=?", [id.as_str()], |row| Ok((size(row, 0)?, row.get(1)?, unsigned(row, 2)?, unsigned(row, 3)?))).optional().map_err(|error| sql(&error))?;
    let (length, bytes, generation, epoch) = value.ok_or(Error::NotFound)?;
    if length != METADATA_BYTES {
        return Err(Error::Corrupt);
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
    snapshot.validate().map_err(|_| Error::Corrupt)?;
    if snapshot.id != *id || snapshot.generation != generation || snapshot.epoch != epoch {
        return Err(Error::Corrupt);
    }
    Ok(snapshot)
}
fn current(connection: &Connection, id: &ConversationId, generation: u64) -> Result<Snapshot> {
    let snapshot = get(connection, id)?;
    if generation == 0 || snapshot.generation != generation {
        return Err(Error::Stale);
    }
    Ok(snapshot)
}
pub(super) fn create(
    connection: &mut Connection,
    limits: Limits,
    id: ConversationId,
    endpoint: String,
    cwd: String,
) -> Result<Snapshot> {
    let snapshot = Snapshot {
        id,
        endpoint,
        cwd,
        remote: None,
        generation: 0,
        epoch: 1,
        status: Status::Starting,
        completion: None,
        capabilities: Capabilities::default(),
    };
    let bytes = metadata(&snapshot)?;
    let transaction = connection.transaction().map_err(|error| sql(&error))?;
    let (count, retained): (usize, usize) = transaction
        .query_row(
            "SELECT count(*),coalesce(sum(bytes),0) FROM sessions",
            [],
            |row| Ok((size(row, 0)?, size(row, 1)?)),
        )
        .map_err(|error| sql(&error))?;
    if count >= MAX_SESSIONS
        || retained
            .checked_add(METADATA_BYTES)
            .is_none_or(|bytes| bytes > limits.owner_bytes)
    {
        return Err(Error::Quota);
    }
    if transaction
        .query_row(
            "SELECT 1 FROM sessions WHERE id=?",
            [snapshot.id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| sql(&error))?
        .is_some()
    {
        return Err(Error::Input);
    }
    transaction
        .execute(
            "INSERT INTO sessions VALUES(?,?,0,1,1,1,0,16384)",
            params![snapshot.id.as_str(), bytes],
        )
        .map_err(|error| sql(&error))?;
    transaction.commit().map_err(|error| sql(&error))?;
    Ok(snapshot)
}
pub(super) fn list(
    connection: &Connection,
    after: Option<&ConversationId>,
) -> Result<Vec<Snapshot>> {
    let mut statement = connection
        .prepare("SELECT id FROM sessions WHERE id>? ORDER BY id LIMIT 64")
        .map_err(|error| sql(&error))?;
    let ids = statement
        .query_map([after.map_or("", ConversationId::as_str)], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| sql(&error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| sql(&error))?;
    ids.into_iter()
        .map(|id| {
            get(
                connection,
                &ConversationId::new(id).map_err(|_| Error::Corrupt)?,
            )
        })
        .collect()
}
pub(super) fn connect(connection: &mut Connection, id: &ConversationId) -> Result<Snapshot> {
    let transaction = connection.transaction().map_err(|error| sql(&error))?;
    discard_replay(&transaction, id)?;
    let mut snapshot = get(&transaction, id)?;
    snapshot.generation = snapshot
        .generation
        .checked_add(1)
        .filter(|value| i64::try_from(*value).is_ok())
        .ok_or(Error::Quota)?;
    snapshot.status = Status::Starting;
    snapshot.completion = None;
    transaction
        .execute(
            "UPDATE sessions SET generation=?,write_epoch=visible_epoch WHERE id=?",
            params![integer(snapshot.generation)?, id.as_str()],
        )
        .map_err(|error| sql(&error))?;
    save(&transaction, &snapshot)?;
    transaction.commit().map_err(|error| sql(&error))?;
    Ok(snapshot)
}
pub(super) fn bind(
    connection: &mut Connection,
    id: &ConversationId,
    generation: u64,
    remote: String,
    capabilities: Capabilities,
) -> Result<Snapshot> {
    let transaction = connection.transaction().map_err(|error| sql(&error))?;
    let mut snapshot = current(&transaction, id, generation)?;
    if snapshot
        .remote
        .as_ref()
        .is_some_and(|existing| existing != &remote)
    {
        return Err(Error::Input);
    }
    snapshot.remote = Some(remote);
    snapshot.capabilities = capabilities;
    if snapshot.status == Status::Loading {
        snapshot.epoch = transaction
            .query_row(
                "SELECT write_epoch FROM sessions WHERE id=?",
                [id.as_str()],
                |row| unsigned(row, 0),
            )
            .map_err(|error| sql(&error))?;
        transaction
            .execute(
                "UPDATE sessions SET visible_epoch=write_epoch WHERE id=?",
                [id.as_str()],
            )
            .map_err(|error| sql(&error))?;
    }
    snapshot.status = Status::Ready;
    save(&transaction, &snapshot)?;
    transaction.commit().map_err(|error| sql(&error))?;
    Ok(snapshot)
}
pub(super) fn append(
    transaction: &Connection,
    limits: Limits,
    id: &ConversationId,
    generation: u64,
    kind: RecordKind,
    bytes: &[u8],
) -> Result<u64> {
    let row: Option<(u64, u64, usize, u64)> = transaction
        .query_row(
            "SELECT sequence,write_epoch,bytes,generation FROM sessions WHERE id=?",
            [id.as_str()],
            |row| {
                Ok((
                    unsigned(row, 0)?,
                    unsigned(row, 1)?,
                    size(row, 2)?,
                    unsigned(row, 3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sql(&error))?;
    let (sequence, epoch, retained, actual) = row.ok_or(Error::NotFound)?;
    if generation == 0 || actual != generation {
        return Err(Error::Stale);
    }
    let total: usize = transaction
        .query_row("SELECT bytes FROM accounting", [], |row| size(row, 0))
        .map_err(|error| sql(&error))?;
    // Charge identities and index metadata as well as encoded peer payload.
    let charged = bytes.len().checked_add(128).ok_or(Error::Quota)?;
    if retained
        .checked_add(charged)
        .is_none_or(|value| value > limits.session_bytes)
        || total
            .checked_add(charged)
            .is_none_or(|value| value > limits.owner_bytes)
    {
        return Err(Error::Quota);
    }
    let sequence = sequence
        .checked_add(1)
        .filter(|seq| i64::try_from(*seq).is_ok())
        .ok_or(Error::Quota)?;
    transaction
        .execute(
            "INSERT INTO observations VALUES(?,?,?,?,?)",
            params![
                id.as_str(),
                integer(sequence)?,
                integer(epoch)?,
                kind.name(),
                bytes
            ],
        )
        .map_err(|error| sql(&error))?;
    transaction
        .execute(
            "UPDATE sessions SET sequence=?, bytes=bytes+? WHERE id=?",
            params![integer(sequence)?, integer(charged)?, id.as_str()],
        )
        .map_err(|error| sql(&error))?;
    Ok(sequence)
}
pub(super) fn settle(
    connection: &Connection,
    id: &ConversationId,
    generation: u64,
    status: Status,
) -> Result<Snapshot> {
    let mut snapshot = current(connection, id, generation)?;
    snapshot.status = status;
    if matches!(
        status,
        Status::Running | Status::Starting | Status::Loading | Status::Discarded
    ) {
        snapshot.completion = None;
    }
    save(connection, &snapshot)?;
    Ok(snapshot)
}
pub(super) fn complete(
    connection: &Connection,
    id: &ConversationId,
    generation: u64,
    completion: Completion,
) -> Result<Snapshot> {
    let mut snapshot = current(connection, id, generation)?;
    snapshot.status = if completion == Completion::Cancelled {
        Status::Cancelled
    } else {
        Status::Completed
    };
    snapshot.completion = Some(completion);
    save(connection, &snapshot)?;
    Ok(snapshot)
}

pub(super) fn begin_replay(
    connection: &mut Connection,
    id: &ConversationId,
    generation: u64,
) -> Result<Snapshot> {
    let transaction = connection.transaction().map_err(|error| sql(&error))?;
    let mut snapshot = current(&transaction, id, generation)?;
    if snapshot.status == Status::Loading {
        return Err(Error::Busy);
    }
    discard_replay(&transaction, id)?;
    let epoch: u64 = transaction
        .query_row(
            "SELECT epoch_counter FROM sessions WHERE id=?",
            [id.as_str()],
            |row| unsigned(row, 0),
        )
        .map_err(|error| sql(&error))?;
    let next = epoch
        .checked_add(1)
        .filter(|epoch| i64::try_from(*epoch).is_ok())
        .ok_or(Error::Quota)?;
    transaction
        .execute(
            "UPDATE sessions SET write_epoch=?,epoch_counter=? WHERE id=?",
            params![integer(next)?, integer(next)?, id.as_str()],
        )
        .map_err(|error| sql(&error))?;
    snapshot.status = Status::Loading;
    save(&transaction, &snapshot)?;
    transaction.commit().map_err(|error| sql(&error))?;
    Ok(snapshot)
}
fn discard_replay(connection: &Connection, id: &ConversationId) -> Result<()> {
    let charged: usize = connection.query_row(
        "SELECT coalesce(sum(length(payload)+128),0) FROM observations WHERE session=?1 AND epoch=(SELECT write_epoch FROM sessions WHERE id=?1 AND write_epoch!=visible_epoch)",
        [id.as_str()], |row| size(row, 0)).map_err(|error| sql(&error))?;
    connection.execute("DELETE FROM observations WHERE session=?1 AND epoch=(SELECT write_epoch FROM sessions WHERE id=?1 AND write_epoch!=visible_epoch)", [id.as_str()]).map_err(|error| sql(&error))?;
    connection
        .execute(
            "UPDATE sessions SET bytes=bytes-?,write_epoch=visible_epoch WHERE id=?",
            params![integer(charged)?, id.as_str()],
        )
        .map_err(|error| sql(&error))?;
    Ok(())
}
pub(super) fn finish_replay(
    connection: &mut Connection,
    id: &ConversationId,
    generation: u64,
    complete: bool,
) -> Result<Snapshot> {
    let transaction = connection.transaction().map_err(|error| sql(&error))?;
    let mut snapshot = current(&transaction, id, generation)?;
    if snapshot.status != Status::Loading {
        return Err(Error::Input);
    }
    if complete {
        snapshot.epoch = transaction
            .query_row(
                "SELECT write_epoch FROM sessions WHERE id=?",
                [id.as_str()],
                |row| unsigned(row, 0),
            )
            .map_err(|error| sql(&error))?;
        snapshot.status = Status::Ready;
    } else {
        discard_replay(&transaction, id)?;
        snapshot.status = Status::Unknown;
    }
    transaction
        .execute(
            "UPDATE sessions SET visible_epoch=?,write_epoch=? WHERE id=?",
            params![
                integer(snapshot.epoch)?,
                integer(snapshot.epoch)?,
                id.as_str()
            ],
        )
        .map_err(|error| sql(&error))?;
    save(&transaction, &snapshot)?;
    transaction.commit().map_err(|error| sql(&error))?;
    Ok(snapshot)
}
pub(super) fn page(
    connection: &Connection,
    id: &ConversationId,
    epoch: u64,
    after: u64,
) -> Result<Page> {
    get(connection, id)?;
    if epoch == 0 || epoch > i64::MAX as u64 || after > i64::MAX as u64 {
        return Err(Error::Input);
    }
    let mut statement = connection.prepare("SELECT seq,kind,length(payload) FROM observations WHERE session=? AND epoch=? AND seq>? ORDER BY seq LIMIT 65").map_err(|error| sql(&error))?;
    let mut rows = statement
        .query(params![id.as_str(), integer(epoch)?, integer(after)?])
        .map_err(|error| sql(&error))?;
    let mut records = Vec::new();
    let mut remaining = PAGE_BYTES;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(|error| sql(&error))? {
        if records.len() == 64 {
            has_more = true;
            break;
        }
        let sequence = unsigned(row, 0).map_err(|error| sql(&error))?;
        let kind = RecordKind::parse(&row.get::<_, String>(1).map_err(|error| sql(&error))?)?;
        let bytes = size(row, 2).map_err(|error| sql(&error))?;
        if bytes > rsi_acp_protocol::MAX_FRAME_BYTES {
            return Err(Error::Corrupt);
        }
        let value = if bytes <= remaining {
            let payload: Vec<u8> = connection
                .query_row(
                    "SELECT substr(payload,1,1048576) FROM observations WHERE session=? AND seq=?",
                    params![id.as_str(), integer(sequence)?],
                    |row| row.get(0),
                )
                .map_err(|error| sql(&error))?;
            remaining -= bytes;
            Some(serde_json::from_slice(&payload).map_err(|_| Error::Corrupt)?)
        } else {
            None
        };
        records.push(Record {
            sequence,
            epoch,
            kind,
            bytes,
            value,
        });
    }
    Ok(Page { records, has_more })
}
pub(super) fn position(connection: &Connection, id: &ConversationId, epoch: u64) -> Result<u64> {
    if get(connection, id)?.epoch != epoch {
        return Err(Error::Stale);
    }
    connection
        .query_row(
            "SELECT coalesce(max(seq),0) FROM observations WHERE session=? AND epoch=?",
            params![id.as_str(), integer(epoch)?],
            |row| unsigned(row, 0),
        )
        .map_err(|error| sql(&error))
}
pub(super) fn window(
    connection: &Connection,
    id: &ConversationId,
    epoch: u64,
    sequence: u64,
    start: usize,
) -> Result<Vec<u8>> {
    get(connection, id)?;
    if epoch == 0
        || sequence == 0
        || epoch > i64::MAX as u64
        || sequence > i64::MAX as u64
        || start > rsi_acp_protocol::MAX_FRAME_BYTES
    {
        return Err(Error::Input);
    }
    let (length, bytes): (usize, Vec<u8>) = connection.query_row("SELECT length(payload),substr(payload,?,65536) FROM observations WHERE session=? AND epoch=? AND seq=?", params![integer(start + 1)?, id.as_str(), integer(epoch)?, integer(sequence)?], |row| Ok((size(row, 0)?, row.get(1)?))).optional().map_err(|error| sql(&error))?.ok_or(Error::NotFound)?;
    if length > rsi_acp_protocol::MAX_FRAME_BYTES || start > length {
        return Err(Error::Corrupt);
    }
    Ok(bytes)
}

fn integer(value: impl TryInto<i64>) -> Result<i64> {
    value.try_into().map_err(|_| Error::Input)
}
fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    value.try_into().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
fn size(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<usize> {
    unsigned(row, index)?.try_into().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
