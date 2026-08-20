use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::domain::{
    AiOperationId, BoundaryOutcome, EventSeq, RunRecord, RunStatus, SessionId, StepId, ToolOutcome,
    Transcript, TranscriptEvent, TranscriptEventKind,
};
use crate::error::{StoreErrorClass, corrupt};
use crate::{AgentError, Result};
use crate::{AgentWorkspace, workspace::WorkspaceLease};

const STORE_SCHEMA_VERSION: u32 = 4;
const SESSION_PAGE_SIZE: usize = 128;
const MAX_DATABASE_BYTES: usize = 512 * 1024 * 1024;
const MAX_EVENT_PAYLOAD_BYTES: usize = 2 * rsi_agent_protocol::MAX_DATA_BYTES + 64 * 1024;
const MAX_SESSION_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_SESSION_ID_BYTES: usize = rsi_agent_protocol::MAX_ID_BYTES;
const MAX_SCHEMA_VERSION_BYTES: usize = 10;
const MAX_STORE_META_KEY_BYTES: usize = 32;
const MAX_SCHEMA_SQL_BYTES: usize = 4 * 1024;
const STORE_QUEUE_CAPACITY: usize = 64;
const MAX_AI_OPERATIONS: usize = 4_096;
const MAX_PREPARED_SNAPSHOT_BYTES: usize = 256 * 1024;
const MAX_AI_OPERATION_TERMINAL_BYTES: usize = 16 * 1024 * 1024;

const STORE_META_SQL: &str =
    "CREATE TABLE store_meta(key TEXT PRIMARY KEY NOT NULL,value TEXT NOT NULL) STRICT";
const SESSIONS_SQL: &str = "CREATE TABLE sessions(
    session_id TEXT PRIMARY KEY NOT NULL,
    prompt TEXT NOT NULL,
    terminal INTEGER NOT NULL CHECK(terminal IN (0,1)),
    next_seq INTEGER NOT NULL CHECK(next_seq >= 1),
    payload_bytes INTEGER NOT NULL CHECK(payload_bytes >= 0 AND payload_bytes <= 67108864)
) STRICT";
const EVENTS_SQL: &str = "CREATE TABLE events(
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK(seq >= 1),
    payload_json TEXT NOT NULL,
    PRIMARY KEY(session_id, seq),
    FOREIGN KEY(session_id) REFERENCES sessions(session_id)
) STRICT";
const OPEN_INDEX_SQL: &str =
    "CREATE INDEX open_sessions_by_id ON sessions(session_id) WHERE terminal=0";
const AI_OPERATIONS_SQL: &str = "CREATE TABLE ai_operations(
    operation_id TEXT PRIMARY KEY NOT NULL,
    prepared_json TEXT NOT NULL
        CHECK(length(CAST(prepared_json AS BLOB)) BETWEEN 2 AND 262144),
    phase INTEGER NOT NULL CHECK(phase IN (0,1,2)),
    terminal_json TEXT
        CHECK(terminal_json IS NULL OR length(CAST(terminal_json AS BLOB)) BETWEEN 2 AND 16777216),
    CHECK((phase=2 AND terminal_json IS NOT NULL) OR (phase IN (0,1) AND terminal_json IS NULL))
) STRICT";
const COUNT_OPEN_AI_OPERATIONS_SQL: &str =
    "SELECT count(*) FROM ai_operations WHERE phase IN (0,1)";
const SCHEMA_OBJECTS: [(&str, &str, &str); 5] = [
    ("store_meta", "table", STORE_META_SQL),
    ("sessions", "table", SESSIONS_SQL),
    ("events", "table", EVENTS_SQL),
    ("ai_operations", "table", AI_OPERATIONS_SQL),
    ("open_sessions_by_id", "index", OPEN_INDEX_SQL),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchemaState {
    Empty,
    Current,
}

#[derive(Debug)]
pub(crate) struct Store {
    connection: Connection,
}

#[cfg(test)]
pub(crate) enum BeginSession {
    Created,
    Existing,
}

pub(crate) enum CreateSession {
    Created {
        cursor: CommitCursor,
        events: Vec<TranscriptEventKind>,
    },
    Exists,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommitCursor {
    pub(crate) next_seq: u64,
    pub(crate) payload_bytes: usize,
}

impl CommitCursor {
    const INITIAL: Self = Self {
        next_seq: 1,
        payload_bytes: 0,
    };

    pub(crate) fn last(self) -> Option<EventSeq> {
        self.next_seq
            .checked_sub(1)
            .filter(|seq| *seq != 0)
            .map(EventSeq::new)
    }
}

pub(crate) enum ProbeSession {
    Missing,
    Open,
    Existing {
        model: String,
        prompt: String,
        record: RunRecord,
    },
}

impl Store {
    #[cfg(test)]
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Self::open_with_identity_check(path, |_| Ok(()))
    }

    fn open_guarded(path: &Path, lease: &WorkspaceLease) -> Result<Self> {
        Self::open_with_identity_check(path, |connection| {
            lease.verify_database(path)?;
            verify_database_handle_not_moved(connection)
        })
    }

    fn open_with_identity_check(
        path: &Path,
        verify_identity: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| AgentError::sqlite("open agent store", error))?;
        verify_identity(&connection)?;

        let max_pages = validate_database_size(&connection)?;
        let schema_state = inspect_schema(&connection)?;
        configure_store(&connection, max_pages)?;
        if schema_state == SchemaState::Empty {
            bootstrap_schema(&connection)?;
        }

        let store = Self { connection };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| AgentError::io("restrict agent store permissions", error))?;
        }
        Ok(store)
    }

    #[cfg(test)]
    fn open_read_only(path: &Path) -> Result<Self> {
        Self::open_read_only_with_identity_check(path, |_| Ok(()))
    }

    fn open_read_only_guarded(path: &Path, lease: &WorkspaceLease) -> Result<Self> {
        Self::open_read_only_with_identity_check(path, |connection| {
            lease.verify_database(path)?;
            verify_database_handle_not_moved(connection)
        })
    }

    fn open_read_only_with_identity_check(
        path: &Path,
        verify_identity: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| AgentError::sqlite_read("open agent transcript reader", error))?;
        verify_identity(&connection)?;
        validate_database_size(&connection)?;
        if inspect_schema(&connection)? != SchemaState::Current {
            return Err(AgentError::CorruptStore {
                message: "agent transcript reader found an empty store".to_owned(),
            });
        }
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| AgentError::sqlite_read("set reader busy timeout", error))?;
        Ok(Self { connection })
    }
}

fn inspect_schema(connection: &Connection) -> Result<SchemaState> {
    let has_meta: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='store_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| AgentError::sqlite_read("inspect agent store schema", error))?;
    if !has_meta {
        let has_user_object = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE sql IS NOT NULL)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| AgentError::sqlite_read("inspect unversioned store objects", error))?;
        if !has_user_object {
            return Ok(SchemaState::Empty);
        }
        return Err(AgentError::CorruptStore {
            message: "non-empty SQLite database has no rsi-agent schema version".to_owned(),
        });
    }

    let version = read_bounded_store_version(connection)?;
    if version != STORE_SCHEMA_VERSION {
        return Err(AgentError::UnsupportedStoreVersion {
            found: version,
            expected: STORE_SCHEMA_VERSION,
        });
    }
    validate_current_schema(connection)?;
    Ok(SchemaState::Current)
}

fn read_bounded_store_version(connection: &Connection) -> Result<u32> {
    let mut statement = connection
        .prepare(
            "SELECT length(CAST(key AS BLOB)),
                    CASE WHEN length(CAST(key AS BLOB)) <= ?1 THEN key END,
                    length(CAST(value AS BLOB)),
                    CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value END
             FROM store_meta LIMIT 2",
        )
        .map_err(|error| AgentError::sqlite_read("prepare bounded store metadata", error))?;
    let mut rows = statement
        .query(params![
            i64::try_from(MAX_STORE_META_KEY_BYTES).expect("metadata key bound fits i64"),
            i64::try_from(MAX_SCHEMA_VERSION_BYTES).expect("version bound fits i64")
        ])
        .map_err(|error| AgentError::sqlite_read("read bounded store metadata", error))?;
    let first = rows
        .next()
        .map_err(|error| AgentError::sqlite_read("read bounded store metadata", error))?
        .ok_or_else(|| corrupt("store_meta has no schema_version"))?;
    let metadata = (
        first
            .get::<_, i64>(0)
            .map_err(|error| AgentError::sqlite_read("read store metadata key length", error))?,
        first
            .get::<_, Option<String>>(1)
            .map_err(|error| AgentError::sqlite_read("read bounded store metadata key", error))?,
        first
            .get::<_, i64>(2)
            .map_err(|error| AgentError::sqlite_read("read store version length", error))?,
        first
            .get::<_, Option<String>>(3)
            .map_err(|error| AgentError::sqlite_read("read bounded store version", error))?,
    );
    if rows
        .next()
        .map_err(|error| AgentError::sqlite_read("read bounded store metadata", error))?
        .is_some()
    {
        return Err(corrupt("store_meta contains unexpected keys"));
    }
    let (key_length, key, version_length, version) = metadata;
    let key = decode_bounded_stored_text(
        "store metadata key",
        key_length,
        key,
        MAX_STORE_META_KEY_BYTES,
    )?;
    if key != "schema_version" {
        return Err(corrupt("store_meta has no schema_version"));
    }
    decode_bounded_stored_text(
        "schema_version",
        version_length,
        version,
        MAX_SCHEMA_VERSION_BYTES,
    )?
    .parse::<u32>()
    .map_err(|_| AgentError::CorruptStore {
        message: "schema_version is not a u32".to_owned(),
    })
}

fn validate_current_schema(connection: &Connection) -> Result<()> {
    let object_count = connection
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT 1 FROM sqlite_schema WHERE sql IS NOT NULL LIMIT ?1
             )",
            [i64::try_from(SCHEMA_OBJECTS.len() + 1).expect("schema object count fits i64")],
            |row| row.get::<_, usize>(0),
        )
        .map_err(|error| AgentError::sqlite_read("count store schema objects", error))?;
    if object_count != SCHEMA_OBJECTS.len() {
        return Err(corrupt("store schema has unexpected objects"));
    }
    for (name, object_type, expected_sql) in SCHEMA_OBJECTS {
        let row = connection
            .query_row(
                "SELECT type,
                        length(CAST(sql AS BLOB)),
                        CASE WHEN length(CAST(sql AS BLOB)) <= ?2 THEN sql END
                 FROM sqlite_schema WHERE name=?1",
                params![
                    name,
                    i64::try_from(MAX_SCHEMA_SQL_BYTES).expect("schema SQL bound fits i64")
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| AgentError::sqlite_read("inspect bounded store schema object", error))?
            .ok_or_else(|| corrupt(format!("store schema object {name} is missing")))?;
        let sql = decode_bounded_stored_text("schema SQL", row.1, row.2, MAX_SCHEMA_SQL_BYTES)?;
        if row.0 != object_type || normalize_schema_sql(&sql) != normalize_schema_sql(expected_sql)
        {
            return Err(corrupt(format!(
                "store schema object {name} does not match version 4"
            )));
        }
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn configure_store(connection: &Connection, max_pages: u64) -> Result<()> {
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| AgentError::sqlite("enable foreign keys", error))?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| AgentError::sqlite("enable WAL", error))?;
    let journal_mode = connection
        .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
        .map_err(|error| AgentError::sqlite("read configured journal mode", error))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(AgentError::Persistence {
            operation: "verify WAL journal mode",
            message: format!("SQLite retained journal mode {journal_mode}"),
        });
    }
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| AgentError::sqlite("set synchronous mode", error))?;
    connection
        .pragma_update(None, "journal_size_limit", 16 * 1024 * 1024)
        .map_err(|error| AgentError::sqlite("set journal size limit", error))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| AgentError::sqlite("set store busy timeout", error))?;
    connection
        .pragma_update(None, "max_page_count", max_pages)
        .map_err(|error| AgentError::sqlite("set store page limit", error))?;
    let applied_max_pages = connection
        .pragma_query_value(None, "max_page_count", |row| row.get::<_, u64>(0))
        .map_err(|error| AgentError::sqlite("verify store page limit", error))?;
    if applied_max_pages != max_pages {
        return Err(AgentError::Persistence {
            operation: "verify store size limit",
            message: format!(
                "SQLite retained a {applied_max_pages}-page limit instead of {max_pages}"
            ),
        });
    }
    Ok(())
}

fn validate_database_size(connection: &Connection) -> Result<u64> {
    let page_size = connection
        .pragma_query_value(None, "page_size", |row| row.get::<_, u64>(0))
        .map_err(|error| AgentError::sqlite_read("read store page size", error))?;
    let max_pages = u64::try_from(MAX_DATABASE_BYTES).expect("database limit fits u64") / page_size;
    let page_count = connection
        .pragma_query_value(None, "page_count", |row| row.get::<_, u64>(0))
        .map_err(|error| AgentError::sqlite_read("read store page count", error))?;
    if max_pages == 0 || page_count > max_pages {
        return Err(AgentError::Persistence {
            operation: "validate store size",
            message: format!(
                "database uses {page_count} pages of {page_size} bytes; maximum is {MAX_DATABASE_BYTES} bytes"
            ),
        });
    }

    Ok(max_pages)
}

fn bootstrap_schema(connection: &Connection) -> Result<()> {
    let schema = format!(
        "BEGIN IMMEDIATE;
         {STORE_META_SQL};
         INSERT INTO store_meta(key, value) VALUES ('schema_version', '{STORE_SCHEMA_VERSION}');
         {SESSIONS_SQL};
         {OPEN_INDEX_SQL};
         {EVENTS_SQL};
         {AI_OPERATIONS_SQL};
         COMMIT;"
    );
    connection
        .execute_batch(&schema)
        .map_err(|error| AgentError::sqlite("create agent store schema", error))?;
    Ok(())
}

fn decode_bounded_stored_text<T: Into<Vec<u8>>>(
    field: &str,
    raw_bytes: i64,
    value: Option<T>,
    maximum: usize,
) -> Result<String> {
    let actual = usize::try_from(raw_bytes).map_err(|_| AgentError::CorruptStore {
        message: format!("stored {field} has an invalid byte length"),
    })?;
    if actual > maximum {
        return Err(AgentError::CorruptStore {
            message: format!("stored {field} is {actual} bytes; maximum is {maximum}"),
        });
    }
    let value: Vec<u8> = value
        .ok_or_else(|| AgentError::CorruptStore {
            message: format!("stored {field} could not be read within its byte limit"),
        })?
        .into();
    if value.len() != actual {
        return Err(AgentError::CorruptStore {
            message: format!("stored {field} changed while it was read"),
        });
    }
    String::from_utf8(value).map_err(|_| AgentError::CorruptStore {
        message: format!("stored {field} is not valid UTF-8"),
    })
}

#[derive(Debug)]
struct StoredSession {
    prompt: String,
    terminal: bool,
    next_seq: i64,
    payload_bytes: i64,
}

