//! Exact-schema `SQLite` and filesystem-CAS Agent Store ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    ActivationId, AgentControlRecord, AgentControlRecordBody, AgentMessage, AgentMessageSource,
    EMPTY_CONTROL_PREFIX_DIGEST, EMPTY_FACT_PREFIX_DIGEST, ForkTurnSelection, InputMessageSource,
    MAXIMUM_DURABLE_AGENT_TREE_NODES, MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_SESSION_HEADER_BYTES,
    MessageDiscardReason, MessageId, MessageTarget, SessionFact, SessionFactBody, SessionHeader,
    SessionId, StepId, TurnId, advance_control_prefix_digest, advance_fact_prefix_digest,
};
use rsi_agent_store_protocol::{
    AGENT_STORE_SCHEMA_VERSION, AgentCommitWatermark, AppendBatch, AppendCommit, AtomicAgentCommit,
    AtomicAgentCommitResult, AtomicSessionAppend, CasObjectRef, MAXIMUM_CONTEXT_CHECKPOINT_BYTES,
    MAXIMUM_STORE_CAS_BYTES, MAXIMUM_STORE_CONTROL_PAGE_BYTES, MAXIMUM_STORE_FACT_PAGE_BYTES,
    MAXIMUM_STORE_MAILBOX_PAGE_BYTES, Result, SessionStore, SessionStoreContract,
    StoreActivationPhase, StoreActiveActivation, StoreAgentChild, StoreAgentChildPage,
    StoreAgentMailbox, StoreAgentMailboxSummary, StoreAgentMessage, StoreAgentMessageState,
    StoreBackwardFactPage, StoreControlPage, StoreDescendantControlSnapshot,
    StoreDescendantControlWatermark, StoreError, StoreFactPage, StoreFactTurnRole,
    StoreForkBoundary, StoreOpenTurn, StoreOpenTurnPage, StoreReadyMessage,
    StoreReadyMessageCursor, StoreReadyMessagePage, StoreReadyRootPage, StoreRecentSession,
    StoreRecentSessionCursor, StoreRecentSessionPage, StoreTurnBoundary, StoreTurnFactPage,
    StoreWaitingActivationPage, StoreWorkspaceContextState, StoredContextCheckpoint,
    WriteContextCheckpoint, validate_message_claim_fact, validate_read_limit,
    validate_session_read_limit,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAXIMUM_ORPHANED_CAS_STAGING_FILES: usize = 64;
const VALIDATED_SESSION_CACHE_CAPACITY: usize = 256;
const MAXIMUM_INDEXED_MESSAGE_STATE_BYTES: usize = 4 * 1024;
const EXPECTED_TABLES: [(&str, &str); 10] = [
    (
        "sessions",
        "CREATE TABLE sessions (
            session_id TEXT PRIMARY KEY NOT NULL,
            created_at_ms INTEGER NOT NULL CHECK (created_at_ms > 0),
            header_json TEXT NOT NULL,
            durable_seq INTEGER NOT NULL CHECK (durable_seq >= 0),
            fact_prefix_sha256 TEXT NOT NULL,
            control_seq INTEGER NOT NULL CHECK (control_seq >= 0),
            control_prefix_sha256 TEXT NOT NULL,
            workspace_instructions_sha256 TEXT,
            workspace_skill_catalog_sha256 TEXT
         ) STRICT",
    ),
    (
        "agent_nodes",
        "CREATE TABLE agent_nodes (
            session_id TEXT PRIMARY KEY NOT NULL
                REFERENCES sessions(session_id) ON DELETE RESTRICT,
            root_session_id TEXT NOT NULL
                REFERENCES sessions(session_id) ON DELETE RESTRICT,
            parent_session_id TEXT NOT NULL
                REFERENCES sessions(session_id) ON DELETE RESTRICT,
            path_json TEXT NOT NULL,
            task_name TEXT NOT NULL,
            UNIQUE (parent_session_id, task_name)
         ) STRICT",
    ),
    (
        "active_activations",
        "CREATE TABLE active_activations (
            session_id TEXT PRIMARY KEY NOT NULL
                REFERENCES sessions(session_id) ON DELETE RESTRICT,
            activation_id TEXT NOT NULL,
            parent_session_id TEXT,
            turn_id TEXT,
            phase TEXT NOT NULL CHECK (phase IN ('running', 'parked', 'waiting')),
            completion_reserved_bytes INTEGER CHECK (completion_reserved_bytes > 0)
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
            terminal_prefix_sha256 TEXT,
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
        "agent_controls",
        "CREATE TABLE agent_controls (
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE RESTRICT,
            seq INTEGER NOT NULL CHECK (seq > 0),
            control_json TEXT NOT NULL,
            PRIMARY KEY (session_id, seq)
         ) STRICT",
    ),
    (
        "agent_messages",
        "CREATE TABLE agent_messages (
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE RESTRICT,
            message_id TEXT NOT NULL,
            accepted_control_seq INTEGER NOT NULL CHECK (accepted_control_seq > 0),
            root_session_id TEXT NOT NULL,
            message_source TEXT NOT NULL CHECK (message_source IN ('human', 'agent', 'completion')),
            message_json TEXT NOT NULL,
            target TEXT NOT NULL CHECK (target IN ('next_turn', 'next_step')),
            wake_required INTEGER NOT NULL CHECK (wake_required IN (0, 1)),
            state TEXT NOT NULL CHECK (state IN ('pending', 'claimed', 'discarded')),
            state_json TEXT NOT NULL,
            PRIMARY KEY (session_id, message_id),
            UNIQUE (session_id, accepted_control_seq),
            FOREIGN KEY (session_id, accepted_control_seq)
                REFERENCES agent_controls(session_id, seq) ON DELETE RESTRICT
         ) STRICT",
    ),
    (
        "ready_messages",
        "CREATE TABLE ready_messages (
            root_session_id TEXT NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE RESTRICT,
            message_id TEXT NOT NULL,
            ready_control_seq INTEGER NOT NULL CHECK (ready_control_seq > 0),
            timestamp_ms INTEGER NOT NULL CHECK (timestamp_ms > 0),
            target TEXT NOT NULL CHECK (target = 'next_turn'),
            PRIMARY KEY (session_id, message_id),
            UNIQUE (session_id, ready_control_seq),
            FOREIGN KEY (session_id, ready_control_seq)
                REFERENCES agent_controls(session_id, seq) ON DELETE RESTRICT
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
const EXPECTED_INDEXES: [(&str, &str); 8] = [
    (
        "facts_by_turn",
        "CREATE INDEX facts_by_turn ON facts (session_id, turn_id, seq)",
    ),
    (
        "open_turns_by_session",
        "CREATE INDEX open_turns_by_session ON turns (session_id, accepted_seq)
         WHERE terminal_seq IS NULL",
    ),
    (
        "sessions_by_created_at",
        "CREATE INDEX sessions_by_created_at
         ON sessions (created_at_ms DESC, session_id DESC)",
    ),
    (
        "ready_messages_by_root",
        "CREATE INDEX ready_messages_by_root
         ON ready_messages (root_session_id, timestamp_ms, session_id, ready_control_seq)",
    ),
    (
        "agent_messages_pending",
        "CREATE INDEX agent_messages_pending
         ON agent_messages (session_id, accepted_control_seq)
         WHERE state = 'pending'",
    ),
    (
        "agent_nodes_by_parent",
        "CREATE INDEX agent_nodes_by_parent
         ON agent_nodes (parent_session_id, session_id)",
    ),
    (
        "agent_nodes_by_root_path",
        "CREATE UNIQUE INDEX agent_nodes_by_root_path
         ON agent_nodes (root_session_id, path_json)",
    ),
    (
        "active_activations_by_parent",
        "CREATE INDEX active_activations_by_parent
         ON active_activations (parent_session_id, session_id)
         WHERE completion_reserved_bytes IS NOT NULL",
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
    connections: Arc<DatabaseConnections>,
    writer_admission: Arc<Semaphore>,
    reader_admission: Arc<Semaphore>,
    validated_sessions: Arc<Mutex<ValidatedSessionCache>>,
    validation_gates: Arc<Mutex<BTreeMap<SessionId, Weak<AsyncMutex<()>>>>>,
    #[cfg(test)]
    validation_runs: Arc<AtomicU64>,
    cas_admission: Arc<Semaphore>,
    root: Arc<PathBuf>,
    cas_dir: Arc<PathBuf>,
    cas_staging_dir: Arc<PathBuf>,
    _writer_lock: Arc<File>,
}

struct DatabaseConnections {
    // Rust drops fields in declaration order. Keeping the reader first makes
    // the writer SQLite's last connection on clean shutdown, which checkpoints
    // and removes the WAL after all Store operations release this shared pair.
    reader: Mutex<Connection>,
    writer: Mutex<Connection>,
}

#[derive(Debug, Default)]
struct ValidatedSessionCache {
    recency: VecDeque<SessionId>,
}

impl ValidatedSessionCache {
    fn touch(&mut self, session_id: &SessionId) -> bool {
        let Some(index) = self
            .recency
            .iter()
            .position(|candidate| candidate == session_id)
        else {
            return false;
        };
        let session_id = self
            .recency
            .remove(index)
            .expect("located validated session is present");
        self.recency.push_back(session_id);
        true
    }

    fn insert(&mut self, session_id: SessionId) {
        if self.touch(&session_id) {
            return;
        }
        if self.recency.len() == VALIDATED_SESSION_CACHE_CAPACITY {
            self.recency.pop_front();
        }
        self.recency.push_back(session_id);
    }
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
    ///
    /// Opening validates the exact schema, owned paths, and writer exclusivity.
    /// First access validates the selected session's bounded Header, mechanical
    /// watermark, stored digest shape, and Fact/turn index relationships, then
    /// caches that proof with bounded recency. It does not decode every Fact or
    /// recompute the canonical prefix digest; use [`Self::verify`] for that
    /// explicit full audit.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = prepare_root(root.as_ref())?;
        let writer_lock = Arc::new(acquire_writer_lock(&root)?);
        let cas_dir = root.join("cas");
        prepare_owned_directory(&cas_dir, "CAS directory")?;
        let cas_staging_dir = cas_dir.join("staging");
        prepare_cas_staging_directory(&cas_staging_dir)?;
        let database_path = root.join("sessions.sqlite3");
        reject_symlink_if_present(&database_path, "SQLite database")?;
        let may_initialize = match fs::metadata(&database_path) {
            Ok(metadata) => metadata.len() == 0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(io_error(error)),
        };
        let mut writer_connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error)?;
        configure_writer(&writer_connection)?;
        initialize_or_validate_schema(&mut writer_connection, may_initialize)?;
        let reader_connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error)?;
        configure_reader(&reader_connection)?;
        Ok(Self {
            connections: Arc::new(DatabaseConnections {
                reader: Mutex::new(reader_connection),
                writer: Mutex::new(writer_connection),
            }),
            writer_admission: Arc::new(Semaphore::new(1)),
            reader_admission: Arc::new(Semaphore::new(1)),
            validated_sessions: Arc::new(Mutex::new(ValidatedSessionCache::default())),
            validation_gates: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(test)]
            validation_runs: Arc::new(AtomicU64::new(0)),
            cas_admission: Arc::new(Semaphore::new(1)),
            root: Arc::new(root),
            cas_dir: Arc::new(cas_dir),
            cas_staging_dir: Arc::new(cas_staging_dir),
            _writer_lock: writer_lock,
        })
    }

    /// Verifies an existing Store without creating any database or CAS path.
    ///
    /// The offline audit acquires the same writer lease as [`Self::open`] and
    /// checks the exact schema, `SQLite` integrity, foreign keys, every bounded
    /// Header and Fact, durable watermark, recomputed canonical Fact-prefix
    /// digest, and all turn-index relationships.
    pub fn verify(root: impl AsRef<Path>) -> Result<()> {
        let root = existing_root(root.as_ref())?;
        let _writer_lock = acquire_existing_writer_lock(&root)?;
        reject_uncheckpointed_wal(&root)?;
        let database_path = root.join("sessions.sqlite3");
        let metadata = fs::symlink_metadata(&database_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                StoreError::NotFound(database_path.display().to_string())
            } else {
                io_error(error)
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(StoreError::Corrupt(
                "SQLite database is not a regular file".into(),
            ));
        }
        let connection = open_verification_database(&database_path)?;
        configure_reader(&connection)?;
        let version = pragma_user_version(&connection)?;
        if version != AGENT_STORE_SCHEMA_VERSION {
            return Err(StoreError::SchemaMismatch {
                expected: AGENT_STORE_SCHEMA_VERSION,
                actual: version,
            });
        }
        validate_schema_shape(&connection)?;
        validate_database(&connection)
    }

    async fn with_database<T, F>(
        admission: Arc<Semaphore>,
        closed_message: &'static str,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = admission
            .acquire_owned()
            .await
            .map_err(|_| StoreError::Io(closed_message.into()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|error| StoreError::Io(format!("SQLite worker failed: {error}")))?
    }

    async fn with_writer<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let connections = Arc::clone(&self.connections);
        Self::with_database(
            Arc::clone(&self.writer_admission),
            "SQLite writer admission closed",
            move || {
                let mut connection = connections.writer.lock().map_err(|_| {
                    StoreError::Io("SQLite writer connection mutex was poisoned".into())
                })?;
                operation(&mut connection)
            },
        )
        .await
    }

    async fn with_reader<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let connections = Arc::clone(&self.connections);
        Self::with_database(
            Arc::clone(&self.reader_admission),
            "SQLite reader admission closed",
            move || {
                let mut connection = connections.reader.lock().map_err(|_| {
                    StoreError::Io("SQLite reader connection mutex was poisoned".into())
                })?;
                operation(&mut connection)
            },
        )
        .await
    }

    fn mark_session_validated(&self, session_id: SessionId) -> Result<()> {
        self.validated_sessions
            .lock()
            .map_err(|_| StoreError::Io("validated-session cache mutex was poisoned".into()))?
            .insert(session_id);
        Ok(())
    }

    fn touch_validated_session(&self, session_id: &SessionId) -> Result<bool> {
        Ok(self
            .validated_sessions
            .lock()
            .map_err(|_| StoreError::Io("validated-session cache mutex was poisoned".into()))?
            .touch(session_id))
    }

    fn validation_gate(&self, session_id: &SessionId) -> Result<Arc<AsyncMutex<()>>> {
        let mut gates = self
            .validation_gates
            .lock()
            .map_err(|_| StoreError::Io("session-validation gate mutex was poisoned".into()))?;
        gates.retain(|_, gate| gate.strong_count() != 0);
        if let Some(gate) = gates.get(session_id).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        let gate = Arc::new(AsyncMutex::new(()));
        gates.insert(session_id.clone(), Arc::downgrade(&gate));
        Ok(gate)
    }

    async fn ensure_session_validated(&self, session_id: &SessionId) -> Result<()> {
        if self.touch_validated_session(session_id)? {
            return Ok(());
        }
        let gate = self.validation_gate(session_id)?;
        let _gate = gate.lock().await;
        if self.touch_validated_session(session_id)? {
            return Ok(());
        }
        let candidate = session_id.clone();
        #[cfg(test)]
        self.validation_runs.fetch_add(1, Ordering::Relaxed);
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            validate_session(&transaction, &candidate)?;
            transaction.commit().map_err(sql_error)
        })
        .await?;
        self.mark_session_validated(session_id.clone())
    }

    async fn session_exists(&self, session_id: &SessionId) -> Result<bool> {
        let candidate = session_id.clone();
        self.with_reader(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
                    [candidate.as_str()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sql_error)
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

mod append;
mod cas;
mod filesystem;
mod session_store;
mod validation;

use append::{
    admit_append, advance_watermark, apply_atomic_sqlite_append, decode_indexed_message,
    decode_ready_message, derived_session_root, indexed_message_row, insert_fact,
    message_target_name, validate_sqlite_agent_guards,
};
use cas::{
    decode_context_checkpoint, decode_json, decode_projected_json, decode_sha256, decode_u64,
    encode_json, fact_index_kind, install_cas, io_error, read_cas_file, read_indexed_fact,
    sql_error, sqlite_u64, sync_directory, validate_sha256,
};
use filesystem::{
    acquire_existing_writer_lock, acquire_writer_lock, configure_reader, configure_writer,
    existing_root, open_verification_database, prepare_cas_staging_directory,
    prepare_owned_directory, prepare_root, reject_symlink_if_present, reject_uncheckpointed_wal,
};
use validation::{
    initialize_or_validate_schema, pragma_user_version, read_session_header_row, validate_database,
    validate_schema_shape, validate_session,
};

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
mod tests;
