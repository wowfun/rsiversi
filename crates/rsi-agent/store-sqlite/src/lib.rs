//! Exact-schema `SQLite` and filesystem-CAS Agent Store ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    EMPTY_FACT_PREFIX_DIGEST, MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_SESSION_HEADER_BYTES,
    SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId, advance_fact_prefix_digest,
};
use rsi_agent_store_protocol::{
    AGENT_STORE_SCHEMA_VERSION, AppendBatch, AppendCommit, CasObjectRef,
    MAXIMUM_CONTEXT_CHECKPOINT_BYTES, MAXIMUM_STORE_CAS_BYTES, MAXIMUM_STORE_FACT_PAGE_BYTES,
    Result, SessionStore, SessionStoreContract, StoreError, StoreFactPage, StoreOpenTurn,
    StoreOpenTurnPage, StoreTurnFactPage, StoredContextCheckpoint, WriteContextCheckpoint,
    validate_read_limit,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAXIMUM_ORPHANED_CAS_STAGING_FILES: usize = 64;
const EXPECTED_TABLES: [(&str, &str); 5] = [
    (
        "sessions",
        "CREATE TABLE sessions (
            session_id TEXT PRIMARY KEY NOT NULL,
            header_json TEXT NOT NULL,
            durable_seq INTEGER NOT NULL CHECK (durable_seq >= 0),
            fact_prefix_sha256 TEXT NOT NULL
         ) STRICT",
    ),
    (
        "facts",
        "CREATE TABLE facts (
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE RESTRICT,
            seq INTEGER NOT NULL CHECK (seq > 0),
            turn_id TEXT NOT NULL,
            fact_kind TEXT NOT NULL
                CHECK (fact_kind IN ('accepted', 'terminal', 'event')),
            fact_json TEXT NOT NULL,
            PRIMARY KEY (session_id, seq)
         ) STRICT",
    ),
    (
        "turns",
        "CREATE TABLE turns (
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE RESTRICT,
            turn_id TEXT NOT NULL,
            accepted_seq INTEGER NOT NULL CHECK (accepted_seq > 0),
            terminal_seq INTEGER CHECK (terminal_seq > accepted_seq),
            PRIMARY KEY (session_id, turn_id),
            UNIQUE (session_id, accepted_seq),
            UNIQUE (session_id, terminal_seq),
            FOREIGN KEY (session_id, accepted_seq)
                REFERENCES facts(session_id, seq) ON DELETE RESTRICT,
            FOREIGN KEY (session_id, terminal_seq)
                REFERENCES facts(session_id, seq) ON DELETE RESTRICT
         ) STRICT",
    ),
    (
        "context_checkpoints",
        "CREATE TABLE context_checkpoints (
            session_id TEXT PRIMARY KEY NOT NULL
                REFERENCES sessions(session_id) ON DELETE RESTRICT,
            header_fingerprint TEXT NOT NULL,
            through_seq INTEGER NOT NULL CHECK (through_seq > 0),
            fact_prefix_sha256 TEXT NOT NULL,
            checkpoint_bytes BLOB NOT NULL
         ) STRICT",
    ),
    (
        "cas_objects",
        "CREATE TABLE cas_objects (
            sha256 TEXT PRIMARY KEY NOT NULL,
            byte_len INTEGER NOT NULL CHECK (byte_len > 0)
         ) STRICT",
    ),
];
const EXPECTED_INDEXES: [(&str, &str); 2] = [
    (
        "facts_by_turn",
        "CREATE INDEX facts_by_turn ON facts (session_id, turn_id, seq)",
    ),
    (
        "open_turns_by_session",
        "CREATE INDEX open_turns_by_session ON turns (session_id, accepted_seq)
         WHERE terminal_seq IS NULL",
    ),
];

/// Explicit `SQLite` Store plugin configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteStoreConfig {
    /// Absolute root owned by this Store instance.
    pub root: PathBuf,
}

impl SqliteStoreConfig {
    fn validate(&self) -> Result<()> {
        if !self.root.is_absolute() || self.root.as_os_str().is_empty() {
            return Err(StoreError::Invalid(
                "SQLite Agent Store root must be an absolute path".into(),
            ));
        }
        Ok(())
    }
}

/// Open Store holding the exact root writer lease until its last clone drops.
#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
    database_admission: Arc<Semaphore>,
    cas_admission: Arc<Semaphore>,
    root: Arc<PathBuf>,
    cas_dir: Arc<PathBuf>,
    cas_staging_dir: Arc<PathBuf>,
    _writer_lock: Arc<File>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteStore")
            .field("root", &self.root)
            .field("cas_dir", &self.cas_dir)
            .finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Opens or creates one exact-schema Store after acquiring its writer lease.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = prepare_root(root.as_ref())?;
        let writer_lock = Arc::new(acquire_writer_lock(&root)?);
        let cas_dir = root.join("cas");
        prepare_owned_directory(&cas_dir, "CAS directory")?;
        let cas_staging_dir = cas_dir.join("staging");
        prepare_cas_staging_directory(&cas_staging_dir)?;
        let database_path = root.join("sessions.sqlite3");
        reject_symlink_if_present(&database_path, "SQLite database")?;
        let mut connection = Connection::open(&database_path).map_err(sql_error)?;
        configure(&connection)?;
        initialize_or_validate_schema(&mut connection)?;
        validate_database(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            database_admission: Arc::new(Semaphore::new(1)),
            cas_admission: Arc::new(Semaphore::new(1)),
            root: Arc::new(root),
            cas_dir: Arc::new(cas_dir),
            cas_staging_dir: Arc::new(cas_staging_dir),
            _writer_lock: writer_lock,
        })
    }

    async fn with_database<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.database_admission)
            .acquire_owned()
            .await
            .map_err(|_| StoreError::Io("SQLite database admission closed".into()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|error| StoreError::Io(format!("SQLite worker failed: {error}")))?
    }

    async fn with_connection<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        self.with_database(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| StoreError::Io("SQLite connection mutex was poisoned".into()))?;
            operation(&mut connection)
        })
        .await
    }

    async fn with_cas<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.cas_admission)
            .acquire_owned()
            .await
            .map_err(|_| StoreError::Io("CAS file admission closed".into()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|error| StoreError::Io(format!("CAS worker failed: {error}")))?
    }
}