fn read_session(connection: &Connection, session_id: &SessionId) -> Result<Option<StoredSession>> {
    let row = connection
        .query_row(
            "SELECT length(CAST(prompt AS BLOB)),
                    CASE WHEN length(CAST(prompt AS BLOB)) <= ?2
                         THEN CAST(prompt AS BLOB) END,
                    terminal,
                    next_seq,
                    payload_bytes
             FROM sessions WHERE session_id=?1",
            params![
                session_id.as_str(),
                i64::try_from(crate::MAX_PROMPT_BYTES).expect("prompt bound fits i64")
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| AgentError::sqlite_read("read session header", error))?;
    let Some((prompt_bytes, prompt, terminal, next_seq, payload_bytes)) = row else {
        return Ok(None);
    };
    let terminal = match terminal {
        0 => false,
        1 => true,
        _ => {
            return Err(AgentError::CorruptStore {
                message: "session terminal flag is not boolean".to_owned(),
            });
        }
    };
    let payload_size = usize::try_from(payload_bytes).map_err(|_| AgentError::CorruptStore {
        message: "session payload_bytes is invalid".to_owned(),
    })?;
    if payload_size > MAX_SESSION_PAYLOAD_BYTES {
        return Err(AgentError::CorruptStore {
            message: format!(
                "session payload_bytes is {payload_size}; maximum is {MAX_SESSION_PAYLOAD_BYTES}"
            ),
        });
    }
    Ok(Some(StoredSession {
        prompt: decode_bounded_stored_text(
            "session prompt",
            prompt_bytes,
            prompt,
            crate::MAX_PROMPT_BYTES,
        )?,
        terminal,
        next_seq,
        payload_bytes,
    }))
}

fn open_session_page(connection: &Connection, after: Option<&SessionId>) -> Result<Vec<SessionId>> {
    let sql = match after {
        None => {
            "SELECT length(CAST(session_id AS BLOB)),
                CASE WHEN length(CAST(session_id AS BLOB)) <= ?2 THEN session_id END
         FROM sessions
         WHERE terminal=0
         ORDER BY session_id LIMIT ?3"
        }
        Some(_) => {
            "SELECT length(CAST(session_id AS BLOB)),
                CASE WHEN length(CAST(session_id AS BLOB)) <= ?2 THEN session_id END
         FROM sessions
         WHERE terminal=0 AND session_id > ?1
         ORDER BY session_id LIMIT ?3"
        }
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| AgentError::sqlite_read("prepare bounded session page", error))?;
    let rows = statement
        .query_map(
            params![
                after.map_or("", SessionId::as_str),
                i64::try_from(MAX_SESSION_ID_BYTES).expect("identifier bound fits i64"),
                i64::try_from(SESSION_PAGE_SIZE).expect("session page bound fits i64")
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .map_err(|error| AgentError::sqlite_read("query bounded session page", error))?;
    rows.map(|row| {
        let (bytes, value) =
            row.map_err(|error| AgentError::sqlite_read("read bounded session identifier", error))?;
        decode_bounded_stored_text("session identifier", bytes, value, MAX_SESSION_ID_BYTES)
            .and_then(SessionId::from_stored)
    })
    .collect()
}

impl Store {
    pub(crate) fn probe_session(&self, session_id: &SessionId) -> Result<ProbeSession> {
        let Some(existing) = read_session(&self.connection, session_id)? else {
            return Ok(ProbeSession::Missing);
        };
        if !existing.terminal {
            self.validate_open_session(session_id, &existing)?;
            return Ok(ProbeSession::Open);
        }
        let prompt = existing.prompt.clone();
        let transcript = self.validated_terminal(session_id, &existing)?;
        let model = transcript
            .events()
            .iter()
            .find_map(|event| match event.kind() {
                TranscriptEventKind::SessionStarted { model, .. } => Some(model.clone()),
                _ => None,
            })
            .ok_or_else(|| AgentError::CorruptSession {
                session_id: session_id.clone(),
                message: "terminal transcript has no session model".to_owned(),
            })?;
        let last = transcript
            .events()
            .last()
            .map(TranscriptEvent::seq)
            .ok_or_else(|| AgentError::CorruptStore {
                message: format!("terminal session {session_id} has no events"),
            })?;
        Ok(ProbeSession::Existing {
            model,
            prompt,
            record: RunRecord::new(session_id.clone(), last, transcript.status().clone()),
        })
    }

    pub(crate) fn recover_open_sessions(&mut self) -> Result<()> {
        let mut after = None;
        loop {
            let page = open_session_page(&self.connection, after.as_ref())?;
            if page.is_empty() {
                break;
            }
            for session_id in &page {
                self.recover_session(session_id)?;
            }
            after = page.last().cloned();
        }
        Ok(())
    }

    pub(crate) fn recover_ai_operations(&mut self) -> Result<()> {
        let total = self
            .connection
            .query_row(COUNT_OPEN_AI_OPERATIONS_SQL, [], |row| row.get::<_, i64>(0))
            .map_err(|error| AgentError::sqlite_read("count AI operations", error))?;
        if usize::try_from(total)
            .ok()
            .is_none_or(|count| count > MAX_AI_OPERATIONS)
        {
            return Err(corrupt("AI operation journal exceeds its row bound"));
        }

        let mut statement = self
            .connection
            .prepare(
                "SELECT length(CAST(operation_id AS BLOB)),
                        CASE WHEN length(CAST(operation_id AS BLOB)) <= ?1 THEN operation_id END,
                        length(CAST(prepared_json AS BLOB)),
                        CASE WHEN length(CAST(prepared_json AS BLOB)) <= ?2
                             THEN CAST(prepared_json AS BLOB) END,
                        phase
                 FROM ai_operations WHERE phase IN (0,1)
                 ORDER BY operation_id LIMIT ?3",
            )
            .map_err(|error| AgentError::sqlite_read("prepare AI operation recovery", error))?;
        let rows = statement
            .query_map(
                params![
                    i64::try_from(MAX_SESSION_ID_BYTES).expect("identifier bound fits i64"),
                    i64::try_from(MAX_PREPARED_SNAPSHOT_BYTES).expect("snapshot bound fits i64"),
                    i64::try_from(MAX_AI_OPERATIONS + 1).expect("operation bound fits i64")
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(|error| AgentError::sqlite_read("query AI operation recovery", error))?;
        let mut open = Vec::new();
        for row in rows {
            let (id_bytes, id, snapshot_bytes, snapshot, phase) =
                row.map_err(|error| AgentError::sqlite_read("read AI operation recovery", error))?;
            let operation_id = AiOperationId::from_stored(decode_bounded_stored_text(
                "AI operation id",
                id_bytes,
                id,
                MAX_SESSION_ID_BYTES,
            )?)?;
            let snapshot = decode_bounded_stored_text(
                "AI prepared snapshot",
                snapshot_bytes,
                snapshot,
                MAX_PREPARED_SNAPSHOT_BYTES,
            )?;
            let snapshot: rsi_ai_meta::PreparedCallSnapshot = serde_json::from_str(&snapshot)
                .map_err(|error| {
                    corrupt(format!(
                        "AI operation {operation_id} has an invalid prepared snapshot: {error}"
                    ))
                })?;
            snapshot.validate().map_err(|error| {
                corrupt(format!(
                    "AI operation {operation_id} has an invalid prepared snapshot: {error}"
                ))
            })?;
            if !matches!(phase, 0 | 1) {
                return Err(corrupt(format!(
                    "AI operation {operation_id} has an invalid open phase"
                )));
            }
            open.push((operation_id, phase));
        }
        drop(statement);

        for (operation_id, phase) in open {
            let terminal = if phase == 0 {
                r#"{"status":"not_started"}"#
            } else {
                r#"{"status":"outcome_unknown"}"#
            };
            let changed = self
                .connection
                .execute(
                    "UPDATE ai_operations SET phase=2,terminal_json=?2
                     WHERE operation_id=?1 AND phase=?3 AND terminal_json IS NULL",
                    params![operation_id.as_str(), terminal, phase],
                )
                .map_err(|error| AgentError::sqlite("recover AI operation", error))?;
            if changed != 1 {
                return Err(corrupt(format!(
                    "AI operation {operation_id} changed during recovery"
                )));
            }
        }
        Ok(())
    }

    fn record_ai_prepared(
        &mut self,
        operation_id: &AiOperationId,
        snapshot: &rsi_ai_meta::PreparedCallSnapshot,
    ) -> Result<()> {
        snapshot.validate().map_err(|error| AgentError::Ai {
            operation: "validate prepared AI operation",
            message: error.to_string(),
        })?;
        let encoded = serde_json::to_string(snapshot).map_err(|error| AgentError::Ai {
            operation: "encode prepared AI operation",
            message: error.to_string(),
        })?;
        if encoded.len() > MAX_PREPARED_SNAPSHOT_BYTES {
            return Err(AgentError::Ai {
                operation: "encode prepared AI operation",
                message: "prepared snapshot exceeds its durable bound".to_owned(),
            });
        }
        self.verify_connection_not_moved()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| AgentError::sqlite("begin prepared AI operation", error))?;
        let total = transaction
            .query_row(COUNT_OPEN_AI_OPERATIONS_SQL, [], |row| row.get::<_, i64>(0))
            .map_err(|error| AgentError::sqlite("count AI operations", error))?;
        if usize::try_from(total)
            .ok()
            .is_none_or(|count| count >= MAX_AI_OPERATIONS)
        {
            return Err(AgentError::Persistence {
                operation: "commit prepared AI operation",
                message: "AI operation journal quota exceeded".to_owned(),
            });
        }
        let inserted = transaction
            .execute(
                "INSERT INTO ai_operations(operation_id,prepared_json,phase,terminal_json)
                 VALUES (?1,?2,0,NULL) ON CONFLICT(operation_id) DO NOTHING",
                params![operation_id.as_str(), encoded],
            )
            .map_err(|error| AgentError::sqlite("insert prepared AI operation", error))?;
        if inserted != 1 {
            return Err(AgentError::AiOperationConflict {
                operation_id: operation_id.clone(),
            });
        }
        verify_database_handle_not_moved(&transaction)?;
        transaction.commit().map_err(|error| {
            AgentError::commit_outcome_unknown("commit prepared AI operation", error)
        })?;
        self.verify_connection_not_moved()
    }

    fn record_ai_started(&mut self, operation_id: &AiOperationId) -> Result<()> {
        self.update_ai_operation(operation_id, 0, 1, None, "commit started AI operation")
    }

    fn record_ai_terminal(
        &mut self,
        operation_id: &AiOperationId,
        terminal: &serde_json::Value,
    ) -> Result<()> {
        let encoded = encode_ai_terminal(terminal)?;
        self.update_ai_operation(
            operation_id,
            1,
            2,
            Some(&encoded),
            "commit terminal AI operation",
        )
    }

    fn update_ai_operation(
        &mut self,
        operation_id: &AiOperationId,
        expected_phase: i64,
        next_phase: i64,
        terminal: Option<&str>,
        operation: &'static str,
    ) -> Result<()> {
        self.verify_connection_not_moved()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| AgentError::sqlite(operation, error))?;
        let changed = transaction
            .execute(
                "UPDATE ai_operations SET phase=?2,terminal_json=?3
                 WHERE operation_id=?1 AND phase=?4 AND terminal_json IS NULL",
                params![operation_id.as_str(), next_phase, terminal, expected_phase],
            )
            .map_err(|error| AgentError::sqlite(operation, error))?;
        if changed != 1 {
            return Err(corrupt(format!(
                "AI operation {operation_id} is not in durable phase {expected_phase}"
            )));
        }
        verify_database_handle_not_moved(&transaction)?;
        transaction
            .commit()
            .map_err(|error| AgentError::commit_outcome_unknown(operation, error))?;
        self.verify_connection_not_moved()
    }

    fn recover_session(&mut self, session_id: &SessionId) -> Result<()> {
        let session = read_session(&self.connection, session_id)?.ok_or_else(|| {
            AgentError::CorruptStore {
                message: format!("open session {session_id} disappeared during recovery"),
            }
        })?;
        let cursor = stored_cursor(session_id, &session)?;
        let loaded = load_events_with_cursor(&self.connection, session_id)?;
        validate_stored_cursor(session_id, cursor, loaded.cursor)?;
        let events = loaded.events;
        let mut machine = crate::transcript::SessionMachine::replay(session.prompt, &events)?;
        let state = machine.recovery_plan()?;

        let mut repairs = Vec::new();
        for (call_id, dispatch_started) in state.unfinished_calls {
            let outcome = if dispatch_started {
                ToolOutcome::OutcomeUnknown
            } else {
                ToolOutcome::NotStarted {
                    reason: "interrupted_before_dispatch".to_owned(),
                }
            };
            repairs.push(TranscriptEventKind::ToolResult { call_id, outcome });
        }
        repairs.push(TranscriptEventKind::StepEnded {
            step: state.open_step,
            outcome: BoundaryOutcome::Interrupted,
        });
        repairs.push(TranscriptEventKind::TurnEnded {
            outcome: BoundaryOutcome::Interrupted,
        });
        machine.apply_batch(EventSeq::new(cursor.next_seq), &repairs)?;
        machine.validate_terminal(&RunStatus::Interrupted)?;
        self.append_terminal_at(session_id, cursor, &repairs, RunStatus::Interrupted)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn begin_session(
        &mut self,
        session_id: &SessionId,
        prompt: &str,
    ) -> Result<BeginSession> {
        if let Some(existing) = read_session(&self.connection, session_id)? {
            if existing.prompt != prompt {
                return Err(AgentError::SessionConflict {
                    session_id: session_id.clone(),
                });
            }
            if !existing.terminal {
                return Err(AgentError::CorruptStore {
                    message: format!("session {session_id} remained open after recovery"),
                });
            }
            self.transcript(session_id)?
                .ok_or_else(|| AgentError::CorruptStore {
                    message: format!("session {session_id} has no terminal transcript"),
                })?;
            return self.record(session_id)?.map_or_else(
                || {
                    Err(AgentError::CorruptStore {
                        message: format!("session {session_id} has no terminal record"),
                    })
                },
                |_record| Ok(BeginSession::Existing),
            );
        }

        match self.create_session(session_id, "default", prompt)? {
            CreateSession::Created { .. } => Ok(BeginSession::Created),
            CreateSession::Exists => Err(AgentError::CorruptStore {
                message: format!("session {session_id} appeared while it was admitted"),
            }),
        }
    }

    pub(crate) fn create_session(
        &mut self,
        session_id: &SessionId,
        model: &str,
        prompt: &str,
    ) -> Result<CreateSession> {
        let prompt_sha256 = crate::digest::sha256_hex(prompt.as_bytes());
        let initial = initial_events(model, prompt, &prompt_sha256);
        self.verify_connection_not_moved()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| AgentError::sqlite("begin session transaction", error))?;
        let inserted = transaction
            .execute(
                "INSERT INTO sessions(session_id,prompt,terminal,next_seq,payload_bytes)
                 VALUES (?1,?2,0,1,0)
                 ON CONFLICT(session_id) DO NOTHING",
                params![session_id.as_str(), prompt],
            )
            .map_err(|error| AgentError::sqlite("insert session", error))?;
        if inserted == 0 {
            return Ok(CreateSession::Exists);
        }
        let cursor = append_events_tx(
            &transaction,
            session_id,
            CommitCursor::INITIAL,
            &initial,
            false,
        )?;
        verify_database_handle_not_moved(&transaction)?;
        transaction
            .commit()
            .map_err(|error| AgentError::commit_outcome_unknown("commit new session", error))?;
        self.verify_connection_not_moved()?;
        Ok(CreateSession::Created {
            cursor,
            events: initial.into_iter().collect(),
        })
    }

    #[cfg(test)]
    pub(crate) fn append(
        &mut self,
        session_id: &SessionId,
        events: &[TranscriptEventKind],
    ) -> Result<EventSeq> {
        self.verify_connection_not_moved()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| AgentError::sqlite("begin append transaction", error))?;
        let expected = read_commit_cursor(&transaction, session_id)?;
        let cursor = append_events_tx(&transaction, session_id, expected, events, false)?;
        verify_database_handle_not_moved(&transaction)?;
        transaction.commit().map_err(|error| {
            AgentError::commit_outcome_unknown("commit transcript events", error)
        })?;
        self.verify_connection_not_moved()?;
        cursor
            .last()
            .ok_or_else(|| corrupt("event append produced no sequence"))
    }

    #[cfg(test)]
    pub(crate) fn append_terminal(
        &mut self,
        session_id: &SessionId,
        events: &[TranscriptEventKind],
        status: RunStatus,
    ) -> Result<RunRecord> {
        let session = read_session(&self.connection, session_id)?.ok_or_else(|| {
            AgentError::CorruptStore {
                message: format!("session {session_id} is missing"),
            }
        })?;
        if session.terminal {
            return Err(corrupt(format!("session {session_id} is already terminal")));
        }
        let cursor = stored_cursor(session_id, &session)?;
        let loaded = load_events_with_cursor(&self.connection, session_id)?;
        validate_stored_cursor(session_id, cursor, loaded.cursor)?;
        let existing = loaded.events;
        let mut machine = crate::transcript::SessionMachine::replay(session.prompt, &existing)?;
        machine.apply_batch(EventSeq::new(cursor.next_seq), events)?;
        machine.validate_terminal(&status)?;
        self.append_terminal_at(session_id, cursor, events, status)
    }

    fn append_terminal_at(
        &mut self,
        session_id: &SessionId,
        expected: CommitCursor,
        events: &[TranscriptEventKind],
        status: RunStatus,
    ) -> Result<RunRecord> {
        self.verify_connection_not_moved()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| AgentError::sqlite("begin terminal transaction", error))?;
        let cursor = append_events_tx(&transaction, session_id, expected, events, true)?;
        verify_database_handle_not_moved(&transaction)?;
        transaction.commit().map_err(|error| {
            AgentError::commit_outcome_unknown("commit terminal session", error)
        })?;
        self.verify_connection_not_moved()?;
        let last = cursor
            .last()
            .ok_or_else(|| corrupt("terminal append produced no sequence"))?;
        Ok(RunRecord::new(session_id.clone(), last, status))
    }

    #[cfg(test)]
    pub(crate) fn record(&self, session_id: &SessionId) -> Result<Option<RunRecord>> {
        let Some(row) = read_session(&self.connection, session_id)? else {
            return Ok(None);
        };
        if !row.terminal {
            return Ok(None);
        }
        let expected = stored_cursor(session_id, &row)?;
        let loaded = load_events_with_cursor(&self.connection, session_id)?;
        validate_stored_cursor(session_id, expected, loaded.cursor)?;
        let events = loaded.events;
        let status = crate::transcript::terminal_status(&events)?;
        crate::transcript::SessionMachine::replay(row.prompt.clone(), &events)?
            .validate_terminal(&status)?;
        let last =
            events
                .last()
                .map(TranscriptEvent::seq)
                .ok_or_else(|| AgentError::CorruptStore {
                    message: format!("terminal session {session_id} has no events"),
                })?;
        Ok(Some(RunRecord::new(session_id.clone(), last, status)))
    }

    pub(crate) fn transcript(&self, session_id: &SessionId) -> Result<Option<Transcript>> {
        let Some(row) = read_session(&self.connection, session_id)? else {
            return Ok(None);
        };
        if !row.terminal {
            self.validate_open_session(session_id, &row)?;
            return Err(AgentError::RecoveryRequired {
                session_id: session_id.clone(),
                message: "session remained durably open without a supervised task".to_owned(),
            });
        }
        self.validated_terminal(session_id, &row).map(Some)
    }

    fn validate_open_session(&self, session_id: &SessionId, row: &StoredSession) -> Result<()> {
        let expected = stored_cursor(session_id, row)?;
        let loaded = load_events_with_cursor(&self.connection, session_id)?;
        validate_stored_cursor(session_id, expected, loaded.cursor)?;
        let events = loaded.events;
        let machine = crate::transcript::SessionMachine::replay(row.prompt.clone(), &events)?;
        if matches!(
            events.last().map(TranscriptEvent::kind),
            Some(TranscriptEventKind::TurnEnded { .. })
        ) {
            let status = crate::transcript::terminal_status(&events)?;
            machine.validate_terminal(&status)?;
            return Err(corrupt(format!(
                "session {session_id} has terminal events but an open header"
            )));
        }
        machine.recovery_plan()?;
        Ok(())
    }

    fn validated_terminal(
        &self,
        session_id: &SessionId,
        row: &StoredSession,
    ) -> Result<Transcript> {
        let expected = stored_cursor(session_id, row)?;
        let loaded = load_events_with_cursor(&self.connection, session_id)?;
        validate_stored_cursor(session_id, expected, loaded.cursor)?;
        let events = loaded.events;
        let status = crate::transcript::terminal_status(&events)?;
        crate::transcript::SessionMachine::replay(row.prompt.clone(), &events)?
            .validate_terminal(&status)?;
        Ok(Transcript::new(session_id.clone(), events, status))
    }

    #[cfg(test)]
    pub(crate) fn load_events(&self, session_id: &SessionId) -> Result<Vec<TranscriptEvent>> {
        load_events_from(&self.connection, session_id)
    }

    fn commit_expected(
        &mut self,
        session_id: &SessionId,
        expected: CommitCursor,
        events: &[TranscriptEventKind],
        terminal: Option<RunStatus>,
    ) -> Result<(CommitCursor, Option<RunRecord>)> {
        self.verify_connection_not_moved()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| AgentError::sqlite("begin writer commit", error))?;
        let cursor = append_events_tx(
            &transaction,
            session_id,
            expected,
            events,
            terminal.is_some(),
        )?;
        verify_database_handle_not_moved(&transaction)?;
        transaction
            .commit()
            .map_err(|error| AgentError::commit_outcome_unknown("commit writer events", error))?;
        self.verify_connection_not_moved()?;
        let record = terminal
            .map(|status| {
                cursor
                    .last()
                    .map(|last| RunRecord::new(session_id.clone(), last, status))
                    .ok_or_else(|| corrupt("terminal commit produced no sequence"))
            })
            .transpose()?;
        Ok((cursor, record))
    }

    fn verify_connection_not_moved(&self) -> Result<()> {
        verify_database_handle_not_moved(&self.connection)
    }

    #[cfg(test)]
    pub(crate) fn make_writes_fail(&self) -> Result<()> {
        self.connection
            .pragma_update(None, "query_only", "ON")
            .map_err(|error| AgentError::sqlite("enable test query-only mode", error))
    }
}

fn encode_ai_terminal(terminal: &serde_json::Value) -> Result<String> {
    let encoded = serde_json::to_string(terminal).map_err(|error| AgentError::Ai {
        operation: "encode terminal AI operation",
        message: error.to_string(),
    })?;
    if encoded.len() < 2 || encoded.len() > MAX_AI_OPERATION_TERMINAL_BYTES {
        return Err(AgentError::Ai {
            operation: "encode terminal AI operation",
            message: "terminal AI operation exceeds its durable bound".to_owned(),
        });
    }
    Ok(encoded)
}

pub(crate) fn preflight_ai_terminal(terminal: &serde_json::Value) -> Result<()> {
    encode_ai_terminal(terminal).map(|_| ())
}

#[allow(unsafe_code)] // Required for SQLite's actual-open-handle identity query.
fn verify_database_handle_not_moved(connection: &Connection) -> Result<()> {
    let mut moved = 0_i32;
    // SAFETY: `connection` owns a live `sqlite3` handle for this call; `main` is
    // a static NUL-terminated database name; `moved` is a valid writable int;
    // and every connection in this module is confined to the current thread,
    // so the raw handle cannot be used concurrently while file_control runs.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            std::ptr::from_mut(&mut moved).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(AgentError::CorruptStore {
            message: format!(
                "SQLite could not verify the open database identity (file-control result {result})"
            ),
        });
    }
    if moved != 0 {
        return Err(AgentError::CorruptStore {
            message: "the open SQLite database file was moved or unlinked".to_owned(),
        });
    }
    Ok(())
}

fn stored_cursor(session_id: &SessionId, session: &StoredSession) -> Result<CommitCursor> {
    let next_seq = u64::try_from(session.next_seq).map_err(|_| AgentError::CorruptStore {
        message: format!("session {session_id} has invalid next_seq"),
    })?;
    let payload_bytes =
        usize::try_from(session.payload_bytes).map_err(|_| AgentError::CorruptStore {
            message: format!("session {session_id} has invalid payload_bytes"),
        })?;
    if next_seq == 0 || payload_bytes > MAX_SESSION_PAYLOAD_BYTES {
        return Err(corrupt(format!(
            "session {session_id} has an invalid commit cursor"
        )));
    }
    Ok(CommitCursor {
        next_seq,
        payload_bytes,
    })
}