#[async_trait]
impl SessionStore for SqliteStore {
    async fn append(&self, batch: AppendBatch) -> Result<AppendCommit> {
        batch.validate()?;
        self.with_connection(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            admit_append(&transaction, &batch)?;
            for fact in &batch.facts {
                insert_fact(&transaction, &batch.session_id, fact)?;
            }
            let durable_seq = advance_watermark(&transaction, &batch)?;
            transaction.commit().map_err(sql_error)?;
            Ok(AppendCommit { durable_seq })
        })
        .await
    }

    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader> {
        let session_id = session_id.clone();
        self.with_connection(move |connection| {
            let projection = connection
                .query_row(
                    "SELECT length(CAST(header_json AS BLOB)),
                            CASE WHEN length(CAST(header_json AS BLOB)) <= ?2
                                 THEN header_json END
                     FROM sessions WHERE session_id = ?1",
                    params![
                        session_id.as_str(),
                        i64::try_from(MAXIMUM_SESSION_HEADER_BYTES)
                            .expect("session header bound fits SQLite INTEGER")
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
            decode_projected_json("session header", projection, MAXIMUM_SESSION_HEADER_BYTES)
        })
        .await
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage> {
        validate_read_limit(limit)?;
        let session_id = session_id.clone();
        self.with_connection(move |connection| {
            let durable_seq = connection
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            if after_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "Fact cursor exceeds the durable tail".into(),
                ));
            }
            let mut statement = connection
                .prepare(
                    "SELECT length(CAST(fact_json AS BLOB)),
                            CASE WHEN length(CAST(fact_json AS BLOB)) <= ?4
                                 THEN fact_json END
                     FROM facts
                     WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        session_id.as_str(),
                        sqlite_u64("Fact cursor", after_seq)?,
                        i64::try_from(limit)
                            .map_err(|_| StoreError::Invalid("read limit exceeds SQLite".into()))?,
                        i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                            .expect("session Fact bound fits SQLite INTEGER"),
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .map_err(sql_error)?;
            let mut facts = Vec::new();
            let mut encoded_bytes = 0_usize;
            for row in rows {
                let projection = row.map_err(sql_error)?;
                let fact: SessionFact =
                    decode_projected_json("session Fact", projection, MAXIMUM_SESSION_FACT_BYTES)?;
                let projected = encoded_bytes
                    .checked_add(fact.encoded_len())
                    .ok_or_else(|| StoreError::Corrupt("Fact page size overflow".into()))?;
                if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                    break;
                }
                encoded_bytes = projected;
                facts.push(fact);
            }
            let page = StoreFactPage {
                after_seq,
                facts,
                durable_seq,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage> {
        validate_read_limit(limit)?;
        let session_id = session_id.clone();
        let turn_id = turn_id.clone();
        self.with_connection(move |connection| {
            let durable_seq = connection
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            if after_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "turn Fact cursor exceeds the durable tail".into(),
                ));
            }
            let turn_exists = connection
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM turns WHERE session_id = ?1 AND turn_id = ?2
                     )",
                    params![session_id.as_str(), turn_id.as_str()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sql_error)?;
            if !turn_exists {
                return Err(StoreError::TurnNotFound {
                    session: session_id.to_string(),
                    turn: turn_id.to_string(),
                });
            }
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("turn read limit exceeds SQLite".into()))?;
            let mut statement = connection
                .prepare(
                    "SELECT length(CAST(fact_json AS BLOB)),
                            CASE WHEN length(CAST(fact_json AS BLOB)) <= ?5
                                 THEN fact_json END
                     FROM facts
                     WHERE session_id = ?1 AND turn_id = ?2 AND seq > ?3
                     ORDER BY seq LIMIT ?4",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        session_id.as_str(),
                        turn_id.as_str(),
                        sqlite_u64("turn Fact cursor", after_seq)?,
                        sqlite_limit,
                        i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                            .expect("session Fact bound fits SQLite INTEGER"),
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .map_err(sql_error)?;
            let mut facts = Vec::new();
            let mut encoded_bytes = 0_usize;
            let mut has_more = false;
            for row in rows {
                if facts.len() == limit {
                    has_more = true;
                    break;
                }
                let projection = row.map_err(sql_error)?;
                let fact: SessionFact =
                    decode_projected_json("session Fact", projection, MAXIMUM_SESSION_FACT_BYTES)?;
                let projected = encoded_bytes
                    .checked_add(fact.encoded_len())
                    .ok_or_else(|| StoreError::Corrupt("turn Fact page size overflow".into()))?;
                if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                    has_more = true;
                    break;
                }
                encoded_bytes = projected;
                facts.push(fact);
            }
            let page = StoreTurnFactPage {
                turn_id,
                after_seq,
                facts,
                durable_seq,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> Result<StoreOpenTurnPage> {
        validate_read_limit(limit)?;
        let session_id = session_id.clone();
        self.with_connection(move |connection| {
            let durable_seq = connection
                .query_row(
                    "SELECT durable_seq FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
                .and_then(|value| decode_u64("durable sequence", value))?;
            if after_accepted_seq > durable_seq {
                return Err(StoreError::Invalid(
                    "open-turn cursor exceeds the durable tail".into(),
                ));
            }
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("open-turn limit exceeds SQLite".into()))?;
            let mut statement = connection
                .prepare(
                    "SELECT turn_id, accepted_seq FROM turns
                     WHERE session_id = ?1 AND terminal_seq IS NULL
                       AND accepted_seq > ?2
                     ORDER BY accepted_seq LIMIT ?3",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map(
                    params![
                        session_id.as_str(),
                        sqlite_u64("open-turn cursor", after_accepted_seq)?,
                        sqlite_limit,
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(sql_error)?;
            let mut turns = Vec::with_capacity(limit + 1);
            for row in rows {
                let (turn_id, accepted_seq) = row.map_err(sql_error)?;
                turns.push(StoreOpenTurn {
                    turn_id: TurnId::new(turn_id)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    accepted_seq: decode_u64("turn acceptance sequence", accepted_seq)?,
                });
            }
            let has_more = turns.len() > limit;
            turns.truncate(limit);
            let page = StoreOpenTurnPage {
                after_accepted_seq,
                turns,
                durable_seq,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<rsi_agent_store_protocol::StoreSessionPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_connection(move |connection| {
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("session read limit exceeds SQLite".into()))?;
            let mut sessions = Vec::with_capacity(limit + 1);
            if let Some(after) = &after {
                let mut statement = connection
                    .prepare(
                        "SELECT session_id FROM sessions
                         WHERE session_id > ?1 ORDER BY session_id LIMIT ?2",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(params![after.as_str(), sqlite_limit], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(sql_error)?;
                for row in rows {
                    let value = row.map_err(sql_error)?;
                    sessions.push(
                        SessionId::new(value)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            } else {
                let mut statement = connection
                    .prepare("SELECT session_id FROM sessions ORDER BY session_id LIMIT ?1")
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map([sqlite_limit], |row| row.get::<_, String>(0))
                    .map_err(sql_error)?;
                for row in rows {
                    let value = row.map_err(sql_error)?;
                    sessions.push(
                        SessionId::new(value)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            }
            let has_more = sessions.len() > limit;
            sessions.truncate(limit);
            let page = rsi_agent_store_protocol::StoreSessionPage {
                after,
                sessions,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<rsi_agent_store_protocol::StoreSessionPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)?;
        let after = after.cloned();
        self.with_connection(move |connection| {
            let sqlite_limit = i64::try_from(limit + 1)
                .map_err(|_| StoreError::Invalid("session read limit exceeds SQLite".into()))?;
            let mut sessions = Vec::with_capacity(limit + 1);
            if let Some(after) = &after {
                let mut statement = connection
                    .prepare(
                        "SELECT DISTINCT session_id FROM turns
                         WHERE terminal_seq IS NULL AND session_id > ?1
                         ORDER BY session_id LIMIT ?2",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map(params![after.as_str(), sqlite_limit], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(sql_error)?;
                for row in rows {
                    sessions.push(
                        SessionId::new(row.map_err(sql_error)?)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            } else {
                let mut statement = connection
                    .prepare(
                        "SELECT DISTINCT session_id FROM turns
                         WHERE terminal_seq IS NULL
                         ORDER BY session_id LIMIT ?1",
                    )
                    .map_err(sql_error)?;
                let rows = statement
                    .query_map([sqlite_limit], |row| row.get::<_, String>(0))
                    .map_err(sql_error)?;
                for row in rows {
                    sessions.push(
                        SessionId::new(row.map_err(sql_error)?)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    );
                }
            }
            let has_more = sessions.len() > limit;
            sessions.truncate(limit);
            let page = rsi_agent_store_protocol::StoreSessionPage {
                after,
                sessions,
                has_more,
            };
            page.validate()?;
            Ok(page)
        })
        .await
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoredContextCheckpoint>> {
        let session_id = session_id.clone();
        self.with_connection(move |connection| {
            let exists = connection
                .query_row(
                    "SELECT 1 FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(sql_error)?
                .is_some();
            if !exists {
                return Err(StoreError::NotFound(session_id.to_string()));
            }
            let projection = connection
                .query_row(
                    "SELECT c.header_fingerprint, c.through_seq, c.fact_prefix_sha256,
                            length(c.checkpoint_bytes),
                            CASE WHEN length(c.checkpoint_bytes) <= ?2
                                 THEN c.checkpoint_bytes END,
                            length(CAST(s.header_json AS BLOB)),
                            CASE WHEN length(CAST(s.header_json AS BLOB)) <= ?3
                                 THEN s.header_json END,
                            s.durable_seq
                     FROM context_checkpoints c
                     JOIN sessions s ON s.session_id = c.session_id
                     WHERE c.session_id = ?1",
                    params![
                        session_id.as_str(),
                        i64::try_from(MAXIMUM_CONTEXT_CHECKPOINT_BYTES)
                            .expect("checkpoint bound fits SQLite INTEGER"),
                        i64::try_from(MAXIMUM_SESSION_HEADER_BYTES)
                            .expect("session header bound fits SQLite INTEGER"),
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Option<Vec<u8>>>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?;
            projection.map(decode_context_checkpoint).transpose()
        })
        .await
    }

    async fn write_context_checkpoint(&self, write: WriteContextCheckpoint) -> Result<()> {
        write.validate()?;
        self.with_connection(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let (actual, header_json, fact_prefix_sha256) = transaction
                .query_row(
                    "SELECT durable_seq, header_json, fact_prefix_sha256
                     FROM sessions WHERE session_id = ?1",
                    [write.session_id.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(write.session_id.to_string()))
                .and_then(|(value, header, digest)| {
                    Ok((decode_u64("durable sequence", value)?, header, digest))
                })?;
            if actual != write.expected_durable_seq {
                return Err(StoreError::Conflict {
                    expected: write.expected_durable_seq,
                    actual,
                });
            }
            let header: SessionHeader = decode_json("session header", &header_json)?;
            if write.checkpoint.header_fingerprint
                != header.fingerprint().map_err(|error| {
                    StoreError::Corrupt(format!("stored session header is invalid: {error}"))
                })?
            {
                return Err(StoreError::Invalid(
                    "checkpoint header fingerprint differs from the durable session".into(),
                ));
            }
            validate_sha256("Fact-prefix digest", &fact_prefix_sha256)?;
            if write.checkpoint.fact_prefix_sha256 != fact_prefix_sha256 {
                return Err(StoreError::Invalid(
                    "checkpoint Fact-prefix digest differs from the durable session".into(),
                ));
            }
            transaction
                .execute(
                    "INSERT INTO context_checkpoints
                         (session_id, header_fingerprint, through_seq,
                          fact_prefix_sha256, checkpoint_bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(session_id) DO UPDATE SET
                         header_fingerprint = excluded.header_fingerprint,
                         through_seq = excluded.through_seq,
                         fact_prefix_sha256 = excluded.fact_prefix_sha256,
                         checkpoint_bytes = excluded.checkpoint_bytes",
                    params![
                        write.session_id.as_str(),
                        write.checkpoint.header_fingerprint,
                        sqlite_u64("checkpoint sequence", write.checkpoint.through_seq)?,
                        write.checkpoint.fact_prefix_sha256,
                        write.checkpoint.bytes.as_ref(),
                    ],
                )
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)
        })
        .await
    }

    async fn put_cas(&self, bytes: Arc<[u8]>) -> Result<CasObjectRef> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_STORE_CAS_BYTES {
            return Err(StoreError::Invalid(
                "CAS bytes must be nonempty and bounded".into(),
            ));
        }
        let cas_dir = Arc::clone(&self.cas_dir);
        let cas_staging_dir = Arc::clone(&self.cas_staging_dir);
        let reference = self
            .with_cas(move || {
                let reference = CasObjectRef {
                    sha256: hex::encode(Sha256::digest(&bytes)),
                    byte_len: u64::try_from(bytes.len())
                        .map_err(|_| StoreError::Invalid("CAS length exceeds u64".into()))?,
                };
                reference.validate()?;
                install_cas(&cas_dir, &cas_staging_dir, &reference.sha256, &bytes)?;
                Ok(reference)
            })
            .await?;
        self.with_connection(move |connection| {
            if let Some(existing) = connection
                .query_row(
                    "SELECT byte_len FROM cas_objects WHERE sha256 = ?1",
                    [&reference.sha256],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
            {
                if decode_u64("CAS byte length", existing)? != reference.byte_len {
                    return Err(StoreError::Corrupt(
                        "CAS metadata conflicts with existing digest".into(),
                    ));
                }
            } else {
                connection
                    .execute(
                        "INSERT INTO cas_objects (sha256, byte_len) VALUES (?1, ?2)",
                        params![
                            &reference.sha256,
                            sqlite_u64("CAS byte length", reference.byte_len)?,
                        ],
                    )
                    .map_err(sql_error)?;
            }
            Ok(reference)
        })
        .await
    }

    async fn read_cas(&self, object: &CasObjectRef) -> Result<Arc<[u8]>> {
        object.validate()?;
        let object = object.clone();
        let verified = object.clone();
        self.with_connection(move |connection| {
            let byte_len = connection
                .query_row(
                    "SELECT byte_len FROM cas_objects WHERE sha256 = ?1",
                    [&object.sha256],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| StoreError::NotFound(object.sha256.clone()))?;
            if decode_u64("CAS byte length", byte_len)? != object.byte_len {
                return Err(StoreError::Corrupt(
                    "CAS reference disagrees with SQLite metadata".into(),
                ));
            }
            Ok(())
        })
        .await?;
        let cas_dir = Arc::clone(&self.cas_dir);
        self.with_cas(move || {
            let bytes = read_cas_file(&cas_dir, &verified.sha256)?;
            if bytes.len() as u64 != verified.byte_len {
                return Err(StoreError::Corrupt(
                    "CAS body length disagrees with metadata".into(),
                ));
            }
            Ok(Arc::from(bytes))
        })
        .await
    }
}

fn admit_append(transaction: &Transaction<'_>, batch: &AppendBatch) -> Result<()> {
    let existing = transaction
        .query_row(
            "SELECT durable_seq FROM sessions WHERE session_id = ?1",
            [batch.session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    let actual = existing
        .map(|value| decode_u64("durable_seq", value))
        .transpose()?
        .unwrap_or(0);
    if actual != batch.expected_seq {
        return Err(StoreError::Conflict {
            expected: batch.expected_seq,
            actual,
        });
    }
    match (existing, batch.header.as_ref()) {
        (None, Some(header)) => transaction
            .execute(
                "INSERT INTO sessions
                    (session_id, header_json, durable_seq, fact_prefix_sha256)
                 VALUES (?1, ?2, 0, ?3)",
                params![
                    batch.session_id.as_str(),
                    encode_json("session header", header)?,
                    hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                ],
            )
            .map(|_| ())
            .map_err(sql_error),
        (None, None) => Err(StoreError::NotFound(batch.session_id.as_str().into())),
        (Some(_), Some(_)) => Err(StoreError::Invalid(
            "existing session cannot replace its immutable header".into(),
        )),
        (Some(_), None) => Ok(()),
    }
}

fn insert_fact(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    fact: &SessionFact,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO facts (session_id, seq, turn_id, fact_kind, fact_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id.as_str(),
                sqlite_u64("fact sequence", fact.seq())?,
                fact.body().turn_id().as_str(),
                fact_index_kind(fact.body()),
                encode_json("session Fact", fact)?,
            ],
        )
        .map_err(sql_error)?;
    update_turn_index(transaction, session_id, fact)
}

fn update_turn_index(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    fact: &SessionFact,
) -> Result<()> {
    let changed = match fact.body() {
        SessionFactBody::TurnAccepted { turn_id, .. }
        | SessionFactBody::ImageRequested { turn_id, .. } => transaction
            .execute(
                "INSERT OR IGNORE INTO turns
                 (session_id, turn_id, accepted_seq, terminal_seq)
                 VALUES (?1, ?2, ?3, NULL)",
                params![
                    session_id.as_str(),
                    turn_id.as_str(),
                    sqlite_u64("turn acceptance sequence", fact.seq())?,
                ],
            )
            .map_err(sql_error)?,
        SessionFactBody::TurnTerminal { turn_id, .. } => transaction
            .execute(
                "UPDATE turns SET terminal_seq = ?1
                 WHERE session_id = ?2 AND turn_id = ?3 AND terminal_seq IS NULL",
                params![
                    sqlite_u64("turn terminal sequence", fact.seq())?,
                    session_id.as_str(),
                    turn_id.as_str(),
                ],
            )
            .map_err(sql_error)?,
        body => transaction
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM turns
                   WHERE session_id = ?1 AND turn_id = ?2 AND terminal_seq IS NULL
                 )",
                params![session_id.as_str(), body.turn_id().as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map(usize::from)
            .map_err(sql_error)?,
    };
    if changed == 1 {
        return Ok(());
    }
    let message = if matches!(fact.body(), SessionFactBody::TurnTerminal { .. }) {
        "terminal references a closed or unknown turn"
    } else if matches!(
        fact.body(),
        SessionFactBody::TurnAccepted { .. } | SessionFactBody::ImageRequested { .. }
    ) {
        "durable turn was accepted more than once"
    } else {
        "nonterminal Fact references a closed or unknown turn"
    };
    Err(StoreError::Corrupt(message.into()))
}

fn advance_watermark(transaction: &Transaction<'_>, batch: &AppendBatch) -> Result<u64> {
    let durable_seq = batch
        .facts
        .last()
        .expect("validated append is nonempty")
        .seq();
    let previous = transaction
        .query_row(
            "SELECT fact_prefix_sha256 FROM sessions WHERE session_id = ?1",
            [batch.session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_error)?;
    let mut fact_prefix_digest = decode_sha256("Fact-prefix digest", &previous)?;
    for fact in &batch.facts {
        fact_prefix_digest = advance_fact_prefix_digest(fact_prefix_digest, fact)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
    }
    let changed = transaction
        .execute(
            "UPDATE sessions SET durable_seq = ?1, fact_prefix_sha256 = ?2
             WHERE session_id = ?3 AND durable_seq = ?4",
            params![
                sqlite_u64("durable sequence", durable_seq)?,
                hex::encode(fact_prefix_digest),
                batch.session_id.as_str(),
                sqlite_u64("expected sequence", batch.expected_seq)?,
            ],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(StoreError::Corrupt(
            "SQLite lost a transaction-local append predicate".into(),
        ));
    }
    Ok(durable_seq)
}

fn prepare_root(path: &Path) -> Result<PathBuf> {
    reject_symlink_if_present(path, "Store root")?;
    create_private_directories(path)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::Invalid(
            "SQLite Agent Store root must be a real directory".into(),
        ));
    }
    set_directory_permissions(path)?;
    fs::canonicalize(path).map_err(io_error)
}

fn prepare_owned_directory(path: &Path, label: &str) -> Result<()> {
    reject_symlink_if_present(path, label)?;
    create_private_directories(path)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::Corrupt(format!(
            "{label} is not a real directory"
        )));
    }
    set_directory_permissions(path)?;
    Ok(())
}

fn prepare_cas_staging_directory(path: &Path) -> Result<()> {
    prepare_owned_directory(path, "CAS staging directory")?;
    for (index, entry) in fs::read_dir(path).map_err(io_error)?.enumerate() {
        if index >= MAXIMUM_ORPHANED_CAS_STAGING_FILES {
            return Err(StoreError::Corrupt(format!(
                "CAS staging directory exceeds {MAXIMUM_ORPHANED_CAS_STAGING_FILES} orphaned files"
            )));
        }
        let entry = entry.map_err(io_error)?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(StoreError::Corrupt(
                "CAS staging entry is not a real regular file".into(),
            ));
        }
        fs::remove_file(entry.path()).map_err(io_error)?;
    }
    sync_directory(path)
}

#[cfg(unix)]
fn create_private_directories(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).map_err(io_error)
}

#[cfg(not(unix))]
fn create_private_directories(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io_error)
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn reject_symlink_if_present(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::Invalid(format!(
            "{label} must not be a symbolic link"
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn acquire_writer_lock(root: &Path) -> Result<File> {
    let path = root.join(".writer.lock");
    reject_symlink_if_present(&path, "Store writer lock")?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(io_error)?;
    validate_open_file(&path, &file, "Store writer lock")?;
    if let Err(error) = file.try_lock() {
        let error: std::io::Error = error.into();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(StoreError::WriterLocked);
        }
        return Err(io_error(error));
    }
    validate_open_file(&path, &file, "Store writer lock")?;
    Ok(file)
}

fn validate_open_file(path: &Path, file: &File, label: &str) -> Result<()> {
    let path_metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let file_metadata = file.metadata().map_err(io_error)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !file_metadata.file_type().is_file()
    {
        return Err(StoreError::Corrupt(format!(
            "{label} is not a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(StoreError::Corrupt(format!(
                "{label} changed while opening"
            )));
        }
    }
    Ok(())
}

fn configure(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql_error)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;",
        )
        .map_err(sql_error)
}

fn initialize_or_validate_schema(connection: &mut Connection) -> Result<()> {
    let version = pragma_user_version(connection)?;
    let tables = user_tables(connection)?;
    if version == 0 && tables.is_empty() {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let mut schema = EXPECTED_TABLES
            .iter()
            .map(|(_, sql)| *sql)
            .collect::<Vec<_>>()
            .join(";\n");
        for (_, sql) in EXPECTED_INDEXES {
            schema.push_str(";\n");
            schema.push_str(sql);
        }
        write!(
            &mut schema,
            ";\nPRAGMA user_version = {AGENT_STORE_SCHEMA_VERSION};"
        )
        .expect("writing to a String cannot fail");
        transaction.execute_batch(&schema).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
    } else if version != AGENT_STORE_SCHEMA_VERSION {
        return Err(StoreError::SchemaMismatch {
            expected: AGENT_STORE_SCHEMA_VERSION,
            actual: version,
        });
    }
    validate_schema_shape(connection)
}

fn validate_schema_shape(connection: &Connection) -> Result<()> {
    let expected = BTreeSet::from([
        "cas_objects".to_owned(),
        "context_checkpoints".to_owned(),
        "facts".to_owned(),
        "sessions".to_owned(),
        "turns".to_owned(),
    ]);
    let actual = user_tables(connection)?;
    if actual != expected {
        return Err(StoreError::SchemaMismatch {
            expected: AGENT_STORE_SCHEMA_VERSION,
            actual: pragma_user_version(connection)?,
        });
    }
    for (table, expected_sql) in EXPECTED_TABLES {
        let observed_sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?;
        if normalize_schema_sql(&observed_sql) != normalize_schema_sql(expected_sql) {
            return Err(StoreError::Corrupt(format!(
                "SQLite table `{table}` does not match the exact schema"
            )));
        }
    }
    let expected_indexes = EXPECTED_INDEXES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    if user_indexes(connection)? != expected_indexes {
        return Err(StoreError::Corrupt(
            "SQLite schema contains missing or unexpected indexes".into(),
        ));
    }
    for (index, expected_sql) in EXPECTED_INDEXES {
        let observed_sql = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?;
        if normalize_schema_sql(&observed_sql) != normalize_schema_sql(expected_sql) {
            return Err(StoreError::Corrupt(format!(
                "SQLite index `{index}` does not match the exact schema"
            )));
        }
    }
    let unexpected_triggers_or_views = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('trigger', 'view')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql_error)?;
    if unexpected_triggers_or_views != 0 {
        return Err(StoreError::Corrupt(
            "SQLite schema contains unexpected triggers or views".into(),
        ));
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

fn pragma_user_version(connection: &Connection) -> Result<u32> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error)?;
    u32::try_from(version).map_err(|_| StoreError::Corrupt("negative user_version".into()))
}

fn user_tables(connection: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(sql_error)?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(sql_error)
}

fn user_indexes(connection: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(sql_error)?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(sql_error)
}

fn validate_database(connection: &Connection) -> Result<()> {
    let integrity = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(sql_error)?;
    if integrity != "ok" {
        return Err(StoreError::Corrupt(format!(
            "SQLite integrity_check returned {integrity:?}"
        )));
    }
    let foreign_key_failure = {
        let mut statement = connection
            .prepare("PRAGMA foreign_key_check")
            .map_err(sql_error)?;
        let mut rows = statement.query([]).map_err(sql_error)?;
        rows.next().map_err(sql_error)?.is_some()
    };
    if foreign_key_failure {
        return Err(StoreError::Corrupt(
            "SQLite foreign_key_check reported a violation".into(),
        ));
    }
    let invalid_watermark = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions AS session
               WHERE session.durable_seq != (
                 SELECT COUNT(*) FROM facts WHERE facts.session_id = session.session_id
               ) OR session.durable_seq != COALESCE((
                 SELECT MAX(seq) FROM facts WHERE facts.session_id = session.session_id
               ), 0)
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if invalid_watermark {
        return Err(StoreError::Corrupt(
            "session durable watermark differs from its contiguous Fact stream".into(),
        ));
    }
    let invalid_fact_prefix_digest = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions
               WHERE typeof(fact_prefix_sha256) != 'text'
                  OR length(fact_prefix_sha256) != 64
                  OR fact_prefix_sha256 GLOB '*[^0-9a-f]*'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if invalid_fact_prefix_digest {
        return Err(StoreError::Corrupt(
            "Fact-prefix digest is not lowercase SHA-256".into(),
        ));
    }
    let invalid_turn_index = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1
               FROM facts AS fact
               LEFT JOIN turns AS turn
                 ON turn.session_id = fact.session_id AND turn.turn_id = fact.turn_id
               WHERE turn.turn_id IS NULL
                  OR (fact.fact_kind = 'accepted' AND turn.accepted_seq != fact.seq)
                  OR (fact.fact_kind = 'terminal' AND turn.terminal_seq != fact.seq)
                  OR (fact.fact_kind = 'event' AND (
                       fact.seq <= turn.accepted_seq
                       OR (turn.terminal_seq IS NOT NULL AND fact.seq >= turn.terminal_seq)
                     ))
               UNION ALL
               SELECT 1
               FROM turns AS turn
               WHERE NOT EXISTS (
                       SELECT 1 FROM facts AS accepted
                       WHERE accepted.session_id = turn.session_id
                         AND accepted.seq = turn.accepted_seq
                         AND accepted.turn_id = turn.turn_id
                         AND accepted.fact_kind = 'accepted'
                     )
                  OR (turn.terminal_seq IS NOT NULL AND NOT EXISTS (
                       SELECT 1 FROM facts AS terminal
                       WHERE terminal.session_id = turn.session_id
                         AND terminal.seq = turn.terminal_seq
                         AND terminal.turn_id = turn.turn_id
                         AND terminal.fact_kind = 'terminal'
                     ))
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sql_error)?;
    if invalid_turn_index {
        return Err(StoreError::Corrupt(
            "turn index differs from the canonical Fact streams".into(),
        ));
    }
    Ok(())
}

fn install_cas(cas_dir: &Path, cas_staging_dir: &Path, sha256: &str, bytes: &[u8]) -> Result<()> {
    validate_digest(sha256, bytes)?;
    let target = cas_dir.join(sha256);
    if target.exists() {
        let existing = read_cas_file(cas_dir, sha256)?;
        if existing != bytes {
            return Err(StoreError::Corrupt(
                "existing CAS body conflicts with its digest name".into(),
            ));
        }
        return Ok(());
    }
    let temporary = cas_staging_dir.join(format!(
        ".{sha256}.{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error)?;
    let publish = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_regular_file_bounded(&target, MAXIMUM_STORE_CAS_BYTES)? != bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "existing CAS body differs from candidate",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        fs::remove_file(&temporary)?;
        sync_directory_io(cas_staging_dir)?;
        sync_directory_io(cas_dir)
    })();
    if let Err(error) = publish {
        let _ignored = fs::remove_file(&temporary);
        if target.exists() && read_cas_file(cas_dir, sha256)? == bytes {
            return Ok(());
        }
        return Err(io_error(error));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory_io(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory_io(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn read_cas_file(cas_dir: &Path, sha256: &str) -> Result<Vec<u8>> {
    validate_sha256("CAS identity", sha256)?;
    let path = cas_dir.join(sha256);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StoreError::NotFound(sha256.into())
        } else {
            io_error(error)
        }
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(StoreError::Corrupt(
            "CAS entry is not a regular file".into(),
        ));
    }
    let bytes = read_regular_file_bounded(&path, MAXIMUM_STORE_CAS_BYTES).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            StoreError::Corrupt(error.to_string())
        } else {
            io_error(error)
        }
    })?;
    validate_digest(sha256, &bytes)?;
    Ok(bytes)
}

fn read_regular_file_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CAS entry is not a regular file",
        ));
    }
    let file = File::open(path)?;
    if file.metadata()?.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("CAS entry exceeds {maximum_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("CAS entry exceeds {maximum_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

type ContextCheckpointProjection = (
    String,
    i64,
    String,
    i64,
    Option<Vec<u8>>,
    i64,
    Option<String>,
    i64,
);

fn decode_context_checkpoint(
    projection: ContextCheckpointProjection,
) -> Result<StoredContextCheckpoint> {
    let (
        header_fingerprint,
        through_seq,
        fact_prefix_sha256,
        encoded_len,
        bytes,
        header_encoded_len,
        header_json,
        durable_seq,
    ) = projection;
    let encoded_len = usize::try_from(encoded_len)
        .map_err(|_| StoreError::Corrupt("checkpoint length is invalid".into()))?;
    if encoded_len == 0 || encoded_len > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
        return Err(StoreError::Corrupt(
            "checkpoint bytes exceed their durable bound".into(),
        ));
    }
    let bytes = bytes.ok_or_else(|| {
        StoreError::Corrupt("bounded checkpoint projection returned no bytes".into())
    })?;
    if bytes.len() != encoded_len {
        return Err(StoreError::Corrupt(
            "checkpoint byte length changed during read".into(),
        ));
    }
    let checkpoint = StoredContextCheckpoint {
        header_fingerprint,
        through_seq: decode_u64("checkpoint sequence", through_seq)?,
        fact_prefix_sha256,
        bytes: Arc::from(bytes),
    };
    checkpoint
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    let header: SessionHeader = decode_projected_json(
        "session header",
        (header_encoded_len, header_json),
        MAXIMUM_SESSION_HEADER_BYTES,
    )?;
    let expected_fingerprint = header.fingerprint().map_err(|error| {
        StoreError::Corrupt(format!("stored session header is invalid: {error}"))
    })?;
    if checkpoint.header_fingerprint != expected_fingerprint {
        return Err(StoreError::Corrupt(
            "checkpoint header fingerprint differs from the durable session".into(),
        ));
    }
    if checkpoint.through_seq > decode_u64("durable sequence", durable_seq)? {
        return Err(StoreError::Corrupt(
            "checkpoint cursor exceeds the durable tail".into(),
        ));
    }
    Ok(checkpoint)
}

fn validate_sha256(label: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(StoreError::Corrupt(format!(
            "{label} is not lowercase SHA-256"
        )));
    }
    Ok(())
}

fn decode_sha256(label: &str, value: &str) -> Result<[u8; 32]> {
    validate_sha256(label, value)?;
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(value, &mut digest)
        .map_err(|error| StoreError::Corrupt(format!("cannot decode {label}: {error}")))?;
    Ok(digest)
}

fn validate_digest(sha256: &str, bytes: &[u8]) -> Result<()> {
    validate_sha256("CAS identity", sha256)?;
    if hex::encode(Sha256::digest(bytes)) != sha256 {
        return Err(StoreError::Corrupt(
            "CAS body does not match its digest".into(),
        ));
    }
    Ok(())
}

fn encode_json(label: &str, value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::Invalid(format!("cannot encode {label}: {error}")))
}

fn decode_json<T: serde::de::DeserializeOwned>(label: &str, json: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|error| StoreError::Corrupt(format!("invalid {label}: {error}")))
}

fn decode_projected_json<T: serde::de::DeserializeOwned>(
    label: &str,
    (encoded_len, json): (i64, Option<String>),
    maximum_bytes: usize,
) -> Result<T> {
    let encoded_len = usize::try_from(encoded_len)
        .map_err(|_| StoreError::Corrupt(format!("{label} has a negative byte length")))?;
    if encoded_len > maximum_bytes {
        return Err(StoreError::Corrupt(format!(
            "{label} exceeds {maximum_bytes} encoded bytes"
        )));
    }
    let json = json.ok_or_else(|| {
        StoreError::Corrupt(format!(
            "{label} is absent from its bounded SQLite projection"
        ))
    })?;
    if json.len() != encoded_len {
        return Err(StoreError::Corrupt(format!(
            "{label} byte length disagrees with its SQLite projection"
        )));
    }
    decode_json(label, &json)
}

fn sqlite_u64(label: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::Invalid(format!("{label} exceeds SQLite INTEGER")))
}

fn decode_u64(label: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt(format!("{label} is negative")))
}

const fn fact_index_kind(body: &SessionFactBody) -> &'static str {
    match body {
        SessionFactBody::TurnAccepted { .. } | SessionFactBody::ImageRequested { .. } => "accepted",
        SessionFactBody::TurnTerminal { .. } => "terminal",
        _ => "event",
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    let message = error.to_string();
    drop(error);
    StoreError::Io(format!("SQLite: {message}"))
}

fn io_error(error: std::io::Error) -> StoreError {
    let message = error.to_string();
    drop(error);
    StoreError::Io(format!("filesystem: {message}"))
}

/// Ordinary factory for one exact-root `SQLite` Agent Store.
#[derive(Clone, Debug, Default)]
pub struct SqliteStoreFactory;

fn store_config_retained_bytes(config: &SqliteStoreConfig) -> rsi_meta::Result<usize> {
    std::mem::size_of::<SqliteStoreConfig>()
        .checked_add(config.root.as_os_str().len())
        .ok_or_else(|| {
            MetaError::InvalidInput("SQLite Store retained byte count overflowed".into())
        })
}

#[async_trait]
impl PluginFactory for SqliteStoreFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: SqliteStoreConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = store_config_retained_bytes(&config)?;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<SqliteStoreConfig>()?;
        let store = tokio::task::spawn_blocking(move || SqliteStore::open(config.root))
            .await
            .map_err(|error| MetaError::Activation(format!("SQLite Store worker failed: {error}")))?
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let store: Arc<dyn SessionStore> = Arc::new(store);
        let supply = plan
            .context()
            .provide_local::<SessionStoreContract>(store)?;
        plan.defer(
            "withdraw SQLite Agent Store",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_store_charge_includes_inline_and_dynamic_config_state() {
        let config = SqliteStoreConfig {
            root: PathBuf::from("/tmp/rsi-agent-store"),
        };

        assert_eq!(
            store_config_retained_bytes(&config).unwrap(),
            std::mem::size_of::<SqliteStoreConfig>() + config.root.as_os_str().len()
        );
    }
}