#[cfg(test)]
fn load_events_from(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<Vec<TranscriptEvent>> {
    Ok(load_events_with_cursor(connection, session_id)?.events)
}

#[derive(Debug)]
struct LoadedEvents {
    events: Vec<TranscriptEvent>,
    cursor: CommitCursor,
}

fn load_events_with_cursor(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<LoadedEvents> {
    load_events_with_cursor_and_hook(connection, session_id, || {})
}

#[cfg(test)]
fn load_events_from_with_hook(
    connection: &Connection,
    session_id: &SessionId,
    after_first_event: impl FnOnce(),
) -> Result<Vec<TranscriptEvent>> {
    Ok(load_events_with_cursor_and_hook(connection, session_id, after_first_event)?.events)
}

fn load_events_with_cursor_and_hook(
    connection: &Connection,
    session_id: &SessionId,
    after_first_event: impl FnOnce(),
) -> Result<LoadedEvents> {
    if !connection.is_autocommit() {
        return load_events_in_snapshot(connection, session_id, after_first_event);
    }
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| AgentError::sqlite_read("begin transcript read snapshot", error))?;
    let loaded = load_events_in_snapshot(&transaction, session_id, after_first_event)?;
    transaction
        .commit()
        .map_err(|error| AgentError::sqlite_read("finish transcript read snapshot", error))?;
    Ok(loaded)
}

fn load_events_in_snapshot(
    connection: &Connection,
    session_id: &SessionId,
    after_first_event: impl FnOnce(),
) -> Result<LoadedEvents> {
    debug_assert!(!connection.is_autocommit());
    let mut statement = connection
        .prepare(
            "SELECT seq,
                    length(CAST(payload_json AS BLOB)),
                    CASE WHEN length(CAST(payload_json AS BLOB)) <= ?3
                         THEN CAST(payload_json AS BLOB) END
             FROM events WHERE session_id=?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(|error| AgentError::sqlite_read("prepare transcript query", error))?;
    let rows = statement
        .query(params![
            session_id.as_str(),
            i64::try_from(crate::MAX_TRANSCRIPT_EVENTS + 1).expect("event limit fits i64"),
            i64::try_from(MAX_EVENT_PAYLOAD_BYTES).expect("payload limit fits i64")
        ])
        .map_err(|error| AgentError::sqlite_read("query transcript", error))?;
    let mut rows = rows;
    let mut events = Vec::with_capacity(16);
    let mut payload_bytes = 0_usize;
    let mut expected_seq = 1_u64;
    let mut after_first_event = Some(after_first_event);
    while let Some(row) = rows
        .next()
        .map_err(|error| AgentError::sqlite_read("read transcript row", error))?
    {
        if events.len() == crate::MAX_TRANSCRIPT_EVENTS {
            return Err(AgentError::CorruptStore {
                message: format!(
                    "session {session_id} exceeds the {}-event limit",
                    crate::MAX_TRANSCRIPT_EVENTS
                ),
            });
        }
        let raw_seq = row
            .get::<_, i64>(0)
            .map_err(|error| AgentError::sqlite_read("read transcript event sequence", error))?;
        let seq = u64::try_from(raw_seq).map_err(|_| AgentError::CorruptStore {
            message: format!(
                "session {session_id} expected event sequence {expected_seq}, found {raw_seq}"
            ),
        })?;
        if seq != expected_seq {
            return Err(AgentError::CorruptStore {
                message: format!(
                    "session {session_id} expected event sequence {expected_seq}, found {seq}"
                ),
            });
        }
        let (event, projected_payload_bytes) =
            decode_event_row(row, session_id, seq, payload_bytes)?;
        payload_bytes = projected_payload_bytes;
        events.push(event);
        expected_seq = expected_seq
            .checked_add(1)
            .ok_or_else(|| corrupt("transcript event sequence exhausted"))?;
        if events.len() == 1 {
            after_first_event
                .take()
                .expect("first-event hook runs once")();
        }
    }
    Ok(LoadedEvents {
        events,
        cursor: CommitCursor {
            next_seq: expected_seq,
            payload_bytes,
        },
    })
}

fn decode_event_row(
    row: &rusqlite::Row<'_>,
    session_id: &SessionId,
    seq: u64,
    current_payload_bytes: usize,
) -> Result<(TranscriptEvent, usize)> {
    let raw_payload_bytes = row
        .get::<_, i64>(1)
        .map_err(|error| AgentError::sqlite_read("read transcript payload length", error))?;
    let event_payload_bytes =
        usize::try_from(raw_payload_bytes).map_err(|_| AgentError::CorruptStore {
            message: format!("session {session_id} event {seq} has an invalid payload length"),
        })?;
    if event_payload_bytes > MAX_EVENT_PAYLOAD_BYTES {
        return Err(AgentError::CorruptStore {
            message: format!(
                "session {session_id} event {seq} payload is {event_payload_bytes} bytes; maximum is {MAX_EVENT_PAYLOAD_BYTES}"
            ),
        });
    }
    let projected_payload_bytes = current_payload_bytes
        .checked_add(event_payload_bytes)
        .ok_or_else(|| AgentError::CorruptStore {
            message: format!("session {session_id} transcript payload size overflowed"),
        })?;
    if projected_payload_bytes > MAX_SESSION_PAYLOAD_BYTES {
        return Err(AgentError::CorruptStore {
            message: format!(
                "session {session_id} transcript payloads total {projected_payload_bytes} bytes; maximum is {MAX_SESSION_PAYLOAD_BYTES}"
            ),
        });
    }

    // The SQL CASE prevents an oversized value from crossing the SQLite row
    // boundary. The cumulative check above runs before Rust materializes TEXT.
    let payload = row
        .get::<_, Option<Vec<u8>>>(2)
        .map_err(|error| AgentError::sqlite_read("read transcript payload", error))?
        .ok_or_else(|| AgentError::CorruptStore {
            message: format!(
                "session {session_id} event {seq} payload could not be read within its byte limit"
            ),
        })?;
    if payload.len() != event_payload_bytes {
        return Err(AgentError::CorruptStore {
            message: format!("session {session_id} transcript changed while it was read"),
        });
    }
    let kind = serde_json::from_slice::<TranscriptEventKind>(&payload).map_err(|error| {
        AgentError::CorruptStore {
            message: format!("session {session_id} event {seq} is invalid: {error}"),
        }
    })?;
    Ok((
        TranscriptEvent::new(EventSeq::new(seq), kind),
        projected_payload_bytes,
    ))
}

fn validate_stored_cursor(
    session_id: &SessionId,
    stored: CommitCursor,
    actual: CommitCursor,
) -> Result<()> {
    if stored != actual {
        return Err(AgentError::CorruptStore {
            message: format!(
                "session {session_id} commit cursor {stored:?} does not match events {actual:?}"
            ),
        });
    }
    Ok(())
}

fn initial_events(model: &str, prompt: &str, prompt_sha256: &str) -> [TranscriptEventKind; 4] {
    [
        TranscriptEventKind::SessionStarted {
            model: model.to_owned(),
            prompt_sha256: prompt_sha256.to_owned(),
        },
        TranscriptEventKind::TurnStarted,
        TranscriptEventKind::StepStarted {
            step: StepId::new(1),
        },
        TranscriptEventKind::UserMessage {
            content: prompt.to_owned(),
        },
    ]
}

#[cfg(test)]
fn read_commit_cursor(connection: &Connection, session_id: &SessionId) -> Result<CommitCursor> {
    let (next_seq, payload_bytes) = connection
        .query_row(
            "SELECT next_seq,payload_bytes FROM sessions
             WHERE session_id=?1 AND terminal=0",
            [session_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| AgentError::sqlite("read session commit cursor", error))?
        .ok_or_else(|| AgentError::CorruptStore {
            message: format!("session {session_id} is missing or terminal"),
        })?;
    let next_seq = u64::try_from(next_seq).map_err(|_| AgentError::CorruptStore {
        message: format!("session {session_id} has invalid next_seq"),
    })?;
    let payload_bytes = usize::try_from(payload_bytes).map_err(|_| AgentError::CorruptStore {
        message: format!("session {session_id} has invalid payload_bytes"),
    })?;
    if next_seq == 0 || payload_bytes > MAX_SESSION_PAYLOAD_BYTES {
        return Err(AgentError::CorruptStore {
            message: format!("session {session_id} has an invalid commit cursor"),
        });
    }
    Ok(CommitCursor {
        next_seq,
        payload_bytes,
    })
}

fn append_events_tx(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    expected: CommitCursor,
    events: &[TranscriptEventKind],
    terminal: bool,
) -> Result<CommitCursor> {
    if events.is_empty() {
        return Err(AgentError::CorruptStore {
            message: "event append must not be empty".to_owned(),
        });
    }
    if expected.next_seq == 0 || expected.payload_bytes > MAX_SESSION_PAYLOAD_BYTES {
        return Err(corrupt("caller supplied an invalid session commit cursor"));
    }
    let projected = expected
        .next_seq
        .checked_add(u64::try_from(events.len()).expect("event count fits u64"))
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| AgentError::CorruptStore {
            message: "event sequence exhausted".to_owned(),
        })?;
    if projected > u64::try_from(crate::MAX_TRANSCRIPT_EVENTS).expect("event limit fits u64") {
        return Err(AgentError::Persistence {
            operation: "append transcript events",
            message: format!(
                "session event limit {} exceeded",
                crate::MAX_TRANSCRIPT_EVENTS
            ),
        });
    }

    let (encoded, projected_payload_bytes) =
        encode_appended_events(session_id, expected.payload_bytes, events)?;

    let next_cursor = CommitCursor {
        next_seq: projected + 1,
        payload_bytes: projected_payload_bytes,
    };
    let changed = transaction
        .execute(
            "UPDATE sessions SET next_seq=?2,payload_bytes=?3,terminal=?4
             WHERE session_id=?1 AND terminal=0 AND next_seq=?5 AND payload_bytes=?6",
            params![
                session_id.as_str(),
                i64::try_from(next_cursor.next_seq).expect("bounded event seq fits i64"),
                i64::try_from(next_cursor.payload_bytes).expect("payload bound fits i64"),
                i64::from(terminal),
                i64::try_from(expected.next_seq).expect("bounded event seq fits i64"),
                i64::try_from(expected.payload_bytes).expect("payload bound fits i64")
            ],
        )
        .map_err(|error| AgentError::sqlite("advance event commit cursor", error))?;
    if changed != 1 {
        return Err(AgentError::CorruptStore {
            message: format!("session {session_id} commit cursor is stale"),
        });
    }

    for (offset, payload) in encoded.into_iter().enumerate() {
        let seq = expected.next_seq + u64::try_from(offset).expect("event offset fits u64");
        transaction
            .execute(
                "INSERT INTO events(session_id,seq,payload_json) VALUES (?1,?2,?3)",
                params![
                    session_id.as_str(),
                    i64::try_from(seq).expect("bounded event seq fits i64"),
                    payload
                ],
            )
            .map_err(|error| AgentError::sqlite("append transcript event", error))?;
    }
    Ok(next_cursor)
}

fn encode_appended_events(
    session_id: &SessionId,
    existing_payload_bytes: usize,
    events: &[TranscriptEventKind],
) -> Result<(Vec<String>, usize)> {
    if existing_payload_bytes > MAX_SESSION_PAYLOAD_BYTES {
        return Err(AgentError::CorruptStore {
            message: format!(
                "session {session_id} transcript payloads total {existing_payload_bytes} bytes; maximum is {MAX_SESSION_PAYLOAD_BYTES}"
            ),
        });
    }

    let mut projected_payload_bytes = existing_payload_bytes;
    let encoded = events
        .iter()
        .map(|event| {
            let payload =
                serde_json::to_string(event).map_err(|error| AgentError::CorruptStore {
                    message: format!("could not encode transcript event: {error}"),
                })?;
            if payload.len() > MAX_EVENT_PAYLOAD_BYTES {
                return Err(AgentError::Persistence {
                    operation: "append transcript events",
                    message: format!(
                        "encoded event payload is {} bytes; maximum is {MAX_EVENT_PAYLOAD_BYTES}",
                        payload.len()
                    ),
                });
            }
            projected_payload_bytes = projected_payload_bytes
                .checked_add(payload.len())
                .ok_or_else(|| AgentError::Persistence {
                    operation: "append transcript events",
                    message: "session transcript payload size overflowed".to_owned(),
                })?;
            if projected_payload_bytes > MAX_SESSION_PAYLOAD_BYTES {
                return Err(AgentError::Persistence {
                    operation: "append transcript events",
                    message: format!(
                        "session transcript payloads would total {projected_payload_bytes} bytes; maximum is {MAX_SESSION_PAYLOAD_BYTES}"
                    ),
                });
            }
            Ok(payload)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((encoded, projected_payload_bytes))
}

pub(crate) fn preflight_appended_events(
    session_id: &SessionId,
    cursor: CommitCursor,
    events: &[TranscriptEventKind],
) -> Result<()> {
    let projected = cursor
        .next_seq
        .checked_add(u64::try_from(events.len()).expect("event count fits u64"))
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| AgentError::Persistence {
            operation: "preflight transcript events",
            message: "event sequence exhausted".to_owned(),
        })?;
    if projected > u64::try_from(crate::MAX_TRANSCRIPT_EVENTS).expect("event limit fits u64") {
        return Err(AgentError::Persistence {
            operation: "preflight transcript events",
            message: format!(
                "session event limit {} exceeded",
                crate::MAX_TRANSCRIPT_EVENTS
            ),
        });
    }
    encode_appended_events(session_id, cursor.payload_bytes, events).map(|_| ())
}

#[derive(Clone, Debug)]
pub(crate) struct HealthLatch(Arc<AtomicBool>);

impl HealthLatch {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(AgentError::HostTerminal)
        }
    }

    pub(crate) fn poison(&self) {
        self.0.store(false, Ordering::Release);
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) struct CommitReceipt {
    pub(crate) cursor: CommitCursor,
    pub(crate) record: Option<RunRecord>,
}

#[derive(Clone, Debug)]
pub(crate) struct WriterHandle {
    sender: mpsc::Sender<WriterCommand>,
    health: HealthLatch,
    #[cfg(test)]
    commits: Arc<AtomicUsize>,
}

enum WriterCommand {
    Create {
        session_id: SessionId,
        model: String,
        prompt: String,
        response: oneshot::Sender<Result<CreateSession>>,
    },
    Commit {
        session_id: SessionId,
        expected: CommitCursor,
        events: Vec<TranscriptEventKind>,
        terminal: Option<RunStatus>,
        response: oneshot::Sender<Result<CommitReceipt>>,
    },
    AiPrepared {
        operation_id: AiOperationId,
        snapshot: Box<rsi_ai_meta::PreparedCallSnapshot>,
        response: oneshot::Sender<Result<()>>,
    },
    AiStarted {
        operation_id: AiOperationId,
        response: oneshot::Sender<Result<()>>,
    },
    AiTerminal {
        operation_id: AiOperationId,
        terminal: serde_json::Value,
        response: oneshot::Sender<Result<()>>,
    },
    #[cfg(test)]
    MakeWritesFail {
        response: oneshot::Sender<Result<()>>,
    },
    #[cfg(test)]
    GateNextDispatchCommit {
        gate: ThreadGate,
        response: oneshot::Sender<()>,
    },
    #[cfg(test)]
    FailNextDispatchCommitUncertain { response: oneshot::Sender<()> },
}

// Rust drops struct fields in declaration order. Keeping the SQLite owner
// first guarantees its connection closes before this worker releases its
// workspace lease, including early returns and panics during command handling.
struct WriterWorker {
    store: Store,
    lease: Arc<WorkspaceLease>,
}

impl WriterHandle {
    #[allow(clippy::too_many_lines)] // Thread setup and its closed command loop share ownership.
    pub(crate) async fn open(
        workspace: AgentWorkspace,
        health: HealthLatch,
        max_cold_reads: NonZeroU8,
    ) -> Result<(Self, ColdReader)> {
        let (sender, mut receiver) = mpsc::channel(STORE_QUEUE_CAPACITY);
        let (ready, opened) = oneshot::channel::<Result<Arc<WorkspaceLease>>>();
        let thread_health = health.clone();
        #[cfg(test)]
        let commits = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let thread_commits = Arc::clone(&commits);
        std::thread::Builder::new()
            .name("rsi-agent-store-writer".to_owned())
            .spawn(move || {
                let initialized = (|| {
                    let lease = Arc::new(WorkspaceLease::acquire(&workspace)?);
                    let database_path = lease.database_path();
                    let mut store = Store::open_guarded(&database_path, &lease)?;
                    store.recover_open_sessions()?;
                    store.recover_ai_operations()?;
                    store.verify_connection_not_moved()?;
                    lease.verify_database(&database_path)?;
                    Ok::<_, AgentError>(WriterWorker { store, lease })
                })();
                let mut worker = match initialized {
                    Ok(initialized) => {
                        if ready.send(Ok(Arc::clone(&initialized.lease))).is_err() {
                            return;
                        }
                        initialized
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                #[cfg(test)]
                let mut next_dispatch_gate = None::<ThreadGate>;
                #[cfg(test)]
                let mut fail_next_dispatch_commit_uncertain = false;
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        WriterCommand::Create {
                            session_id,
                            model,
                            prompt,
                            response,
                        } => {
                            let result = if thread_health.is_healthy() {
                                worker.store.create_session(&session_id, &model, &prompt)
                            } else {
                                Err(AgentError::HostTerminal)
                            };
                            #[cfg(test)]
                            if matches!(result, Ok(CreateSession::Created { .. })) {
                                thread_commits.fetch_add(1, Ordering::Relaxed);
                            }
                            poison_on_store_error(&thread_health, &result);
                            let _ = response.send(result);
                        }
                        WriterCommand::Commit {
                            session_id,
                            expected,
                            events,
                            terminal,
                            response,
                        } => {
                            #[cfg(test)]
                            let is_dispatch = events.iter().any(|event| {
                                matches!(event, TranscriptEventKind::ToolDispatchStarted { .. })
                            });
                            let result = if thread_health.is_healthy() {
                                worker
                                    .store
                                    .commit_expected(&session_id, expected, &events, terminal)
                                    .map(|(cursor, record)| CommitReceipt { cursor, record })
                            } else {
                                Err(AgentError::HostTerminal)
                            };
                            #[cfg(test)]
                            if result.is_ok() {
                                thread_commits.fetch_add(1, Ordering::Relaxed);
                            }
                            #[cfg(test)]
                            let result = if result.is_ok()
                                && is_dispatch
                                && fail_next_dispatch_commit_uncertain
                            {
                                fail_next_dispatch_commit_uncertain = false;
                                Err(AgentError::commit_outcome_unknown(
                                    "acknowledge test dispatch commit",
                                    rusqlite::Error::SqliteFailure(
                                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                                        Some("injected after durable dispatch commit".to_owned()),
                                    ),
                                ))
                            } else {
                                result
                            };
                            #[cfg(test)]
                            if result.is_ok()
                                && is_dispatch
                                && let Some(gate) = next_dispatch_gate.take()
                            {
                                gate.block_thread();
                            }
                            poison_on_store_error(&thread_health, &result);
                            let _ = response.send(result);
                        }
                        WriterCommand::AiPrepared {
                            operation_id,
                            snapshot,
                            response,
                        } => {
                            let result = if thread_health.is_healthy() {
                                worker.store.record_ai_prepared(&operation_id, &snapshot)
                            } else {
                                Err(AgentError::HostTerminal)
                            };
                            poison_on_store_error(&thread_health, &result);
                            let _ = response.send(result);
                        }
                        WriterCommand::AiStarted {
                            operation_id,
                            response,
                        } => {
                            let result = if thread_health.is_healthy() {
                                worker.store.record_ai_started(&operation_id)
                            } else {
                                Err(AgentError::HostTerminal)
                            };
                            poison_on_store_error(&thread_health, &result);
                            let _ = response.send(result);
                        }
                        WriterCommand::AiTerminal {
                            operation_id,
                            terminal,
                            response,
                        } => {
                            let result = if thread_health.is_healthy() {
                                worker.store.record_ai_terminal(&operation_id, &terminal)
                            } else {
                                Err(AgentError::HostTerminal)
                            };
                            poison_on_store_error(&thread_health, &result);
                            let _ = response.send(result);
                        }
                        #[cfg(test)]
                        WriterCommand::MakeWritesFail { response } => {
                            let result = worker.store.make_writes_fail();
                            let _ = response.send(result);
                        }
                        #[cfg(test)]
                        WriterCommand::GateNextDispatchCommit { gate, response } => {
                            next_dispatch_gate = Some(gate);
                            let _ = response.send(());
                        }
                        #[cfg(test)]
                        WriterCommand::FailNextDispatchCommitUncertain { response } => {
                            fail_next_dispatch_commit_uncertain = true;
                            let _ = response.send(());
                        }
                    }
                }
            })
            .map_err(|error| AgentError::io("start agent store writer", error))?;
        let lease = opened.await.map_err(|_| AgentError::WorkerStopped)??;
        let reader_path = lease.database_path();
        let writer = Self {
            sender,
            health: health.clone(),
            #[cfg(test)]
            commits,
        };
        let reader = ColdReader::new(reader_path, lease, health, max_cold_reads);
        Ok((writer, reader))
    }

    pub(crate) async fn create(
        &self,
        session_id: SessionId,
        model: String,
        prompt: String,
    ) -> Result<CreateSession> {
        self.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::Create {
                session_id,
                model,
                prompt,
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    pub(crate) async fn commit(
        &self,
        session_id: SessionId,
        expected: CommitCursor,
        events: Vec<TranscriptEventKind>,
        terminal: Option<RunStatus>,
    ) -> Result<CommitReceipt> {
        self.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::Commit {
                session_id,
                expected,
                events,
                terminal,
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    pub(crate) async fn ai_prepared(
        &self,
        operation_id: AiOperationId,
        snapshot: rsi_ai_meta::PreparedCallSnapshot,
    ) -> Result<()> {
        self.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::AiPrepared {
                operation_id,
                snapshot: Box::new(snapshot),
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    pub(crate) async fn ai_started(&self, operation_id: AiOperationId) -> Result<()> {
        self.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::AiStarted {
                operation_id,
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    pub(crate) async fn ai_terminal(
        &self,
        operation_id: AiOperationId,
        terminal: serde_json::Value,
    ) -> Result<()> {
        self.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::AiTerminal {
                operation_id,
                terminal,
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    pub(crate) fn check_health(&self) -> Result<()> {
        self.health.check()
    }

    #[cfg(test)]
    pub(crate) fn commit_count(&self) -> usize {
        self.commits.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) async fn make_writes_fail(&self) -> Result<()> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::MakeWritesFail { response })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    #[cfg(test)]
    pub(crate) async fn gate_next_dispatch_commit(&self) -> Result<ThreadGate> {
        let gate = ThreadGate::new();
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::GateNextDispatchCommit {
                gate: gate.clone(),
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?;
        Ok(gate)
    }

    #[cfg(test)]
    pub(crate) async fn fail_next_dispatch_commit_uncertain(&self) -> Result<()> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(WriterCommand::FailNextDispatchCommitUncertain { response })
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())
    }

    fn worker_stopped(&self) -> AgentError {
        self.health.poison();
        AgentError::WorkerStopped
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct ThreadGate {
    entered: Arc<Notify>,
    released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(test)]
impl ThreadGate {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            released: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
        }
    }

    fn block_thread(&self) {
        self.entered.notify_one();
        let (lock, condition) = &*self.released;
        let mut released = lock.lock().expect("probe gate lock");
        while !*released {
            released = condition.wait(released).expect("probe gate wait");
        }
    }

    pub(crate) async fn entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        let (lock, condition) = &*self.released;
        *lock.lock().expect("probe gate lock") = true;
        condition.notify_all();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ColdReader {
    path: Arc<PathBuf>,
    lease: Arc<WorkspaceLease>,
    slots: Arc<Semaphore>,
    health: HealthLatch,
    #[cfg(test)]
    next_probe_gate: Arc<std::sync::Mutex<Option<ThreadGate>>>,
}

impl ColdReader {
    fn new(
        path: PathBuf,
        lease: Arc<WorkspaceLease>,
        health: HealthLatch,
        max_cold_reads: NonZeroU8,
    ) -> Self {
        Self {
            path: Arc::new(path),
            lease,
            slots: Arc::new(Semaphore::new(usize::from(max_cold_reads.get()))),
            health,
            #[cfg(test)]
            next_probe_gate: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub(crate) fn workspace_root(&self) -> PathBuf {
        self.path
            .parent()
            .expect("agent database always has a workspace parent")
            .to_path_buf()
    }

    pub(crate) fn workspace_lease(&self) -> Arc<WorkspaceLease> {
        Arc::clone(&self.lease)
    }

    pub(crate) async fn probe(&self, session_id: SessionId) -> Result<ProbeSession> {
        self.health.check()?;
        let slot = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| self.worker_stopped())?;
        self.health.check()?;
        let path = Arc::clone(&self.path);
        let lease = Arc::clone(&self.lease);
        let health = self.health.clone();
        #[cfg(test)]
        let gate = self
            .next_probe_gate
            .lock()
            .expect("cold-reader gate lock")
            .take();
        let result = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let lease = lease;
            health.check()?;
            let store = Store::open_read_only_guarded(path.as_ref(), &lease)?;
            let result = store.probe_session(&session_id);
            #[cfg(test)]
            if let Some(gate) = gate {
                gate.block_thread();
            }
            store.verify_connection_not_moved()?;
            lease.verify_database(path.as_ref())?;
            result.map_err(|error| localize_session_corruption(&session_id, error))
        })
        .await
        .map_err(|_| self.worker_stopped())?;
        self.finish(result)
    }

    pub(crate) async fn transcript(&self, session_id: SessionId) -> Result<Option<Transcript>> {
        self.health.check()?;
        let slot = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| self.worker_stopped())?;
        self.health.check()?;
        let path = Arc::clone(&self.path);
        let lease = Arc::clone(&self.lease);
        let health = self.health.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let lease = lease;
            health.check()?;
            let store = Store::open_read_only_guarded(path.as_ref(), &lease)?;
            let result = store.transcript(&session_id);
            store.verify_connection_not_moved()?;
            lease.verify_database(path.as_ref())?;
            result.map_err(|error| localize_session_corruption(&session_id, error))
        })
        .await
        .map_err(|_| self.worker_stopped())?;
        self.finish(result)
    }

    #[cfg(test)]
    pub(crate) fn gate_next_probe(&self) -> Result<ThreadGate> {
        let gate = ThreadGate::new();
        let replaced = self
            .next_probe_gate
            .lock()
            .expect("cold-reader gate lock")
            .replace(gate.clone());
        if replaced.is_some() {
            return Err(AgentError::Persistence {
                operation: "install cold-reader test gate",
                message: "a probe gate is already pending".to_owned(),
            });
        }
        Ok(gate)
    }

    fn finish<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            match error.store_error_class() {
                StoreErrorClass::SessionCorrupt | StoreErrorClass::ReadUnavailable => {}
                StoreErrorClass::NotStoreRelated if matches!(error, AgentError::HostTerminal) => {}
                StoreErrorClass::FatalStore
                | StoreErrorClass::CommitOutcomeUnknown
                | StoreErrorClass::NotStoreRelated => self.health.poison(),
            }
        }
        result
    }

    fn worker_stopped(&self) -> AgentError {
        self.health.poison();
        AgentError::WorkerStopped
    }
}

fn localize_session_corruption(session_id: &SessionId, error: AgentError) -> AgentError {
    match error {
        AgentError::CorruptStore { message } => AgentError::CorruptSession {
            session_id: session_id.clone(),
            message,
        },
        error => error,
    }
}

fn poison_on_store_error<T>(health: &HealthLatch, result: &Result<T>) {
    if matches!(
        result,
        Err(AgentError::CorruptStore { .. }
            | AgentError::Persistence { .. }
            | AgentError::CommitOutcomeUnknown { .. })
    ) {
        health.poison();
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("agent.sqlite3");
        let store = Store::open(&path).expect("store");
        (temp, store)
    }

    fn prepared_request(
        events: &[TranscriptEvent],
        request_id: &str,
    ) -> crate::ModelRequestSnapshot {
        let context = events
            .iter()
            .find_map(|event| match event.kind() {
                TranscriptEventKind::ContextSnapshot { context } => Some(context.clone()),
                _ => None,
            })
            .expect("context");
        let projection = crate::transcript::project_model_visible(
            &events
                .iter()
                .map(|event| event.kind().clone())
                .collect::<Vec<_>>(),
        )
        .expect("projection");
        let source = events.last().expect("source event").seq();
        crate::transcript::prepare_projected_model_request(&context, projection, source, request_id)
            .expect("request")
    }

    fn prepared_model_call(request: &crate::ModelRequestSnapshot) -> TranscriptEventKind {
        TranscriptEventKind::ModelCallPrepared {
            request_id: request.request_id.clone(),
            snapshot: rsi_ai_meta::PreparedCallSnapshot {
                call_id: request.request_id.clone(),
                deployment_id: "fixture".to_owned(),
                provider_family: "fixture".to_owned(),
                capability: rsi_ai_meta::Capability::Language,
                model: request.model.clone(),
                protocol: "fixture".to_owned(),
                transport: "memory".to_owned(),
                endpoint_fingerprint: "fixture".to_owned(),
                config_generation: 1,
                credential_source: None,
                retry_policy: rsi_ai_meta::RetryPolicy::default(),
                request_sha256: request.sha256.clone(),
            },
        }
    }

    fn ai_snapshot(call_id: &str) -> rsi_ai_meta::PreparedCallSnapshot {
        rsi_ai_meta::PreparedCallSnapshot {
            call_id: call_id.to_owned(),
            deployment_id: "fixture".to_owned(),
            provider_family: "fixture".to_owned(),
            capability: rsi_ai_meta::Capability::Image,
            model: "fixture-model".to_owned(),
            protocol: "fixture".to_owned(),
            transport: "memory".to_owned(),
            endpoint_fingerprint: "fixture".to_owned(),
            config_generation: 1,
            credential_source: None,
            retry_policy: rsi_ai_meta::RetryPolicy::default(),
            request_sha256: "a".repeat(64),
        }
    }

    fn ai_phase(store: &Store, operation_id: &AiOperationId) -> (i64, Option<String>) {
        store
            .connection
            .query_row(
                "SELECT phase,terminal_json FROM ai_operations WHERE operation_id=?1",
                [operation_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("AI operation row")
    }

    #[test]
    fn ai_operation_barriers_are_durable_and_duplicate_ids_are_rejected() {
        let (_temp, mut store) = store();
        let id = AiOperationId::new("image-one").expect("operation id");
        store
            .record_ai_prepared(&id, &ai_snapshot("provider-call-1"))
            .expect("prepared barrier");
        assert_eq!(ai_phase(&store, &id), (0, None));
        assert!(matches!(
            store.record_ai_prepared(&id, &ai_snapshot("provider-call-2")),
            Err(AgentError::AiOperationConflict { .. })
        ));

        store.record_ai_started(&id).expect("started barrier");
        assert_eq!(ai_phase(&store, &id), (1, None));
        store
            .record_ai_terminal(&id, &serde_json::json!({"status":"succeeded"}))
            .expect("terminal barrier");
        assert_eq!(
            ai_phase(&store, &id),
            (2, Some(r#"{"status":"succeeded"}"#.to_owned()))
        );
    }

    #[test]
    fn recovery_never_replays_unfinished_ai_operations() {
        let (_temp, mut store) = store();
        let prepared = AiOperationId::new("prepared-only").expect("operation id");
        let started = AiOperationId::new("started-only").expect("operation id");
        store
            .record_ai_prepared(&prepared, &ai_snapshot("provider-call-1"))
            .expect("prepared");
        store
            .record_ai_prepared(&started, &ai_snapshot("provider-call-2"))
            .expect("prepared");
        store.record_ai_started(&started).expect("started");

        store.recover_ai_operations().expect("recovery");
        assert_eq!(
            ai_phase(&store, &prepared),
            (2, Some(r#"{"status":"not_started"}"#.to_owned()))
        );
        assert_eq!(
            ai_phase(&store, &started),
            (2, Some(r#"{"status":"outcome_unknown"}"#.to_owned()))
        );
    }

    #[test]
    fn completed_ai_operations_do_not_consume_the_open_operation_quota() {
        let (_temp, mut store) = store();
        let snapshot = serde_json::to_string(&ai_snapshot("historical-call")).expect("snapshot");
        let transaction = store.connection.transaction().expect("transaction");
        for index in 0..MAX_AI_OPERATIONS {
            transaction
                .execute(
                    "INSERT INTO ai_operations(operation_id,prepared_json,phase,terminal_json)
                     VALUES (?1,?2,2,'{\"status\":\"succeeded\"}')",
                    rusqlite::params![format!("historical-{index}"), snapshot],
                )
                .expect("historical terminal row");
        }
        transaction.commit().expect("commit history");

        store
            .recover_ai_operations()
            .expect("terminal history is valid");
        let next = AiOperationId::new("next-open-operation").expect("operation id");
        store
            .record_ai_prepared(&next, &ai_snapshot("next-provider-call"))
            .expect("open-operation quota excludes terminal history");
    }

    #[test]
    fn terminal_ai_operation_is_preflighted_before_the_store_transition() {
        let oversized = serde_json::json!({
            "status":"succeeded",
            "result":{"transcription":"\"".repeat(9 * 1024 * 1024)}
        });
        assert!(preflight_ai_terminal(&oversized).is_err());
        assert!(preflight_ai_terminal(&serde_json::json!({"status":"failed"})).is_ok());
    }

    struct ClosedSession {
        _temp: tempfile::TempDir,
        workspace: AgentWorkspace,
        health: HealthLatch,
        _writer: WriterHandle,
        reader: ColdReader,
        id: SessionId,
    }

    async fn closed_interrupted_session(name: &str) -> ClosedSession {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (writer, reader) = WriterHandle::open(
            workspace.clone(),
            health.clone(),
            NonZeroU8::new(2).expect("nonzero"),
        )
        .await
        .expect("store workers");
        let id = SessionId::new(name).expect("id");
        let CreateSession::Created { cursor, .. } = writer
            .create(id.clone(), "default".to_owned(), "hello".to_owned())
            .await
            .expect("create")
        else {
            panic!("new session expected")
        };
        writer
            .commit(
                id.clone(),
                cursor,
                vec![
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Interrupted,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Interrupted,
                    },
                ],
                Some(RunStatus::Interrupted),
            )
            .await
            .expect("close session");
        ClosedSession {
            _temp: temp,
            workspace,
            health,
            _writer: writer,
            reader,
            id,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_blocked_cold_read_does_not_block_another_session() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (_writer, reader) =
            WriterHandle::open(workspace, health, NonZeroU8::new(2).expect("nonzero"))
                .await
                .expect("store workers");
        let gate = reader.gate_next_probe().expect("probe gate");
        let blocked_reader = reader.clone();
        let blocked = tokio::spawn(async move {
            blocked_reader
                .probe(SessionId::new("blocked-cold-read").expect("id"))
                .await
        });
        gate.entered().await;

        let independent = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reader.probe(SessionId::new("independent-cold-read").expect("id")),
        )
        .await
        .expect("independent probe must not queue behind a blocked session")
        .expect("independent probe");
        assert!(matches!(independent, ProbeSession::Missing));

        gate.release();
        assert!(matches!(
            blocked.await.expect("blocked task").expect("blocked probe"),
            ProbeSession::Missing
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_lease_outlives_writer_and_in_flight_cold_reads() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (writer, reader) = WriterHandle::open(
            workspace.clone(),
            health,
            NonZeroU8::new(1).expect("nonzero"),
        )
        .await
        .expect("store workers");
        let gate = reader.gate_next_probe().expect("probe gate");
        let blocked_reader = reader.clone();
        let blocked = tokio::spawn(async move {
            blocked_reader
                .probe(SessionId::new("lease-cold-read").expect("id"))
                .await
        });
        gate.entered().await;

        drop(reader);
        drop(writer);
        assert!(matches!(
            WorkspaceLease::acquire(&workspace),
            Err(AgentError::WorkspaceOccupied { .. })
        ));

        gate.release();
        blocked.await.expect("blocked task").expect("blocked probe");
        let lease = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match WorkspaceLease::acquire(&workspace) {
                    Ok(lease) => break lease,
                    Err(AgentError::WorkspaceOccupied { .. }) => tokio::task::yield_now().await,
                    Err(error) => panic!("unexpected reopen error: {error}"),
                }
            }
        })
        .await
        .expect("workers release lease after all connections close");
        drop(lease);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cold_reader_rejects_database_moved_during_selected_read() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (_writer, reader) = WriterHandle::open(
            workspace.clone(),
            health.clone(),
            NonZeroU8::new(1).expect("nonzero"),
        )
        .await
        .expect("store workers");
        let gate = reader.gate_next_probe().expect("probe gate");
        let blocked_reader = reader.clone();
        let blocked = tokio::spawn(async move {
            blocked_reader
                .probe(SessionId::new("moved-cold-read").expect("id"))
                .await
        });
        gate.entered().await;
        std::fs::rename(
            workspace.database_path(),
            workspace.root().join("moved.sqlite3"),
        )
        .expect("move open database");
        gate.release();

        let Err(error) = blocked.await.expect("probe task") else {
            panic!("moved connection must not return a trusted read");
        };
        assert!(
            matches!(error, AgentError::CorruptStore { ref message } if message.contains("moved")),
            "unexpected error: {error}"
        );
        assert!(matches!(health.check(), Err(AgentError::HostTerminal)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn corrupt_terminal_session_is_local_to_that_identifier() {
        let session = closed_interrupted_session("locally-corrupt").await;
        Connection::open(session.workspace.database_path())
            .expect("corrupting connection")
            .execute(
                "UPDATE events SET payload_json='{}' WHERE session_id=?1 AND seq=1",
                [session.id.as_str()],
            )
            .expect("corrupt selected session");

        assert!(matches!(
            session.reader.transcript(session.id.clone()).await,
            Err(AgentError::CorruptSession { session_id, .. }) if session_id == session.id
        ));
        session
            .health
            .check()
            .expect("session-local corruption is isolated");
        assert!(matches!(
            session
                .reader
                .probe(SessionId::new("unrelated-session").expect("id"))
                .await
                .expect("unrelated probe"),
            ProbeSession::Missing
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_utf8_terminal_prompt_is_local_to_that_identifier() {
        let session = closed_interrupted_session("invalid-utf8-prompt").await;
        Connection::open(session.workspace.database_path())
            .expect("corrupting connection")
            .execute(
                "UPDATE sessions SET prompt=CAST(X'80' AS TEXT) WHERE session_id=?1",
                [session.id.as_str()],
            )
            .expect("write invalid UTF-8 prompt");

        assert!(matches!(
            session.reader.transcript(session.id.clone()).await,
            Err(AgentError::CorruptSession { session_id, .. }) if session_id == session.id
        ));
        session
            .health
            .check()
            .expect("selected prompt corruption is session-local");
        assert!(matches!(
            session
                .reader
                .probe(SessionId::new("after-invalid-prompt").expect("id"))
                .await
                .expect("unrelated probe"),
            ProbeSession::Missing
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_utf8_terminal_event_is_local_to_that_identifier() {
        let session = closed_interrupted_session("invalid-utf8-event").await;
        Connection::open(session.workspace.database_path())
            .expect("corrupting connection")
            .execute(
                "UPDATE events SET payload_json=CAST(X'80' AS TEXT)
                 WHERE session_id=?1 AND seq=1",
                [session.id.as_str()],
            )
            .expect("write invalid UTF-8 event");

        assert!(matches!(
            session.reader.transcript(session.id.clone()).await,
            Err(AgentError::CorruptSession { session_id, .. }) if session_id == session.id
        ));
        session
            .health
            .check()
            .expect("selected event corruption is session-local");
        assert!(matches!(
            session
                .reader
                .probe(SessionId::new("after-invalid-event").expect("id"))
                .await
                .expect("unrelated probe"),
            ProbeSession::Missing
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_events_with_an_open_header_are_session_corruption() {
        let session = closed_interrupted_session("terminal-events-open-header").await;
        Connection::open(session.workspace.database_path())
            .expect("corrupting connection")
            .execute(
                "UPDATE sessions SET terminal=0 WHERE session_id=?1",
                [session.id.as_str()],
            )
            .expect("clear terminal flag");

        assert!(matches!(
            session.reader.probe(session.id.clone()).await,
            Err(AgentError::CorruptSession { session_id, .. }) if session_id == session.id
        ));
        session
            .health
            .check()
            .expect("selected probe corruption is session-local");
        assert!(matches!(
            session.reader.transcript(session.id.clone()).await,
            Err(AgentError::CorruptSession { session_id, .. }) if session_id == session.id
        ));
        session
            .health
            .check()
            .expect("selected header corruption is session-local");
        assert!(matches!(
            session
                .reader
                .probe(SessionId::new("after-open-header").expect("id"))
                .await
                .expect("unrelated probe"),
            ProbeSession::Missing
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unsupervised_open_session_requires_recovery_and_poisons_health() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (writer, reader) = WriterHandle::open(
            workspace,
            health.clone(),
            NonZeroU8::new(1).expect("nonzero"),
        )
        .await
        .expect("store workers");
        let id = SessionId::new("unsupervised-open").expect("id");
        assert!(matches!(
            writer
                .create(id.clone(), "default".to_owned(), "hello".to_owned())
                .await
                .expect("create"),
            CreateSession::Created { .. }
        ));

        assert!(matches!(
            reader.transcript(id.clone()).await,
            Err(AgentError::RecoveryRequired { session_id, .. }) if session_id == id
        ));
        assert!(matches!(health.check(), Err(AgentError::HostTerminal)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertain_dispatch_test_seam_commits_before_poisoning() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (writer, _reader) = WriterHandle::open(
            workspace.clone(),
            health.clone(),
            NonZeroU8::new(1).expect("nonzero"),
        )
        .await
        .expect("store workers");
        let id = SessionId::new("uncertain-dispatch").expect("id");
        let CreateSession::Created { cursor, .. } = writer
            .create(id.clone(), "default".to_owned(), "hello".to_owned())
            .await
            .expect("create")
        else {
            panic!("new session expected")
        };
        writer
            .fail_next_dispatch_commit_uncertain()
            .await
            .expect("arm uncertainty seam");

        let error = writer
            .commit(
                id.clone(),
                cursor,
                vec![TranscriptEventKind::ToolDispatchStarted {
                    call_id: crate::CallId::new("call-1").expect("call id"),
                }],
                None,
            )
            .await
            .expect_err("dispatch acknowledgement must be uncertain");
        assert_eq!(
            error.store_error_class(),
            StoreErrorClass::CommitOutcomeUnknown
        );
        assert!(matches!(health.check(), Err(AgentError::HostTerminal)));
        assert_eq!(writer.commit_count(), 2, "both transactions reached COMMIT");

        let durable_dispatches: i64 = Connection::open(workspace.database_path())
            .expect("inspection connection")
            .query_row(
                "SELECT COUNT(*) FROM events WHERE session_id=?1 AND seq=?2",
                params![
                    id.as_str(),
                    i64::try_from(cursor.next_seq).expect("seq fits")
                ],
                |row| row.get(0),
            )
            .expect("durable dispatch count");
        assert_eq!(durable_dispatches, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_schema_corruption_poisons_the_store_health_latch() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (_writer, reader) = WriterHandle::open(
            workspace.clone(),
            health.clone(),
            NonZeroU8::new(1).expect("nonzero"),
        )
        .await
        .expect("store workers");
        Connection::open(workspace.database_path())
            .expect("corrupting connection")
            .execute("DROP INDEX open_sessions_by_id", [])
            .expect("corrupt shared schema");

        assert!(matches!(
            reader
                .probe(SessionId::new("schema-corruption").expect("id"))
                .await,
            Err(AgentError::CorruptStore { .. })
        ));
        assert!(matches!(health.check(), Err(AgentError::HostTerminal)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_unavailable_does_not_poison_the_cold_reader() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let health = HealthLatch::new();
        let (_writer, reader) = WriterHandle::open(
            workspace,
            health.clone(),
            NonZeroU8::new(1).expect("nonzero"),
        )
        .await
        .expect("store workers");
        let busy = AgentError::sqlite_read(
            "probe locked store",
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            ),
        );

        assert!(matches!(
            reader.finish::<()>(Err(busy)),
            Err(AgentError::ReadUnavailable { .. })
        ));
        health.check().expect("read contention is session-local");
        assert!(matches!(
            reader
                .probe(SessionId::new("after-read-contention").expect("id"))
                .await
                .expect("subsequent read"),
            ProbeSession::Missing
        ));
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_open_does_not_follow_a_database_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.sqlite3");
        drop(Store::open(&target).expect("target store"));
        let link = temp.path().join("link.sqlite3");
        symlink(&target, &link).expect("symlink");
        assert!(matches!(
            Store::open(&link),
            Err(AgentError::Persistence { .. })
        ));
    }

    #[test]
    fn sqlite_reports_the_exact_logical_database_page_limit() {
        let (_temp, store) = store();
        let page_size = store
            .connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, u64>(0))
            .expect("page size");
        let max_pages = store
            .connection
            .pragma_query_value(None, "max_page_count", |row| row.get::<_, u64>(0))
            .expect("max pages");
        assert_eq!(
            max_pages,
            u64::try_from(MAX_DATABASE_BYTES).expect("limit") / page_size
        );
        assert!(max_pages * page_size <= u64::try_from(MAX_DATABASE_BYTES).expect("limit"));
        assert!((max_pages + 1) * page_size > u64::try_from(MAX_DATABASE_BYTES).expect("limit"));
    }

    #[test]
    fn accepted_file_store_uses_wal() {
        let (_temp, store) = store();
        let mode = store
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .expect("journal mode");
        assert!(mode.eq_ignore_ascii_case("wal"), "unexpected mode: {mode}");
    }

    #[test]
    fn configure_rejects_a_store_that_cannot_enter_wal() {
        let connection = Connection::open_in_memory().expect("memory connection");
        let max_pages = validate_database_size(&connection).expect("database size");
        assert!(matches!(
            configure_store(&connection, max_pages),
            Err(AgentError::Persistence {
                operation: "verify WAL journal mode",
                ..
            })
        ));
    }

    #[test]
    fn database_size_limit_precedes_schema_inspection() {
        use std::io::{Seek, SeekFrom, Write};

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("agent.sqlite3");
        drop(Store::open(&path).expect("store"));
        let connection = Connection::open(&path).expect("connection");
        let page_size = connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, u64>(0))
            .expect("page size");
        connection
            .execute(
                "UPDATE store_meta SET value='999' WHERE key='schema_version'",
                [],
            )
            .expect("future schema marker");
        drop(connection);
        let oversized_pages = u64::try_from(MAX_DATABASE_BYTES).expect("limit") / page_size + 1;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("database file");
        file.set_len(oversized_pages * page_size)
            .expect("extend sparse database");
        file.seek(SeekFrom::Start(28)).expect("page-count field");
        file.write_all(
            &u32::try_from(oversized_pages)
                .expect("test page count fits SQLite header")
                .to_be_bytes(),
        )
        .expect("logical page count");
        drop(file);

        for result in [Store::open(&path), Store::open_read_only(&path)] {
            assert!(
                matches!(
                    &result,
                    Err(AgentError::Persistence {
                        operation: "validate store size",
                        ..
                    })
                ),
                "unexpected error ordering: {result:?}"
            );
        }
    }

    #[test]
    fn schema_v4_has_only_the_recovery_index_and_rejects_schema_drift() {
        let malformed = tempdir().expect("tempdir");
        let malformed_path = malformed.path().join("agent.sqlite3");
        let connection = Connection::open(&malformed_path).expect("connection");
        let non_strict_sessions = SESSIONS_SQL.trim_end_matches(" STRICT");
        connection
            .execute_batch(&format!(
                "{STORE_META_SQL};
                 INSERT INTO store_meta(key,value) VALUES ('schema_version','4');
                 {non_strict_sessions};
                 {OPEN_INDEX_SQL};
                 {EVENTS_SQL};"
            ))
            .expect("malformed v4 schema");
        drop(connection);
        assert!(matches!(
            Store::open(&malformed_path),
            Err(AgentError::CorruptStore { .. })
        ));

        let triggered = tempdir().expect("tempdir");
        let triggered_path = triggered.path().join("agent.sqlite3");
        drop(Store::open(&triggered_path).expect("store"));
        Connection::open(&triggered_path)
            .expect("connection")
            .execute_batch(
                "CREATE TRIGGER mutate_events AFTER INSERT ON events
                 BEGIN
                     DELETE FROM events WHERE session_id=NEW.session_id AND seq=NEW.seq;
                 END;",
            )
            .expect("trigger");
        assert!(matches!(
            Store::open(&triggered_path),
            Err(AgentError::CorruptStore { .. })
        ));

        let valid = tempdir().expect("tempdir");
        let valid_path = valid.path().join("agent.sqlite3");
        drop(Store::open(&valid_path).expect("store"));
        let connection = Connection::open(&valid_path).expect("connection");
        let indexes = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type='index' AND sql IS NOT NULL ORDER BY name",
            )
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })
            .expect("indexes");
        assert_eq!(indexes, ["open_sessions_by_id"]);
    }

    #[test]
    fn schema_v1_through_v3_are_rejected_without_migration() {
        for version in [1_u32, 2, 3] {
            let temp = tempdir().expect("tempdir");
            let path = temp.path().join("agent.sqlite3");
            drop(Store::open(&path).expect("store"));
            Connection::open(&path)
                .expect("connection")
                .execute(
                    "UPDATE store_meta SET value=?1 WHERE key='schema_version'",
                    [version.to_string()],
                )
                .expect("downgrade version marker");

            assert!(matches!(
                Store::open(&path),
                Err(AgentError::UnsupportedStoreVersion {
                    found,
                    expected: STORE_SCHEMA_VERSION
                }) if found == version
            ));
        }
    }

    #[test]
    fn moved_database_handle_is_rejected_before_the_next_write() {
        let (temp, mut store) = store();
        let original = temp.path().join("agent.sqlite3");
        let moved = temp.path().join("moved.sqlite3");
        let seed = SessionId::new("before-move").expect("id");
        store.begin_session(&seed, "hello").expect("seed session");
        std::fs::rename(&original, &moved).expect("move open database");

        let after = SessionId::new("after-move").expect("id");
        let Err(error) = store.begin_session(&after, "must not commit") else {
            panic!("moved database handle must be rejected");
        };
        assert!(matches!(error, AgentError::CorruptStore { .. }));
        assert!(
            error.to_string().contains("moved"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn guarded_open_rejects_a_replacement_before_configuring_it() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let lease = WorkspaceLease::acquire(&workspace).expect("lease");
        let database = workspace.database_path();
        let displaced = workspace.root().join("displaced.sqlite3");
        std::fs::rename(&database, displaced).expect("displace guarded database");
        let replacement = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&database)
            .expect("replacement database");
        replacement
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .expect("replacement permissions");
        drop(replacement);

        assert!(matches!(
            Store::open_guarded(&database, &lease),
            Err(AgentError::Persistence {
                operation: "verify opened agent store",
                ..
            })
        ));

        let replacement = Connection::open(&database).expect("inspect replacement");
        let journal_mode = replacement
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .expect("journal mode");
        let user_objects = replacement
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE sql IS NOT NULL",
                [],
                |row| row.get::<_, u64>(0),
            )
            .expect("schema objects");
        assert_eq!(journal_mode, "delete");
        assert_eq!(user_objects, 0);
    }

    #[test]
    fn recovery_page_uses_the_partial_keyset_index() {
        let (_temp, store) = store();
        let details = store
            .connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT session_id FROM sessions
                 WHERE terminal=0 AND session_id > ?1
                 ORDER BY session_id LIMIT ?2",
            )
            .and_then(|mut statement| {
                statement
                    .query_map(params!["cursor", SESSION_PAGE_SIZE], |row| {
                        row.get::<_, String>(3)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })
            .expect("query plan");
        assert!(
            details.iter().any(|detail| {
                detail.contains("SEARCH sessions USING COVERING INDEX open_sessions_by_id")
                    && detail.contains("session_id>?")
            }),
            "unexpected query plan: {details:?}"
        );
    }

    #[test]
    fn recovery_visits_more_than_one_open_session_page() {
        let (_temp, mut store) = store();
        let count = SESSION_PAGE_SIZE + 3;
        for index in 0..count {
            let id = SessionId::new(format!("paged-recovery-{index:03}")).expect("session id");
            assert!(matches!(
                store
                    .create_session(&id, "default", "hello")
                    .expect("create"),
                CreateSession::Created { .. }
            ));
        }

        store.recover_open_sessions().expect("recover every page");
        let (open, terminal) = store
            .connection
            .query_row(
                "SELECT SUM(terminal=0),SUM(terminal=1) FROM sessions",
                [],
                |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)),
            )
            .expect("session counts");
        assert_eq!(open, 0);
        assert_eq!(terminal, count);
        for index in [0, SESSION_PAGE_SIZE - 1, SESSION_PAGE_SIZE, count - 1] {
            let id = SessionId::new(format!("paged-recovery-{index:03}")).expect("session id");
            assert_eq!(
                store
                    .transcript(&id)
                    .expect("transcript")
                    .expect("present")
                    .status(),
                &RunStatus::Interrupted
            );
        }
    }

    #[test]
    fn terminal_replay_uses_the_committed_system_prompt() {
        let (temp, mut store) = store();
        let id = SessionId::new("captured-system-prompt").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let custom_prompt = "Use the committed policy, not the current default.";
        store
            .append(
                &id,
                &[TranscriptEventKind::ContextSnapshot {
                    context: crate::ContextSnapshot {
                        system_prompt: custom_prompt.to_owned(),
                        model: "default".to_owned(),
                        model_provider: "model".to_owned(),
                        model_protocol_version: rsi_agent_protocol::WIRE_VERSION,
                        tools_provider: "tools".to_owned(),
                        tools_protocol_version: rsi_agent_protocol::WIRE_VERSION,
                        tools: Vec::new(),
                    },
                }],
            )
            .expect("context");
        let request = prepared_request(&store.load_events(&id).expect("events"), "model-1");
        let prepared = prepared_model_call(&request);
        store
            .append(
                &id,
                &[
                    TranscriptEventKind::ModelRequestPrepared { request },
                    prepared,
                    TranscriptEventKind::AssistantMessage {
                        message: crate::AssistantMessage {
                            content: Some("done".to_owned()),
                            reasoning: None,
                            tool_calls: Vec::new(),
                            finish_reason: rsi_ai_protocol::FinishReason::Stop,
                            usage: None,
                            replay: None,
                            warnings: Vec::new(),
                            sources: Vec::new(),
                        },
                    },
                ],
            )
            .expect("model result");
        store
            .append_terminal(
                &id,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Completed,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Completed,
                    },
                ],
                RunStatus::Completed {
                    final_message: "done".to_owned(),
                },
            )
            .expect("terminal");
        drop(store);

        let reopened = Store::open(&temp.path().join("agent.sqlite3")).expect("reopen");
        let transcript = reopened
            .transcript(&id)
            .expect("valid replay")
            .expect("transcript");
        let canonical = transcript
            .events()
            .iter()
            .find_map(|event| match event.kind() {
                TranscriptEventKind::ModelRequestPrepared { request } => {
                    Some(request.canonical_json.as_bytes())
                }
                _ => None,
            })
            .expect("model request");
        let request = serde_json::from_slice::<rsi_ai_protocol::LanguageRequest>(canonical)
            .expect("committed request decodes");
        assert!(matches!(
            request.messages().first().and_then(|message| message.content().first()),
            Some(rsi_ai_protocol::MessageContent::Text { text }) if text == custom_prompt
        ));
    }

    #[test]
    fn stale_commit_cursor_is_rejected_without_partial_events() {
        let (_temp, mut store) = store();
        let id = SessionId::new("stale-cursor").expect("id");
        let CreateSession::Created { cursor, .. } = store
            .create_session(&id, "default", "hello")
            .expect("create")
        else {
            panic!("new session expected")
        };
        store
            .commit_expected(
                &id,
                cursor,
                &[TranscriptEventKind::UserMessage {
                    content: "first".to_owned(),
                }],
                None,
            )
            .expect("first commit");
        assert!(matches!(
            store.commit_expected(
                &id,
                cursor,
                &[TranscriptEventKind::UserMessage {
                    content: "stale".to_owned(),
                }],
                None,
            ),
            Err(AgentError::CorruptStore { .. })
        ));
        assert_eq!(event_row_count(&store, &id), 5);
    }

    #[test]
    fn recovery_rejects_open_session_header_counters() {
        let (_temp, mut store) = store();
        let id = SessionId::new("bad-open-counters").expect("id");
        store.begin_session(&id, "hello").expect("session");
        store
            .connection
            .execute(
                "UPDATE sessions SET payload_bytes=payload_bytes+1 WHERE session_id=?1",
                [id.as_str()],
            )
            .expect("corrupt counter");
        assert!(matches!(
            store.recover_open_sessions(),
            Err(AgentError::CorruptStore { .. })
        ));
    }

    #[test]
    fn durable_event_row_count_is_bounded_before_returning_events() {
        let (_temp, mut store) = store();
        let id = SessionId::new("too-many-durable-events").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let transaction = store.connection.transaction().expect("transaction");
        transaction
            .execute(
                "WITH RECURSIVE seq(value) AS (
                     SELECT 5
                     UNION ALL
                     SELECT value + 1 FROM seq WHERE value < ?2
                 )
                 INSERT INTO events(session_id,seq,payload_json)
                 SELECT ?1,value,'{\"event\":\"user_message\",\"content\":\"x\"}' FROM seq",
                params![
                    id.as_str(),
                    i64::try_from(crate::MAX_TRANSCRIPT_EVENTS + 1).expect("event limit fits i64")
                ],
            )
            .expect("seed excess durable rows");
        transaction.commit().expect("commit corruption");

        match store.load_events(&id) {
            Err(AgentError::CorruptStore { message }) => {
                assert!(
                    message.contains("event limit"),
                    "unexpected error: {message}"
                );
            }
            Ok(events) => panic!(
                "materialized {} durable rows before enforcing the {}-event limit",
                events.len(),
                crate::MAX_TRANSCRIPT_EVENTS
            ),
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn durable_event_payload_size_is_bounded_before_deserialization() {
        let (_temp, mut store) = store();
        let id = SessionId::new("oversized-durable-event").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let payload =
            serde_json::to_string(&user_message_with_encoded_len(MAX_EVENT_PAYLOAD_BYTES + 1))
                .expect("payload");
        let payload_bytes = payload.len();
        store
            .connection
            .execute(
                "UPDATE events SET payload_json=?2 WHERE session_id=?1 AND seq=4",
                params![id.as_str(), payload],
            )
            .expect("seed oversized durable row");

        match store.load_events(&id) {
            Err(AgentError::CorruptStore { message }) => {
                assert!(message.contains("payload"), "unexpected error: {message}");
            }
            Ok(events) => panic!(
                "materialized a {payload_bytes}-byte durable payload before enforcing its bound ({} events returned)",
                events.len()
            ),
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn event_sequence_gap_is_rejected_before_payload_materialization() {
        let (_temp, mut store) = store();
        let id = SessionId::new("gap-before-large-payload").expect("id");
        store.begin_session(&id, "hello").expect("session");
        store
            .connection
            .execute(
                "UPDATE events
                 SET seq=5,payload_json=CAST(zeroblob(?2) AS TEXT)
                 WHERE session_id=?1 AND seq=4",
                params![
                    id.as_str(),
                    i64::try_from(MAX_EVENT_PAYLOAD_BYTES + 1).expect("payload bound fits i64")
                ],
            )
            .expect("seed gap with oversized payload");

        match store.load_events(&id) {
            Err(AgentError::CorruptStore { message }) => assert!(
                message.contains("expected event sequence 4, found 5"),
                "unexpected error: {message}"
            ),
            Ok(events) => panic!("accepted gapped transcript with {} events", events.len()),
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn single_query_event_read_uses_one_sqlite_snapshot() {
        let (temp, mut store) = store();
        let id = SessionId::new("stable-read-snapshot").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let writer = Connection::open(temp.path().join("agent.sqlite3")).expect("writer");
        let replacement = serde_json::to_string(&TranscriptEventKind::UserMessage {
            content: "jello".to_owned(),
        })
        .expect("replacement");

        let events = load_events_from_with_hook(&store.connection, &id, || {
            writer
                .execute(
                    "UPDATE events SET payload_json=?2 WHERE session_id=?1 AND seq=4",
                    params![id.as_str(), replacement],
                )
                .expect("replace same-length payload while the read cursor is active");
        })
        .expect("snapshot read");

        assert!(store.connection.is_autocommit());
        assert!(matches!(
            events[3].kind(),
            TranscriptEventKind::UserMessage { content } if content == "hello"
        ));
        let stored_payload: String = writer
            .query_row(
                "SELECT payload_json FROM events WHERE session_id=?1 AND seq=4",
                [id.as_str()],
                |row| row.get(0),
            )
            .expect("updated payload");
        assert_eq!(stored_payload, replacement);
    }

    #[test]
    fn event_limit_rejects_the_whole_append_before_writing() {
        let (_temp, mut store) = store();
        let id = SessionId::new("event-limit").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let overflow = vec![
            TranscriptEventKind::UserMessage {
                content: "overflow".to_owned(),
            };
            crate::MAX_TRANSCRIPT_EVENTS - 3
        ];
        assert!(matches!(
            store.append(&id, &overflow),
            Err(AgentError::Persistence { .. })
        ));
        assert_eq!(store.load_events(&id).expect("events").len(), 4);
    }

    #[test]
    fn append_enforces_encoded_event_payload_bound_before_writing() {
        let (_temp, mut store) = store();
        let id = SessionId::new("append-event-payload-limit").expect("id");
        store.begin_session(&id, "hello").expect("session");

        let at_limit = user_message_with_encoded_len(MAX_EVENT_PAYLOAD_BYTES);
        store.append(&id, &[at_limit]).expect("exact event limit");
        let rows_before = event_row_count(&store, &id);
        let over_limit = user_message_with_encoded_len(MAX_EVENT_PAYLOAD_BYTES + 1);
        assert!(matches!(
            store.append(&id, &[over_limit]),
            Err(AgentError::Persistence { .. })
        ));
        assert_eq!(event_row_count(&store, &id), rows_before);
    }

    #[test]
    fn append_enforces_encoded_session_payload_bound_before_writing() {
        let (_temp, mut store) = store();
        let id = SessionId::new("append-session-payload-limit").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let initial_bytes = session_payload_bytes(&store, &id);
        let remaining = MAX_SESSION_PAYLOAD_BYTES - initial_bytes;
        let event_count = remaining.div_ceil(MAX_EVENT_PAYLOAD_BYTES);
        let base_size = remaining / event_count;
        let larger_events = remaining % event_count;
        for index in 0..event_count {
            let encoded_len = base_size + usize::from(index < larger_events);
            store
                .append(&id, &[user_message_with_encoded_len(encoded_len)])
                .expect("fill exact session payload limit");
        }
        assert_eq!(
            session_payload_bytes(&store, &id),
            MAX_SESSION_PAYLOAD_BYTES
        );
        let rows_before = event_row_count(&store, &id);
        assert!(matches!(
            store.append(
                &id,
                &[TranscriptEventKind::UserMessage {
                    content: String::new()
                }]
            ),
            Err(AgentError::Persistence { .. })
        ));
        assert_eq!(event_row_count(&store, &id), rows_before);
    }

    fn user_message_with_encoded_len(encoded_len: usize) -> TranscriptEventKind {
        let overhead = serde_json::to_vec(&TranscriptEventKind::UserMessage {
            content: String::new(),
        })
        .expect("empty event")
        .len();
        assert!(encoded_len >= overhead);
        let event = TranscriptEventKind::UserMessage {
            content: "x".repeat(encoded_len - overhead),
        };
        assert_eq!(
            serde_json::to_vec(&event).expect("event").len(),
            encoded_len
        );
        event
    }

    fn event_row_count(store: &Store, id: &SessionId) -> usize {
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE session_id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .expect("event row count")
    }

    fn session_payload_bytes(store: &Store, id: &SessionId) -> usize {
        store
            .connection
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(payload_json AS BLOB))),0)
                 FROM events WHERE session_id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .expect("session payload bytes")
    }

    fn append_two_prepared_calls(store: &mut Store, id: &SessionId) -> [crate::CallId; 2] {
        store
            .append(
                id,
                &[TranscriptEventKind::ContextSnapshot {
                    context: crate::ContextSnapshot {
                        system_prompt: crate::SYSTEM_PROMPT.to_owned(),
                        model: "default".to_owned(),
                        model_provider: "test-model".to_owned(),
                        model_protocol_version: rsi_agent_protocol::WIRE_VERSION,
                        tools_provider: "test-tools".to_owned(),
                        tools_protocol_version: rsi_agent_protocol::WIRE_VERSION,
                        tools: vec![crate::ToolDefinition {
                            name: "missing".to_owned(),
                            description: "test tool".to_owned(),
                            input_schema: serde_json::json!({"type":"object"}),
                        }],
                    },
                }],
            )
            .expect("context");
        let request = prepared_request(&store.load_events(id).expect("events"), "model-1");
        let prepared = prepared_model_call(&request);
        let first = crate::CallId::new("z-call").expect("id");
        let second = crate::CallId::new("a-call").expect("id");
        let calls = [
            crate::ToolCall {
                id: first.clone(),
                name: "missing".to_owned(),
                arguments: "{}".to_owned(),
            },
            crate::ToolCall {
                id: second.clone(),
                name: "missing".to_owned(),
                arguments: "{}".to_owned(),
            },
        ];
        store
            .append(
                id,
                &[
                    TranscriptEventKind::ModelRequestPrepared { request },
                    prepared,
                    TranscriptEventKind::AssistantMessage {
                        message: crate::AssistantMessage {
                            content: None,
                            reasoning: None,
                            tool_calls: calls.to_vec(),
                            finish_reason: rsi_ai_protocol::FinishReason::ToolCalls,
                            usage: None,
                            replay: None,
                            warnings: Vec::new(),
                            sources: Vec::new(),
                        },
                    },
                    TranscriptEventKind::ToolCallPrepared {
                        call: calls[0].clone(),
                    },
                    TranscriptEventKind::ToolCallPrepared {
                        call: calls[1].clone(),
                    },
                ],
            )
            .expect("prepared calls");
        [first, second]
    }

    #[test]
    fn session_identity_is_idempotent_and_prompt_bound() {
        let (_temp, mut store) = store();
        let id = SessionId::new("session-1").expect("id");
        assert!(matches!(
            store.begin_session(&id, "hello").expect("new"),
            BeginSession::Created
        ));
        store
            .append_terminal(
                &id,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Interrupted,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Interrupted,
                    },
                ],
                RunStatus::Interrupted,
            )
            .expect("terminal");
        assert!(matches!(
            store.begin_session(&id, "hello").expect("replay"),
            BeginSession::Existing
        ));
        assert!(matches!(
            store.begin_session(&id, "different"),
            Err(AgentError::SessionConflict { .. })
        ));
    }

    #[test]
    fn unknown_store_version_and_corrupt_event_fail_closed() {
        let version_temp = tempdir().expect("tempdir");
        let version_path = version_temp.path().join("agent.sqlite3");
        drop(Store::open(&version_path).expect("store"));
        Connection::open(&version_path)
            .expect("connection")
            .execute(
                "UPDATE store_meta SET value='999' WHERE key='schema_version'",
                [],
            )
            .expect("change version");
        assert!(matches!(
            Store::open(&version_path),
            Err(AgentError::UnsupportedStoreVersion {
                found: 999,
                expected: STORE_SCHEMA_VERSION
            })
        ));

        let (corrupt_temp, mut store) = store();
        let id = SessionId::new("corrupt").expect("id");
        store.begin_session(&id, "hello").expect("session");
        store
            .append_terminal(
                &id,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Interrupted,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Interrupted,
                    },
                ],
                RunStatus::Interrupted,
            )
            .expect("terminal");
        drop(store);
        let corrupt_path = corrupt_temp.path().join("agent.sqlite3");
        Connection::open(&corrupt_path)
            .expect("connection")
            .execute(
                "UPDATE events SET payload_json='{}' WHERE session_id=?1 AND seq=1",
                [id.as_str()],
            )
            .expect("corrupt event");
        let mut reopened = Store::open(&corrupt_path).expect("store schema remains valid");
        assert!(matches!(
            reopened.transcript(&id),
            Err(AgentError::CorruptStore { .. })
        ));
        assert!(matches!(
            reopened.begin_session(&id, "hello"),
            Err(AgentError::CorruptStore { .. })
        ));
        reopened
            .recover_open_sessions()
            .expect("cold terminal corruption is validated lazily");
    }

    #[test]
    fn nonempty_unversioned_database_is_rejected_without_rebranding() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("agent.sqlite3");
        Connection::open(&path)
            .expect("seed connection")
            .execute("CREATE TABLE foreign_data(value TEXT)", [])
            .expect("seed foreign schema");

        assert!(matches!(
            Store::open(&path),
            Err(AgentError::CorruptStore { .. })
        ));
        let connection = Connection::open(&path).expect("inspect rejected store");
        let has_meta: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='store_meta')",
                [],
                |row| row.get(0),
            )
            .expect("inspect metadata");
        let foreign_data: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='foreign_data')",
                [],
                |row| row.get(0),
            )
            .expect("inspect foreign schema");
        assert!(!has_meta);
        assert!(foreign_data);
    }

    #[test]
    fn rejected_foreign_database_preserves_journal_mode() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("agent.sqlite3");
        let connection = Connection::open(&path).expect("seed connection");
        connection
            .execute("CREATE TABLE foreign_data(value TEXT)", [])
            .expect("seed foreign schema");
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("set delete journal mode");
        drop(connection);

        assert!(matches!(
            Store::open(&path),
            Err(AgentError::CorruptStore { .. })
        ));
        assert_eq!(journal_mode(&path), "delete");
        assert!(!sqlite_sidecar(&path, "-wal").exists());
        assert!(!sqlite_sidecar(&path, "-shm").exists());
    }

    #[test]
    fn rejected_future_database_preserves_journal_mode() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("agent.sqlite3");
        drop(Store::open(&path).expect("seed current store"));
        let connection = Connection::open(&path).expect("seed connection");
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("set delete journal mode");
        connection
            .execute(
                "UPDATE store_meta SET value='999' WHERE key='schema_version'",
                [],
            )
            .expect("set future schema version");
        drop(connection);

        assert!(matches!(
            Store::open(&path),
            Err(AgentError::UnsupportedStoreVersion {
                found: 999,
                expected: STORE_SCHEMA_VERSION
            })
        ));
        assert_eq!(journal_mode(&path), "delete");
        assert!(!sqlite_sidecar(&path, "-wal").exists());
        assert!(!sqlite_sidecar(&path, "-shm").exists());
    }

    #[test]
    fn oversized_schema_version_is_rejected_before_journal_configuration() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("agent.sqlite3");
        drop(Store::open(&path).expect("seed current store"));
        let connection = Connection::open(&path).expect("seed connection");
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("set delete journal mode");
        connection
            .execute(
                "UPDATE store_meta
                 SET value=CAST(zeroblob(?1) AS TEXT)
                 WHERE key='schema_version'",
                [i64::try_from(MAX_SCHEMA_VERSION_BYTES + 1).expect("bound fits i64")],
            )
            .expect("oversize schema version");
        drop(connection);

        assert!(matches!(
            Store::open(&path),
            Err(AgentError::CorruptStore { message })
                if message.contains("schema_version")
        ));
        assert_eq!(journal_mode(&path), "delete");
    }

    #[test]
    fn recovery_bounds_session_identifiers_before_materialization() {
        let (_temp, mut store) = store();
        let oversized_id = "x".repeat(MAX_SESSION_ID_BYTES + 1);
        store
            .connection
            .execute(
                "INSERT INTO sessions(session_id,prompt,terminal,next_seq,payload_bytes)
                 VALUES (?1,'hello',0,1,0)",
                params![oversized_id],
            )
            .expect("seed oversized identifier");

        assert!(matches!(
            store.recover_open_sessions(),
            Err(AgentError::CorruptStore { message })
                if message.contains("session identifier")
        ));
    }

    #[test]
    fn transcript_bounds_session_header_text_before_materialization() {
        let (_temp, mut store) = store();
        let id = SessionId::new("oversized-header-prompt").expect("id");
        store.begin_session(&id, "hello").expect("session");
        close_interrupted(&mut store, &id);
        store
            .connection
            .execute(
                "UPDATE sessions SET prompt=CAST(zeroblob(?2) AS TEXT) WHERE session_id=?1",
                params![
                    id.as_str(),
                    i64::try_from(crate::MAX_PROMPT_BYTES + 1).expect("bound fits i64")
                ],
            )
            .expect("oversize session header");
        assert!(matches!(
            store.transcript(&id),
            Err(AgentError::CorruptStore { message })
                if message.contains("session prompt")
        ));
    }

    fn close_interrupted(store: &mut Store, id: &SessionId) {
        store
            .append_terminal(
                id,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Interrupted,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Interrupted,
                    },
                ],
                RunStatus::Interrupted,
            )
            .expect("terminal session");
    }

    fn journal_mode(path: &Path) -> String {
        let connection = Connection::open(path).expect("inspect database");
        connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("read journal mode")
    }

    fn sqlite_sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        name.into()
    }

    #[test]
    fn recovery_validates_before_commit_and_retries_corrupt_open_logs() {
        let (temp, mut store) = store();
        let id = SessionId::new("corrupt-open").expect("id");
        store.begin_session(&id, "hello").expect("session");
        store
            .connection
            .execute(
                "UPDATE events SET payload_json=?2 WHERE session_id=?1 AND seq=1",
                params![
                    id.as_str(),
                    r#"{"event":"session_started","prompt_sha256":"wrong"}"#
                ],
            )
            .expect("corrupt prefix");
        assert!(matches!(
            store.recover_open_sessions(),
            Err(AgentError::CorruptStore { .. })
        ));
        let (terminal, next_seq): (i64, i64) = store
            .connection
            .query_row(
                "SELECT terminal,next_seq FROM sessions WHERE session_id=?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("session row");
        assert_eq!(terminal, 0);
        assert_eq!(next_seq, 5);
        drop(store);

        let path = temp.path().join("agent.sqlite3");
        let mut reopened = Store::open(&path).expect("store schema remains readable");
        assert!(matches!(
            reopened.recover_open_sessions(),
            Err(AgentError::CorruptStore { .. })
        ));
    }

    #[test]
    fn terminal_validation_rejects_reordered_calls_and_mismatched_boundaries() {
        let (_temp, mut store) = store();
        let reordered = SessionId::new("reordered").expect("id");
        store.begin_session(&reordered, "hello").expect("session");
        let [first, second] = append_two_prepared_calls(&mut store, &reordered);
        for call_id in [&second, &first] {
            store
                .append(
                    &reordered,
                    &[TranscriptEventKind::ToolResult {
                        call_id: call_id.clone(),
                        outcome: ToolOutcome::Failed {
                            code: "unknown_tool".to_owned(),
                            message: "tool is unavailable".to_owned(),
                        },
                    }],
                )
                .expect("persist adversarial result");
        }
        let failure = crate::Failure::new(crate::FailureKind::ToolProtocol, "stop");
        assert!(matches!(
            store.append_terminal(
                &reordered,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Failed {
                            failure: failure.clone(),
                        },
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Failed {
                            failure: failure.clone(),
                        },
                    },
                ],
                RunStatus::Failed { failure },
            ),
            Err(AgentError::CorruptStore { .. })
        ));

        let mismatched = SessionId::new("mismatched").expect("id");
        store.begin_session(&mismatched, "hello").expect("session");
        let failure = crate::Failure::new(crate::FailureKind::ModelUnavailable, "failed");
        assert!(matches!(
            store.append_terminal(
                &mismatched,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Failed { failure },
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Interrupted,
                    },
                ],
                RunStatus::Interrupted,
            ),
            Err(AgentError::CorruptStore { .. })
        ));
    }

    #[test]
    fn terminal_validation_rejects_unbounded_failure_messages() {
        let (_temp, mut store) = store();
        for (suffix, message) in [
            (
                "oversized",
                "x".repeat(crate::domain::MAX_FAILURE_MESSAGE_BYTES + 1),
            ),
            ("control", "invalid\0failure".to_owned()),
        ] {
            let id = SessionId::new(format!("invalid-failure-{suffix}")).expect("id");
            store.begin_session(&id, "hello").expect("session");
            let failure = crate::Failure {
                kind: crate::FailureKind::ModelProtocol,
                message,
            };
            assert!(matches!(
                store.append_terminal(
                    &id,
                    &[
                        TranscriptEventKind::StepEnded {
                            step: StepId::new(1),
                            outcome: BoundaryOutcome::Failed {
                                failure: failure.clone(),
                            },
                        },
                        TranscriptEventKind::TurnEnded {
                            outcome: BoundaryOutcome::Failed {
                                failure: failure.clone(),
                            },
                        },
                    ],
                    RunStatus::Failed { failure },
                ),
                Err(AgentError::CorruptStore { .. })
            ));
            assert_eq!(store.load_events(&id).expect("rolled back events").len(), 4);
            assert!(store.record(&id).expect("record").is_none());
        }
    }

    #[test]
    fn terminal_validation_rejects_impossible_tool_dispatches() {
        let (_temp, mut store) = store();
        for (suffix, name, arguments) in [
            ("unknown", "missing", "{}"),
            ("schema", "echo", "[]"),
            ("duplicate-json", "echo", r#"{"text":1,"text":2}"#),
        ] {
            let id = SessionId::new(format!("invalid-dispatch-{suffix}")).expect("id");
            store.begin_session(&id, "hello").expect("session");
            let call = crate::ToolCall {
                id: crate::CallId::new(format!("call-{suffix}")).expect("call id"),
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            };
            prepare_call(&mut store, &id, &call);
            store
                .append(
                    &id,
                    &[TranscriptEventKind::ToolDispatchStarted {
                        call_id: call.id.clone(),
                    }],
                )
                .expect("persist adversarial dispatch");
            assert!(matches!(
                store.append_terminal(
                    &id,
                    &[
                        TranscriptEventKind::ToolResult {
                            call_id: call.id.clone(),
                            outcome: ToolOutcome::OutcomeUnknown,
                        },
                        TranscriptEventKind::StepEnded {
                            step: StepId::new(1),
                            outcome: BoundaryOutcome::Interrupted,
                        },
                        TranscriptEventKind::TurnEnded {
                            outcome: BoundaryOutcome::Interrupted,
                        },
                    ],
                    RunStatus::Interrupted,
                ),
                Err(AgentError::CorruptStore { .. })
            ));
            assert!(store.record(&id).expect("record").is_none());
        }
    }

    #[test]
    fn terminal_validation_rejects_dispatch_on_the_final_model_step() {
        let (_temp, mut store) = store();
        let id = SessionId::new("final-step-dispatch").expect("id");
        store.begin_session(&id, "hello").expect("session");
        append_echo_context(&mut store, &id);

        for step_number in 1..=crate::MAX_STEPS {
            let step = StepId::new(u64::from(step_number));
            let events = store.load_events(&id).expect("events");
            let request = prepared_request(&events, &format!("model-{step_number}"));
            let call = crate::ToolCall {
                id: crate::CallId::new(format!("call-{step_number}")).expect("call id"),
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            };
            store
                .append(
                    &id,
                    &[
                        TranscriptEventKind::ModelRequestPrepared { request },
                        TranscriptEventKind::AssistantMessage {
                            message: crate::AssistantMessage {
                                content: None,
                                reasoning: None,
                                tool_calls: vec![call.clone()],
                                finish_reason: rsi_ai_protocol::FinishReason::ToolCalls,
                                usage: None,
                                replay: None,
                                warnings: Vec::new(),
                                sources: Vec::new(),
                            },
                        },
                        TranscriptEventKind::ToolCallPrepared { call: call.clone() },
                        TranscriptEventKind::ToolDispatchStarted {
                            call_id: call.id.clone(),
                        },
                        TranscriptEventKind::ToolResult {
                            call_id: call.id,
                            outcome: ToolOutcome::Succeeded {
                                value: serde_json::json!({}),
                            },
                        },
                    ],
                )
                .expect("adversarial executed step");
            if step_number < crate::MAX_STEPS {
                store
                    .append(
                        &id,
                        &[
                            TranscriptEventKind::StepEnded {
                                step,
                                outcome: BoundaryOutcome::Continued,
                            },
                            TranscriptEventKind::StepStarted {
                                step: StepId::new(u64::from(step_number + 1)),
                            },
                        ],
                    )
                    .expect("next step");
            }
        }

        let failure = crate::Failure::new(
            crate::FailureKind::StepLimitExceeded,
            "final step requested another tool",
        );
        assert!(matches!(
            store.append_terminal(
                &id,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(u64::from(crate::MAX_STEPS)),
                        outcome: BoundaryOutcome::Failed {
                            failure: failure.clone(),
                        },
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Failed {
                            failure: failure.clone(),
                        },
                    },
                ],
                RunStatus::Failed { failure },
            ),
            Err(AgentError::CorruptStore { .. })
        ));
        assert!(store.record(&id).expect("record").is_none());
    }

    #[test]
    fn terminal_validation_does_not_expose_unstarted_work_to_a_later_model_step() {
        let (_temp, mut store) = store();
        let id = SessionId::new("continued-not-started").expect("id");
        store.begin_session(&id, "hello").expect("session");
        let call = crate::ToolCall {
            id: crate::CallId::new("call-1").expect("call id"),
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        };
        prepare_call(&mut store, &id, &call);
        store
            .append(
                &id,
                &[
                    TranscriptEventKind::ToolResult {
                        call_id: call.id,
                        outcome: ToolOutcome::NotStarted {
                            reason: "interrupted_before_dispatch".to_owned(),
                        },
                    },
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(1),
                        outcome: BoundaryOutcome::Continued,
                    },
                    TranscriptEventKind::StepStarted {
                        step: StepId::new(2),
                    },
                ],
            )
            .expect("adversarial continuation");
        let events = store.load_events(&id).expect("events");
        let request = prepared_request(&events, "model-2");
        store
            .append(
                &id,
                &[
                    TranscriptEventKind::ModelRequestPrepared { request },
                    TranscriptEventKind::AssistantMessage {
                        message: crate::AssistantMessage {
                            content: Some("done".to_owned()),
                            reasoning: None,
                            tool_calls: Vec::new(),
                            finish_reason: rsi_ai_protocol::FinishReason::Stop,
                            usage: None,
                            replay: None,
                            warnings: Vec::new(),
                            sources: Vec::new(),
                        },
                    },
                ],
            )
            .expect("adversarial final answer");
        assert!(matches!(
            store.append_terminal(
                &id,
                &[
                    TranscriptEventKind::StepEnded {
                        step: StepId::new(2),
                        outcome: BoundaryOutcome::Completed,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Completed,
                    },
                ],
                RunStatus::Completed {
                    final_message: "done".to_owned(),
                },
            ),
            Err(AgentError::CorruptStore { .. })
        ));
        assert!(store.record(&id).expect("record").is_none());
    }

    #[test]
    fn recovery_distinguishes_not_started_and_unknown() {
        let (temp, mut store) = store();
        let first = SessionId::new("not-started").expect("id");
        store.begin_session(&first, "hello").expect("session");
        let call = crate::ToolCall {
            id: crate::CallId::new("call-1").expect("call id"),
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        };
        prepare_call(&mut store, &first, &call);
        drop(store);

        let mut reopened = Store::open(&temp.path().join("agent.sqlite3")).expect("reopen");
        reopened.recover_open_sessions().expect("recover");
        let transcript = reopened
            .transcript(&first)
            .expect("transcript")
            .expect("present");
        assert!(transcript.events().iter().any(|event| matches!(
            event.kind(),
            TranscriptEventKind::ToolResult {
                outcome: ToolOutcome::NotStarted { .. },
                ..
            }
        )));

        let second = SessionId::new("unknown").expect("id");
        reopened.begin_session(&second, "hello").expect("session");
        prepare_call(&mut reopened, &second, &call);
        reopened
            .append(
                &second,
                &[TranscriptEventKind::ToolDispatchStarted {
                    call_id: call.id.clone(),
                }],
            )
            .expect("dispatch");
        drop(reopened);
        let mut reopened = Store::open(&temp.path().join("agent.sqlite3")).expect("reopen");
        reopened.recover_open_sessions().expect("recover");
        let transcript = reopened
            .transcript(&second)
            .expect("transcript")
            .expect("present");
        assert!(transcript.events().iter().any(|event| matches!(
            event.kind(),
            TranscriptEventKind::ToolResult {
                outcome: ToolOutcome::OutcomeUnknown,
                ..
            }
        )));

        let ordered = SessionId::new("ordered-recovery").expect("id");
        reopened.begin_session(&ordered, "hello").expect("session");
        let [ordered_first, ordered_second] = append_two_prepared_calls(&mut reopened, &ordered);
        reopened
            .append(
                &ordered,
                &[TranscriptEventKind::ToolDispatchStarted {
                    call_id: ordered_first.clone(),
                }],
            )
            .expect("dispatch");
        drop(reopened);
        let mut reopened = Store::open(&temp.path().join("agent.sqlite3")).expect("reopen");
        reopened.recover_open_sessions().expect("recover");
        let transcript = reopened
            .transcript(&ordered)
            .expect("transcript")
            .expect("present");
        let recovered = transcript
            .events()
            .iter()
            .filter_map(|event| match event.kind() {
                TranscriptEventKind::ToolResult { call_id, outcome } => {
                    Some((call_id.clone(), outcome.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            recovered.as_slice(),
            [(first, ToolOutcome::OutcomeUnknown), (second, ToolOutcome::NotStarted { .. })]
                if first == &ordered_first && second == &ordered_second
        ));
    }

    fn append_echo_context(store: &mut Store, session_id: &SessionId) {
        store
            .append(
                session_id,
                &[TranscriptEventKind::ContextSnapshot {
                    context: crate::ContextSnapshot {
                        system_prompt: crate::SYSTEM_PROMPT.to_owned(),
                        model: "default".to_owned(),
                        model_provider: "model".to_owned(),
                        model_protocol_version: rsi_agent_protocol::WIRE_VERSION,
                        tools_provider: "tools".to_owned(),
                        tools_protocol_version: rsi_agent_protocol::WIRE_VERSION,
                        tools: vec![crate::ToolDefinition {
                            name: "echo".to_owned(),
                            description: "echo".to_owned(),
                            input_schema: serde_json::json!({
                                "type": "object",
                                "additionalProperties": false
                            }),
                        }],
                    },
                }],
            )
            .expect("context");
    }

    fn prepare_call(store: &mut Store, session_id: &SessionId, call: &crate::ToolCall) {
        append_echo_context(store, session_id);
        let events = store.load_events(session_id).expect("events");
        let request = prepared_request(&events, "model-1");
        let prepared = prepared_model_call(&request);
        store
            .append(
                session_id,
                &[
                    TranscriptEventKind::ModelRequestPrepared { request },
                    prepared,
                    TranscriptEventKind::AssistantMessage {
                        message: crate::AssistantMessage {
                            content: None,
                            reasoning: None,
                            tool_calls: vec![call.clone()],
                            finish_reason: rsi_ai_protocol::FinishReason::ToolCalls,
                            usage: None,
                            replay: None,
                            warnings: Vec::new(),
                            sources: Vec::new(),
                        },
                    },
                    TranscriptEventKind::ToolCallPrepared { call: call.clone() },
                ],
            )
            .expect("prepared call");
    }
}
