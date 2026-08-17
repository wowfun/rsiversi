use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use rsi_meta_loader::{LoaderError, read_bounded_file_following_symlinks};

use crate::host::MAX_COMPOSITION_DOCUMENT_BYTES;
use crate::model::{DesiredState, GraphRevision, InstanceId};
use crate::protocol::{CommandOutcomeEnvelope, Event, EventEnvelope};
use crate::{HostError, Result};

mod operations;
mod schema;
mod state;

use schema::STORE_SCHEMA_SQL;
pub(crate) use state::{CasResult, PluginStateValue};
use state::{enforce_state_quotas, read_plugin_state};

pub(crate) const STORE_SCHEMA_VERSION: u32 = 2;
pub(crate) const OPERATION_RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;
pub(crate) const MAX_RETAINED_OPERATION_RESULTS: usize = 100_000;
pub(crate) const EVENT_RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;
pub(crate) const MAX_RETAINED_EVENTS: usize = 100_000;
pub(crate) const MAX_STATE_KEYS_PER_INSTANCE: usize = 4_096;
pub(crate) const MAX_STATE_TOMBSTONES_PER_INSTANCE: usize = 4_096;
pub(crate) const MAX_STATE_BYTES_PER_INSTANCE: usize = 16 * 1024 * 1024;
pub(crate) const MAX_STATE_KEYS_PER_COMPOSITION: usize = 16_384;
pub(crate) const MAX_STATE_BYTES_PER_COMPOSITION: usize = 64 * 1024 * 1024;
const MAX_DATABASE_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PendingEffect {
    Apply {
        requested_desired: DesiredState,
        graph_revision: GraphRevision,
    },
    Install {
        manifest_path: std::path::PathBuf,
        lock_path: std::path::PathBuf,
        manifest_bytes: Vec<u8>,
        lock_bytes: Vec<u8>,
        graph_revision: GraphRevision,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct UnjournaledCommand {
    pub command_id: String,
    pub effect: Option<PendingEffect>,
}

#[derive(Clone, Debug)]
pub(crate) enum StoredCommand {
    Pending {
        request_hash: Vec<u8>,
    },
    Terminal {
        request_hash: Vec<u8>,
        outcome: Box<CommandOutcomeEnvelope>,
    },
    Expired {
        request_hash: Vec<u8>,
        classification: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PendingApply {
    pub command_id: String,
    pub composition_id: String,
    pub installed_manifest_path: std::path::PathBuf,
    pub installed_lock_path: std::path::PathBuf,
    #[allow(dead_code)] // retained as durable provenance; recovery trusts installed hashes only
    pub candidate_manifest_path: std::path::PathBuf,
    #[allow(dead_code)] // retained as durable provenance; recovery trusts installed hashes only
    pub candidate_lock_path: std::path::PathBuf,
    pub candidate_manifest_hash: String,
    pub candidate_lock_hash: String,
    pub previous_manifest_bytes: Option<Vec<u8>>,
    pub previous_lock_bytes: Option<Vec<u8>>,
    pub previous_manifest_hash: Option<String>,
    pub previous_lock_hash: Option<String>,
    pub terminal_graph_revision: GraphRevision,
    pub terminal_event: Event,
    pub terminal_outcome: CommandOutcomeEnvelope,
    pub terminal_desired: DesiredState,
    pub operation_kind: String,
}

#[derive(Debug)]
pub(crate) struct Persistence {
    connection: Connection,
    maintenance_writes: u16,
    #[cfg(test)]
    fail_maintenance_after_commit: bool,
}

impl Persistence {
    pub(crate) fn open_leased(
        path: &Path,
        lease: &crate::workspace::WorkspaceLease,
    ) -> Result<Self> {
        let connection = Self::connect(path)?;
        lease.verify_opened_database(path)?;
        Self::initialize(connection)
    }

    #[cfg(test)]
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Self::initialize(Self::connect(path)?)
    }

    fn connect(path: &Path) -> Result<Connection> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| HostError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Connection::open(path).map_err(Into::into)
    }

    fn initialize(mut connection: Connection) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        let existing_version = existing_schema_version(&connection)?;
        if let Some(version) = existing_version
            && version != STORE_SCHEMA_VERSION
        {
            return Err(HostError::UnsupportedStoreSchema {
                found: version,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        connection.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "wal_autocheckpoint", 1_000)?;
        connection.pragma_update(None, "journal_size_limit", 16 * 1024 * 1024)?;
        let page_size: usize =
            connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        connection.pragma_update(
            None,
            "max_page_count",
            MAX_DATABASE_BYTES.div_ceil(page_size),
        )?;
        if existing_version.is_none() {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(STORE_SCHEMA_SQL)?;
            transaction.commit()?;
        }
        let mut persistence = Self {
            connection,
            maintenance_writes: 0,
            #[cfg(test)]
            fail_maintenance_after_commit: false,
        };
        persistence.compact()?;
        Ok(persistence)
    }

    pub(crate) fn latest_graph_revision(&self) -> Result<GraphRevision> {
        Ok(GraphRevision(read_meta_u64(
            &self.connection,
            "latest_graph_revision",
            "graph revision",
        )?))
    }

    pub(crate) fn latest_cursor(&self) -> Result<u64> {
        read_meta_u64(&self.connection, "latest_event_cursor", "event cursor")
    }

    pub(crate) fn token_generation(&self) -> Result<u64> {
        let value: String = self.connection.query_row(
            "SELECT value FROM store_meta WHERE key = 'token_generation'",
            [],
            |row| row.get(0),
        )?;
        value.parse().map_err(|_| {
            HostError::InvalidEnvelope("stored token generation is not a u64".to_owned())
        })
    }

    pub(crate) fn desired_state(&self) -> Result<DesiredState> {
        let value: String = self.connection.query_row(
            "SELECT value FROM store_meta WHERE key = 'desired_state'",
            [],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&value)?)
    }

    pub(crate) fn set_desired_state(&mut self, desired: &DesiredState) -> Result<()> {
        self.connection.execute(
            "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
            [serde_json::to_string(desired)?],
        )?;
        Ok(())
    }

    pub(crate) fn latest_composition_event(&self) -> Result<Option<EventEnvelope>> {
        let encoded: String = self.connection.query_row(
            "SELECT value FROM store_meta WHERE key = 'latest_composition_event'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        Ok(serde_json::from_str(&encoded)?)
    }

    pub(crate) fn find_command(&self, command_id: &str) -> Result<Option<StoredCommand>> {
        let stored = self
            .connection
            .query_row(
                "SELECT request_hash, status, outcome_json, terminal_classification
                 FROM command_outcome WHERE command_id = ?1",
                [command_id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        stored
            .map(
                |(request_hash, status, json, classification)| match status.as_str() {
                    "pending" => Ok(StoredCommand::Pending { request_hash }),
                    "terminal" => match json {
                        Some(json) => Ok(StoredCommand::Terminal {
                            request_hash,
                            outcome: Box::new(serde_json::from_str(&json)?),
                        }),
                        None => Err(HostError::InvalidEnvelope(
                            "terminal operation is missing its result".to_owned(),
                        )),
                    },
                    "expired" => Ok(StoredCommand::Expired {
                        request_hash,
                        classification: classification.unwrap_or_else(|| "unknown".to_owned()),
                    }),
                    other => Err(HostError::InvalidEnvelope(format!(
                        "unknown command outcome status {other:?}"
                    ))),
                },
            )
            .transpose()
    }

    fn reserve_pending(
        &mut self,
        composition_id: &str,
        command_id: &str,
        request_hash: &[u8],
        effect: &PendingEffect,
        desired: Option<&DesiredState>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO command_outcome(
               command_id, composition_id, request_hash, operation_kind, status, pending_json
             ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![
                command_id,
                composition_id,
                request_hash,
                match effect {
                    PendingEffect::Apply { .. } => "apply",
                    PendingEffect::Install { .. } => "install",
                },
                serde_json::to_string(effect)?
            ],
        )?;
        if let Some(desired) = desired {
            transaction.execute(
                "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
                [serde_json::to_string(desired)?],
            )?;
        }
        transaction.commit()?;
        self.note_retained_write();
        Ok(())
    }

    pub(crate) fn finish_pending_outcome(
        &mut self,
        command_id: &str,
        outcome: &CommandOutcomeEnvelope,
        desired: Option<&DesiredState>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE command_outcome
             SET status = 'terminal', outcome_json = ?1, terminal_classification = ?2,
                 completed_at = unixepoch(), expires_at = unixepoch() + ?3,
                 pending_json = NULL
             WHERE command_id = ?4 AND status = 'pending'",
            params![
                serde_json::to_string(outcome)?,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
                command_id,
            ],
        )?;
        if updated != 1 {
            return Err(HostError::InvalidEnvelope(format!(
                "reserved command {command_id:?} is not pending"
            )));
        }
        if let Some(desired) = desired {
            transaction.execute(
                "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
                [serde_json::to_string(desired)?],
            )?;
        }
        transaction.commit()?;
        self.note_retained_write();
        Ok(())
    }

    pub(crate) fn unjournaled_commands(&self) -> Result<Vec<UnjournaledCommand>> {
        let mut statement = self.connection.prepare(
            "SELECT o.command_id, o.pending_json
             FROM command_outcome o
             LEFT JOIN apply_journal j ON j.command_id = o.command_id
             WHERE o.status = 'pending' AND j.command_id IS NULL
             ORDER BY o.created_at ASC, o.command_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut commands = Vec::new();
        for row in rows {
            let (command_id, pending_json) = row?;
            let effect = pending_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?;
            commands.push(UnjournaledCommand { command_id, effect });
        }
        Ok(commands)
    }

    pub(crate) fn store_outcome(
        &mut self,
        composition_id: &str,
        command_id: &str,
        request_hash: &[u8],
        outcome: &CommandOutcomeEnvelope,
    ) -> Result<()> {
        let json = serde_json::to_string(outcome)?;
        self.connection.execute(
            "INSERT INTO command_outcome(
               command_id, composition_id, request_hash, operation_kind, status, outcome_json,
               terminal_classification, completed_at, expires_at
             ) VALUES (?1, ?2, ?3, 'legacy', 'terminal', ?4, ?5, unixepoch(), unixepoch() + ?6)",
            params![
                command_id,
                composition_id,
                request_hash,
                json,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
            ],
        )?;
        self.note_retained_write();
        Ok(())
    }

    pub(crate) fn store_operation_outcome(
        &mut self,
        composition_id: &str,
        operation_id: &str,
        operation_kind: &str,
        request_hash: &[u8],
        outcome: &CommandOutcomeEnvelope,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO command_outcome(
               command_id, composition_id, request_hash, operation_kind, status, outcome_json,
               terminal_classification, completed_at, expires_at
             ) VALUES (?1, ?2, ?3, ?4, 'terminal', ?5, ?6, unixepoch(), unixepoch() + ?7)",
            params![
                operation_id,
                composition_id,
                request_hash,
                operation_kind,
                serde_json::to_string(outcome)?,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
            ],
        )?;
        self.note_retained_write();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reserve_install(
        &mut self,
        composition_id: &str,
        operation_id: &str,
        request_hash: &[u8],
        manifest_path: &Path,
        lock_path: &Path,
        manifest_bytes: &[u8],
        lock_bytes: &[u8],
        graph_revision: GraphRevision,
    ) -> Result<()> {
        self.reserve_pending(
            composition_id,
            operation_id,
            request_hash,
            &PendingEffect::Install {
                manifest_path: manifest_path.to_owned(),
                lock_path: lock_path.to_owned(),
                manifest_bytes: manifest_bytes.to_vec(),
                lock_bytes: lock_bytes.to_vec(),
                graph_revision,
            },
            None,
        )
    }

    pub(crate) fn allocate_token_generation(
        &mut self,
        composition_id: &str,
        command_id: &str,
        request_hash: &[u8],
        graph_revision: GraphRevision,
    ) -> Result<CommandOutcomeEnvelope> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: String = transaction.query_row(
            "SELECT value FROM store_meta WHERE key = 'token_generation'",
            [],
            |row| row.get(0),
        )?;
        let current = current.parse::<u64>().map_err(|_| {
            HostError::InvalidEnvelope("stored token generation is not a u64".to_owned())
        })?;
        let generation = current.checked_add(1).ok_or_else(|| {
            HostError::InvalidEnvelope("token generation exhausted u64".to_owned())
        })?;
        transaction.execute(
            "UPDATE store_meta SET value = ?1 WHERE key = 'token_generation'",
            [generation.to_string()],
        )?;
        let outcome = CommandOutcomeEnvelope::token_rotated(
            command_id.to_owned(),
            graph_revision,
            generation,
        );
        transaction.execute(
            "INSERT INTO command_outcome(
               command_id, composition_id, request_hash, operation_kind, status, outcome_json,
               terminal_classification, completed_at, expires_at
             ) VALUES (?1, ?2, ?3, 'token_rotation', 'terminal', ?4, ?5,
                       unixepoch(), unixepoch() + ?6)",
            params![
                command_id,
                composition_id,
                request_hash,
                serde_json::to_string(&outcome)?,
                outcome_classification(&outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
            ],
        )?;
        transaction.commit()?;
        self.note_retained_write();
        Ok(outcome)
    }

    pub(crate) fn commit_event_and_outcome(
        &mut self,
        composition_id: &str,
        command_id: &str,
        request_hash: &[u8],
        graph_revision: GraphRevision,
        event: Event,
        outcome: &CommandOutcomeEnvelope,
    ) -> Result<EventEnvelope> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO control_event(
               composition_id, command_id, graph_revision, event_json
             ) VALUES (?1, ?2, ?3, '{}')",
            params![
                composition_id,
                command_id,
                to_i64(graph_revision.0, "graph revision")?
            ],
        )?;
        let cursor = to_u64(transaction.last_insert_rowid(), "event cursor")?;
        let envelope = EventEnvelope::new(command_id, cursor, graph_revision, event);
        transaction.execute(
            "UPDATE control_event SET event_json = ?1 WHERE cursor = ?2",
            params![
                serde_json::to_string(&envelope)?,
                to_i64(cursor, "event cursor")?
            ],
        )?;
        record_event_metadata(&transaction, &envelope)?;
        transaction.execute(
            "INSERT INTO command_outcome(
               command_id, composition_id, request_hash, operation_kind, status, outcome_json,
               terminal_classification, completed_at, expires_at
             ) VALUES (?1, ?2, ?3, 'shutdown', 'terminal', ?4, ?5,
                       unixepoch(), unixepoch() + ?6)",
            params![
                command_id,
                composition_id,
                request_hash,
                serde_json::to_string(outcome)?,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
            ],
        )?;
        transaction.commit()?;
        self.note_retained_write();
        Ok(envelope)
    }

    pub(crate) fn append_event(
        &mut self,
        composition_id: &str,
        command_id: &str,
        graph_revision: GraphRevision,
        event: Event,
    ) -> Result<EventEnvelope> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO control_event(
               composition_id, command_id, graph_revision, event_json
             ) VALUES (?1, ?2, ?3, '{}')",
            params![
                composition_id,
                command_id,
                to_i64(graph_revision.0, "graph revision")?
            ],
        )?;
        let cursor = to_u64(transaction.last_insert_rowid(), "event cursor")?;
        let envelope = EventEnvelope::new(command_id, cursor, graph_revision, event);
        transaction.execute(
            "UPDATE control_event SET event_json = ?1 WHERE cursor = ?2",
            params![
                serde_json::to_string(&envelope)?,
                to_i64(cursor, "event cursor")?
            ],
        )?;
        record_event_metadata(&transaction, &envelope)?;
        transaction.commit()?;
        self.note_retained_write();
        Ok(envelope)
    }

    pub(crate) fn query_events_through(
        &self,
        after_cursor: u64,
        through_cursor: u64,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>> {
        let minimum_available: String = self.connection.query_row(
            "SELECT value FROM store_meta WHERE key = 'minimum_event_cursor'",
            [],
            |row| row.get(0),
        )?;
        let minimum_available = minimum_available.parse::<u64>().map_err(|_| {
            HostError::InvalidEnvelope("stored minimum event cursor is not a u64".to_owned())
        })?;
        if after_cursor < minimum_available {
            return Err(HostError::EventCursorExpired {
                requested: after_cursor,
                minimum_available,
            });
        }
        if after_cursor > i64::MAX as u64 {
            return Ok(Vec::new());
        }
        let through_cursor = through_cursor.min(i64::MAX as u64);
        if after_cursor >= through_cursor {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT event_json FROM control_event
             WHERE cursor > ?1 AND cursor <= ?2 ORDER BY cursor ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                to_i64(after_cursor, "event cursor")?,
                to_i64(through_cursor, "event replay boundary")?,
                i64::from(limit.clamp(1, 10_000))
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        Ok(events)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_apply(
        &mut self,
        operation_kind: &'static str,
        command_id: &str,
        composition_id: &str,
        installed_manifest_path: &Path,
        installed_lock_path: &Path,
        candidate_manifest_path: &Path,
        candidate_lock_path: &Path,
        candidate_manifest_hash: &str,
        candidate_lock_hash: &str,
        terminal_graph_revision: GraphRevision,
        terminal_event: &Event,
        terminal_outcome: &CommandOutcomeEnvelope,
        terminal_desired: &DesiredState,
    ) -> Result<()> {
        let manifest_path = utf8_path(installed_manifest_path)?;
        let lock_path = utf8_path(installed_lock_path)?;
        let candidate_manifest_path = utf8_path(candidate_manifest_path)?;
        let candidate_lock_path = utf8_path(candidate_lock_path)?;
        let previous_manifest_bytes = read_optional(installed_manifest_path)?;
        let previous_lock_bytes = read_optional(installed_lock_path)?;
        let previous_manifest_hash = previous_manifest_bytes
            .as_ref()
            .map(rsi_meta_loader::ContentHash::digest)
            .map(|hash| hash.to_string());
        let previous_lock_hash = previous_lock_bytes
            .as_ref()
            .map(rsi_meta_loader::ContentHash::digest)
            .map(|hash| hash.to_string());
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO apply_journal(
               command_id, composition_id, installed_manifest_path, installed_lock_path,
               candidate_manifest_path, candidate_lock_path,
               candidate_manifest_hash, candidate_lock_hash,
               previous_manifest_bytes, previous_lock_bytes,
               previous_manifest_hash, previous_lock_hash,
               terminal_graph_revision, terminal_event_json,
               terminal_outcome_json, terminal_desired_json, operation_kind
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
               ?13, ?14, ?15, ?16, ?17
             )",
            params![
                command_id,
                composition_id,
                manifest_path,
                lock_path,
                candidate_manifest_path,
                candidate_lock_path,
                candidate_manifest_hash,
                candidate_lock_hash,
                previous_manifest_bytes,
                previous_lock_bytes,
                previous_manifest_hash,
                previous_lock_hash,
                to_i64(terminal_graph_revision.0, "graph revision")?,
                serde_json::to_string(terminal_event)?,
                serde_json::to_string(terminal_outcome)?,
                serde_json::to_string(terminal_desired)?,
                operation_kind,
            ],
        )?;
        let pending: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM command_outcome
             WHERE command_id = ?1 AND status = 'pending'",
            [command_id],
            |row| row.get(0),
        )?;
        if pending != 1 {
            return Err(HostError::InvalidEnvelope(format!(
                "apply journal {command_id:?} has no reserved pending command"
            )));
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn commit_pending_apply(
        &mut self,
        command_id: &str,
        graph_revision: GraphRevision,
        event: Event,
        outcome: &CommandOutcomeEnvelope,
        desired: &DesiredState,
    ) -> Result<EventEnvelope> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let composition_id: String = transaction.query_row(
            "SELECT composition_id FROM apply_journal
             WHERE command_id = ?1",
            [command_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO control_event(
               composition_id, command_id, graph_revision, event_json
             ) VALUES (?1, ?2, ?3, '{}')",
            params![
                composition_id,
                command_id,
                to_i64(graph_revision.0, "graph revision")?
            ],
        )?;
        let cursor = to_u64(transaction.last_insert_rowid(), "event cursor")?;
        let envelope = EventEnvelope::new(command_id, cursor, graph_revision, event);
        transaction.execute(
            "UPDATE control_event SET event_json = ?1 WHERE cursor = ?2",
            params![
                serde_json::to_string(&envelope)?,
                to_i64(cursor, "event cursor")?
            ],
        )?;
        record_event_metadata(&transaction, &envelope)?;
        let updated = transaction.execute(
            "UPDATE command_outcome
             SET status = 'terminal', outcome_json = ?1, terminal_classification = ?2,
                 completed_at = unixepoch(), expires_at = unixepoch() + ?3,
                 pending_json = NULL
             WHERE command_id = ?4 AND status = 'pending'",
            params![
                serde_json::to_string(outcome)?,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
                command_id,
            ],
        )?;
        if updated != 1 {
            return Err(HostError::InvalidEnvelope(format!(
                "pending command outcome {command_id:?} disappeared"
            )));
        }
        transaction.execute(
            "DELETE FROM apply_journal WHERE command_id = ?1",
            [command_id],
        )?;
        transaction.execute(
            "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
            [serde_json::to_string(desired)?],
        )?;
        transaction.commit()?;
        self.note_retained_write();
        Ok(envelope)
    }

    pub(crate) fn commit_pending_install(
        &mut self,
        operation_id: &str,
        outcome: &CommandOutcomeEnvelope,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE command_outcome
             SET status = 'terminal', outcome_json = ?1, terminal_classification = ?2,
                 completed_at = unixepoch(), expires_at = unixepoch() + ?3,
                 pending_json = NULL
             WHERE command_id = ?4 AND status = 'pending' AND operation_kind = 'install'",
            params![
                serde_json::to_string(outcome)?,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
                operation_id,
            ],
        )?;
        if updated != 1 {
            return Err(HostError::InvalidEnvelope(format!(
                "pending install operation {operation_id:?} disappeared"
            )));
        }
        transaction.execute(
            "DELETE FROM apply_journal WHERE command_id = ?1 AND operation_kind = 'install'",
            [operation_id],
        )?;
        transaction.commit()?;
        self.note_retained_write();
        Ok(())
    }

    pub(crate) fn abort_pending_apply(
        &mut self,
        command_id: &str,
        outcome: &CommandOutcomeEnvelope,
        desired: &DesiredState,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE command_outcome
             SET status = 'terminal', outcome_json = ?1, terminal_classification = ?2,
                 completed_at = unixepoch(), expires_at = unixepoch() + ?3,
                 pending_json = NULL
             WHERE command_id = ?4 AND status = 'pending'",
            params![
                serde_json::to_string(outcome)?,
                outcome_classification(outcome),
                to_i64(OPERATION_RETENTION_SECONDS, "operation retention")?,
                command_id,
            ],
        )?;
        transaction.execute(
            "DELETE FROM apply_journal WHERE command_id = ?1",
            [command_id],
        )?;
        transaction.execute(
            "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
            [serde_json::to_string(desired)?],
        )?;
        transaction.commit()?;
        self.note_retained_write();
        Ok(())
    }

    pub(crate) fn pending_applies(&self) -> Result<Vec<PendingApply>> {
        let mut statement = self.connection.prepare(
            "SELECT j.command_id, j.composition_id,
                    j.installed_manifest_path, j.installed_lock_path,
                    j.candidate_manifest_path, j.candidate_lock_path,
                    j.candidate_manifest_hash, j.candidate_lock_hash,
                    j.previous_manifest_bytes, j.previous_lock_bytes,
                    j.previous_manifest_hash, j.previous_lock_hash,
                    j.terminal_graph_revision, j.terminal_event_json,
                    j.terminal_outcome_json, j.terminal_desired_json,
                    j.operation_kind
             FROM apply_journal j
             JOIN command_outcome o ON o.command_id = j.command_id
             WHERE o.status = 'pending'
             ORDER BY j.created_at ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PendingApply {
                command_id: row.get(0)?,
                composition_id: row.get(1)?,
                installed_manifest_path: std::path::PathBuf::from(row.get::<_, String>(2)?),
                installed_lock_path: std::path::PathBuf::from(row.get::<_, String>(3)?),
                candidate_manifest_path: std::path::PathBuf::from(row.get::<_, String>(4)?),
                candidate_lock_path: std::path::PathBuf::from(row.get::<_, String>(5)?),
                candidate_manifest_hash: row.get(6)?,
                candidate_lock_hash: row.get(7)?,
                previous_manifest_bytes: row.get(8)?,
                previous_lock_bytes: row.get(9)?,
                previous_manifest_hash: row.get(10)?,
                previous_lock_hash: row.get(11)?,
                terminal_graph_revision: {
                    let revision = row.get::<_, i64>(12)?;
                    GraphRevision(
                        u64::try_from(revision)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(12, revision))?,
                    )
                },
                terminal_event: serde_json::from_str(&row.get::<_, String>(13)?).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            13,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    },
                )?,
                terminal_outcome: serde_json::from_str(&row.get::<_, String>(14)?).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            14,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    },
                )?,
                terminal_desired: serde_json::from_str(&row.get::<_, String>(15)?).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            15,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    },
                )?,
                operation_kind: row.get(16)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(HostError::from)
    }

    fn note_retained_write(&mut self) {
        self.maintenance_writes = self.maintenance_writes.saturating_add(1);
        if self.maintenance_writes < 256 {
            return;
        }
        if let Err(error) = self.compact() {
            tracing::warn!(%error, "post-commit SQLite maintenance failed");
        }
        self.maintenance_writes = 0;
    }

    pub(crate) fn compact(&mut self) -> Result<()> {
        let now: i64 = self
            .connection
            .query_row("SELECT unixepoch()", [], |row| row.get(0))?;
        self.compact_at(now, MAX_RETAINED_OPERATION_RESULTS, MAX_RETAINED_EVENTS)
    }

    fn compact_at(&mut self, now: i64, operation_limit: usize, event_limit: usize) -> Result<()> {
        let operation_limit = i64::try_from(operation_limit).map_err(|_| {
            HostError::InvalidEnvelope("operation retention limit exceeds i64".to_owned())
        })?;
        let event_limit = u64::try_from(event_limit).map_err(|_| {
            HostError::InvalidEnvelope("event retention limit exceeds u64".to_owned())
        })?;
        let operation_cutoff =
            now.saturating_sub(i64::try_from(OPERATION_RETENTION_SECONDS).unwrap_or(i64::MAX));
        let event_cutoff =
            now.saturating_sub(i64::try_from(EVENT_RETENTION_SECONDS).unwrap_or(i64::MAX));
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE command_outcome
             SET status = 'expired', outcome_json = NULL, pending_json = NULL
             WHERE status = 'terminal' AND (
                 completed_at < ?1 OR command_id IN (
                     SELECT command_id FROM command_outcome
                     WHERE status = 'terminal'
                     ORDER BY completed_at DESC, command_id DESC
                     LIMIT -1 OFFSET ?2
                 )
             )",
            params![operation_cutoff, operation_limit],
        )?;
        let expired_by_age: Option<i64> = transaction.query_row(
            "SELECT MAX(cursor)
             FROM control_event INDEXED BY control_event_created
             WHERE created_at < ?1",
            [event_cutoff],
            |row| row.get(0),
        )?;
        let expired_by_age = expired_by_age
            .map(|cursor| to_u64(cursor, "event retention boundary"))
            .transpose()?
            .unwrap_or(0);
        let latest_cursor = read_meta_u64(&transaction, "latest_event_cursor", "event cursor")?;
        if expired_by_age > latest_cursor {
            return Err(HostError::InvalidEnvelope(
                "event cursor highwater is behind retained rows".to_owned(),
            ));
        }
        let deleted_through = expired_by_age.max(latest_cursor.saturating_sub(event_limit));
        if deleted_through != 0 {
            let deleted_through = to_i64(deleted_through, "event retention boundary")?;
            transaction.execute(
                "DELETE FROM control_event WHERE cursor <= ?1",
                [deleted_through],
            )?;
            transaction.execute(
                "UPDATE store_meta
                 SET value = CAST(MAX(CAST(value AS INTEGER), ?1) AS TEXT)
                 WHERE key = 'minimum_event_cursor'",
                [deleted_through],
            )?;
        }
        transaction.commit()?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_maintenance_after_commit) {
            return Err(HostError::InvalidEnvelope(
                "injected maintenance failure after compaction commit".to_owned(),
            ));
        }
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA incremental_vacuum(256);")?;
        Ok(())
    }

    pub(crate) fn get_plugin_state(
        &self,
        composition_id: &str,
        instance_id: &InstanceId,
        key: &str,
    ) -> Result<Option<PluginStateValue>> {
        read_plugin_state(&self.connection, composition_id, instance_id, key)
    }

    pub(crate) fn compare_and_swap_plugin_state(
        &mut self,
        composition_id: &str,
        instance_id: &InstanceId,
        key: &str,
        expected_version: Option<u64>,
        value: Option<&serde_json::Value>,
    ) -> Result<CasResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_plugin_state(&transaction, composition_id, instance_id, key)?;
        if current.as_ref().map(|entry| entry.version) != expected_version {
            transaction.rollback()?;
            return Ok(CasResult::Conflict(current));
        }
        let next_version = expected_version
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                HostError::InvalidEnvelope("plugin state version exhausted u64".to_owned())
            })?;
        let value_json = value.map(serde_json::to_string).transpose()?;
        enforce_state_quotas(
            &transaction,
            composition_id,
            instance_id,
            key,
            value_json.as_ref().map_or(0, String::len),
            value.is_none(),
        )?;
        transaction.execute(
            "INSERT INTO plugin_state(
               composition_id, instance_id, state_key, version, value_json, tombstone
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(composition_id, instance_id, state_key) DO UPDATE SET
               version = excluded.version,
               value_json = excluded.value_json,
               tombstone = excluded.tombstone,
               updated_at = unixepoch()",
            params![
                composition_id,
                instance_id.0,
                key,
                to_i64(next_version, "plugin state version")?,
                value_json,
                i64::from(value.is_none())
            ],
        )?;
        transaction.commit()?;
        Ok(CasResult::Applied(PluginStateValue {
            version: next_version,
            value: value.cloned(),
        }))
    }

    #[cfg(test)]
    pub(crate) fn journal_mode(&self) -> Result<String> {
        Ok(self
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?)
    }
}

fn outcome_classification(outcome: &CommandOutcomeEnvelope) -> &'static str {
    use crate::protocol::CommandOutcome;

    match &outcome.payload {
        CommandOutcome::Applied => "applied",
        CommandOutcome::NoChange => "no_change",
        CommandOutcome::TokenRotated { .. } => "token_rotated",
        CommandOutcome::RestartRequired { .. } => "restart_required",
        CommandOutcome::Installed { .. } => "installed",
        CommandOutcome::Rejected { .. } => "rejected",
        CommandOutcome::ShuttingDown => "shutting_down",
    }
}

fn existing_schema_version(connection: &Connection) -> Result<Option<u32>> {
    let has_meta = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'store_meta'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_meta {
        return Ok(None);
    }
    let version = connection
        .query_row(
            "SELECT value FROM store_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let version = version.ok_or_else(|| {
        HostError::InvalidEnvelope(
            "store_meta exists without an authoritative schema_version".to_owned(),
        )
    })?;
    version
        .parse::<u32>()
        .map(Some)
        .map_err(|_| HostError::InvalidEnvelope("stored schema version is not a u32".to_owned()))
}

fn read_meta_u64(connection: &Connection, key: &str, label: &'static str) -> Result<u64> {
    let value: String = connection.query_row(
        "SELECT value FROM store_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )?;
    value
        .parse()
        .map_err(|_| HostError::InvalidEnvelope(format!("stored {label} is not a u64")))
}

fn record_event_metadata(transaction: &Transaction<'_>, envelope: &EventEnvelope) -> Result<()> {
    let cursor = envelope.cursor.to_string();
    let graph_revision = envelope.graph_revision.0.to_string();
    let composition = matches!(&envelope.payload, Event::CompositionCommitted { .. })
        .then(|| serde_json::to_string(envelope))
        .transpose()?;
    let updated = if let Some(composition) = composition {
        transaction.execute(
            "UPDATE store_meta SET value = CASE key
                 WHEN 'latest_event_cursor' THEN ?1
                 WHEN 'latest_graph_revision' THEN ?2
                 WHEN 'latest_composition_event' THEN ?3
             END
             WHERE key IN (
                 'latest_event_cursor', 'latest_graph_revision', 'latest_composition_event'
             )",
            params![cursor, graph_revision, composition],
        )?
    } else {
        transaction.execute(
            "UPDATE store_meta SET value = CASE key
                 WHEN 'latest_event_cursor' THEN ?1
                 WHEN 'latest_graph_revision' THEN ?2
             END
             WHERE key IN ('latest_event_cursor', 'latest_graph_revision')",
            params![cursor, graph_revision],
        )?
    };
    let expected = if matches!(&envelope.payload, Event::CompositionCommitted { .. }) {
        3
    } else {
        2
    };
    if updated != expected {
        return Err(HostError::InvalidEnvelope(format!(
            "event highwater metadata is incomplete: updated {updated} of {expected} rows"
        )));
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match read_bounded_file_following_symlinks(
        path,
        "read installed rollback document",
        MAX_COMPOSITION_DOCUMENT_BYTES,
    ) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(LoaderError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn to_i64(value: u64, label: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| HostError::InvalidEnvelope(format!("{label} exceeds i64")))
}

fn to_u64(value: i64, label: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| HostError::InvalidEnvelope(format!("negative {label}")))
}

fn utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| HostError::InvalidEnvelope(format!("path {} is not UTF-8", path.display())))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn leased_open_rejects_a_replaced_database_before_mutation() {
        let temp = tempdir().expect("tempdir");
        let database_path = temp.path().join("state.sqlite3");
        let workspace = crate::CompositionWorkspace {
            database_path: database_path.clone(),
            cache_root: temp.path().join("cache"),
            manifest_path: temp.path().join("composition.toml"),
            lock_path: temp.path().join("rsi-meta.lock"),
        };
        let lease = crate::workspace::WorkspaceLease::acquire(&workspace).expect("workspace lease");
        let replacement = temp.path().join("replacement.sqlite3");
        std::fs::File::create(&replacement).expect("replacement database");
        std::fs::rename(&replacement, &database_path).expect("replace database path");

        assert!(matches!(
            Persistence::open_leased(&database_path, &lease),
            Err(HostError::OperationRejected { ref code, .. })
                if code == "workspace_identity_changed"
        ));
        assert_eq!(
            std::fs::metadata(&database_path)
                .expect("replacement metadata")
                .len(),
            0,
            "identity rejection must happen before SQLite mutates the replacement"
        );
    }

    #[test]
    fn enables_wal_and_applies_compare_and_swap() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        assert_eq!(persistence.journal_mode().expect("journal mode"), "wal");

        let instance = InstanceId::new("writer");
        let first = persistence
            .compare_and_swap_plugin_state(
                "demo",
                &instance,
                "checkpoint",
                None,
                Some(&serde_json::json!({"offset": 1})),
            )
            .expect("create state");
        assert!(matches!(first, CasResult::Applied(value) if value.version == 1));

        let conflict = persistence
            .compare_and_swap_plugin_state(
                "demo",
                &instance,
                "checkpoint",
                None,
                Some(&serde_json::json!({"offset": 2})),
            )
            .expect("conflict result");
        assert!(matches!(
            conflict,
            CasResult::Conflict(Some(value)) if value.version == 1
        ));

        let deleted = persistence
            .compare_and_swap_plugin_state("demo", &instance, "checkpoint", Some(1), None)
            .expect("delete state");
        assert!(matches!(
            deleted,
            CasResult::Applied(value) if value.version == 2 && value.value.is_none()
        ));
    }

    #[test]
    fn compaction_keeps_replay_identity_and_expires_old_event_cursors() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        let outcome = |id: &str| {
            CommandOutcomeEnvelope::new(
                id.to_owned(),
                GraphRevision(1),
                crate::protocol::CommandOutcome::Applied,
            )
        };
        for id in ["op-1", "op-2", "op-3"] {
            persistence
                .store_operation_outcome("demo", id, "apply", id.as_bytes(), &outcome(id))
                .unwrap();
            persistence
                .append_event(
                    "demo",
                    id,
                    GraphRevision(1),
                    Event::Unknown {
                        event_type: "fixture".to_owned(),
                        payload: serde_json::Map::new(),
                    },
                )
                .unwrap();
        }
        let now = persistence
            .connection
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .unwrap();
        persistence.compact_at(now, 2, 2).unwrap();

        assert!(matches!(
            persistence.find_command("op-1").unwrap(),
            Some(StoredCommand::Expired { classification, .. }) if classification == "applied"
        ));
        assert!(matches!(
            persistence.query_events_through(0, u64::MAX, 10),
            Err(HostError::EventCursorExpired {
                requested: 0,
                minimum_available: 1,
            })
        ));
        assert_eq!(
            persistence
                .query_events_through(1, u64::MAX, 10)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn compaction_removes_the_complete_prefix_through_the_minimum_cursor() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        for index in 1..=4 {
            persistence
                .append_event(
                    "demo",
                    &format!("event-{index}"),
                    GraphRevision(1),
                    Event::Unknown {
                        event_type: "fixture".to_owned(),
                        payload: serde_json::Map::new(),
                    },
                )
                .unwrap();
        }
        let now = persistence
            .connection
            .query_row("SELECT unixepoch()", [], |row| row.get::<_, i64>(0))
            .unwrap();
        let old = now
            .saturating_sub(i64::try_from(EVENT_RETENTION_SECONDS).unwrap())
            .saturating_sub(1);
        persistence
            .connection
            .execute(
                "UPDATE control_event SET created_at = ?1 WHERE cursor = 3",
                [old],
            )
            .unwrap();

        persistence.compact_at(now, 100, 100).unwrap();

        let mut statement = persistence
            .connection
            .prepare("SELECT cursor FROM control_event ORDER BY cursor")
            .unwrap();
        let remaining = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(remaining, vec![4]);
    }

    #[test]
    fn compaction_rejects_an_event_highwater_behind_retained_rows() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        persistence
            .append_event(
                "demo",
                "event-1",
                GraphRevision(1),
                Event::Unknown {
                    event_type: "fixture".to_owned(),
                    payload: serde_json::Map::new(),
                },
            )
            .unwrap();
        let now = persistence
            .connection
            .query_row("SELECT unixepoch()", [], |row| row.get::<_, i64>(0))
            .unwrap();
        let old = now
            .saturating_sub(i64::try_from(EVENT_RETENTION_SECONDS).unwrap())
            .saturating_sub(1);
        persistence
            .connection
            .execute("UPDATE control_event SET created_at = ?1", [old])
            .unwrap();
        persistence
            .connection
            .execute(
                "UPDATE store_meta SET value = '0' WHERE key = 'latest_event_cursor'",
                [],
            )
            .unwrap();

        assert!(matches!(
            persistence.compact_at(now, 100, 100),
            Err(HostError::InvalidEnvelope(message))
                if message == "event cursor highwater is behind retained rows"
        ));
    }

    #[test]
    fn compaction_preserves_event_and_graph_highwater_and_current_composition() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("state.sqlite3");
        let mut persistence = Persistence::open(&path).expect("database");
        let expected = persistence
            .append_event(
                "demo",
                "apply-7",
                GraphRevision(7),
                Event::CompositionCommitted {
                    source: crate::protocol::CompositionChangeSource::Apply,
                    composition_id: "demo".to_owned(),
                    manifest_sha256: "manifest".to_owned(),
                    lock_sha256: "lock".to_owned(),
                    active_instances: 3,
                    inactive_instances: 1,
                },
            )
            .expect("append composition event");
        let now = persistence
            .connection
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .unwrap();

        persistence
            .compact_at(now, 100, 0)
            .expect("compact all events");

        assert_eq!(persistence.latest_cursor().unwrap(), expected.cursor);
        assert_eq!(
            persistence.latest_graph_revision().unwrap(),
            GraphRevision(7)
        );
        assert_eq!(
            persistence.latest_composition_event().unwrap(),
            Some(expected.clone())
        );
        drop(persistence);

        let reopened = Persistence::open(&path).expect("reopen compacted database");
        assert_eq!(reopened.latest_cursor().unwrap(), expected.cursor);
        assert_eq!(reopened.latest_graph_revision().unwrap(), GraphRevision(7));
        assert_eq!(reopened.latest_composition_event().unwrap(), Some(expected));
    }

    #[test]
    fn schema_bootstrap_is_atomic_when_a_later_ddl_statement_fails() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("state.sqlite3");
        let connection = Connection::open(&path).expect("seed database");
        connection
            .execute_batch("CREATE VIEW plugin_state AS SELECT 1 AS value;")
            .expect("seed conflicting object");
        drop(connection);

        assert!(Persistence::open(&path).is_err());

        let connection = Connection::open(&path).expect("inspect failed bootstrap");
        let store_meta_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'store_meta')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !store_meta_exists,
            "failed bootstrap must leave no partial schema"
        );
    }

    #[test]
    fn unversioned_store_meta_is_rejected_without_rebranding() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("state.sqlite3");
        let connection = Connection::open(&path).expect("seed database");
        connection
            .execute_batch(
                "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\
                 INSERT INTO store_meta(key, value) VALUES ('foreign_marker', 'owned');",
            )
            .expect("seed unversioned metadata");
        drop(connection);

        assert!(Persistence::open(&path).is_err());

        let connection = Connection::open(&path).expect("inspect rejected database");
        let schema_version: Option<String> = connection
            .query_row(
                "SELECT value FROM store_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            schema_version, None,
            "opening must not rebrand an unknown store"
        );
    }

    #[test]
    fn terminal_apply_deletes_its_recovery_journal() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        let desired = DesiredState {
            manifest_sha256: Some("manifest".to_owned()),
            lock_sha256: Some("lock".to_owned()),
            applied: true,
            last_rejection_code: None,
            plugin_restart_requested: false,
        };
        persistence
            .reserve_apply("demo", "apply-1", b"request", &desired, GraphRevision(1))
            .unwrap();
        let outcome = CommandOutcomeEnvelope::new(
            "apply-1".to_owned(),
            GraphRevision(1),
            crate::protocol::CommandOutcome::Applied,
        );
        persistence
            .begin_apply(
                "apply",
                "apply-1",
                "demo",
                &temp.path().join("installed.toml"),
                &temp.path().join("installed.lock"),
                &temp.path().join("candidate.toml"),
                &temp.path().join("candidate.lock"),
                "manifest",
                "lock",
                GraphRevision(1),
                &Event::Unknown {
                    event_type: "fixture".to_owned(),
                    payload: serde_json::Map::new(),
                },
                &outcome,
                &desired,
            )
            .unwrap();
        persistence
            .commit_pending_apply(
                "apply-1",
                GraphRevision(1),
                Event::Unknown {
                    event_type: "fixture".to_owned(),
                    payload: serde_json::Map::new(),
                },
                &outcome,
                &desired,
            )
            .unwrap();
        let journals: i64 = persistence
            .connection
            .query_row("SELECT COUNT(*) FROM apply_journal", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journals, 0);
    }

    #[test]
    fn post_commit_maintenance_failure_does_not_reclassify_a_durable_apply() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        let desired = DesiredState {
            manifest_sha256: Some("manifest".to_owned()),
            lock_sha256: Some("lock".to_owned()),
            applied: true,
            last_rejection_code: None,
            plugin_restart_requested: false,
        };
        let outcome = CommandOutcomeEnvelope::new(
            "apply-maintenance-failure".to_owned(),
            GraphRevision(1),
            crate::protocol::CommandOutcome::Applied,
        );
        persistence
            .reserve_apply(
                "demo",
                "apply-maintenance-failure",
                b"request",
                &desired,
                GraphRevision(1),
            )
            .unwrap();
        persistence
            .begin_apply(
                "apply",
                "apply-maintenance-failure",
                "demo",
                &temp.path().join("installed.toml"),
                &temp.path().join("installed.lock"),
                &temp.path().join("candidate.toml"),
                &temp.path().join("candidate.lock"),
                "manifest",
                "lock",
                GraphRevision(1),
                &Event::Unknown {
                    event_type: "fixture".to_owned(),
                    payload: serde_json::Map::new(),
                },
                &outcome,
                &desired,
            )
            .unwrap();
        persistence.maintenance_writes = 255;
        persistence.fail_maintenance_after_commit = true;

        let event = persistence
            .commit_pending_apply(
                "apply-maintenance-failure",
                GraphRevision(1),
                Event::Unknown {
                    event_type: "fixture".to_owned(),
                    payload: serde_json::Map::new(),
                },
                &outcome,
                &desired,
            )
            .expect("maintenance cannot turn a durable apply into an error");

        assert_eq!(event.graph_revision, GraphRevision(1));
        assert!(matches!(
            persistence
                .find_command("apply-maintenance-failure")
                .unwrap(),
            Some(StoredCommand::Terminal { outcome: stored, .. }) if *stored == outcome
        ));
    }

    #[test]
    fn state_key_quota_rejects_before_inserting_an_extra_row() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        persistence
            .connection
            .execute(
                "WITH RECURSIVE keys(value) AS (
                     SELECT 1 UNION ALL SELECT value + 1 FROM keys WHERE value < ?1
                 )
                 INSERT INTO plugin_state(
                     composition_id, instance_id, state_key, version, value_json, tombstone
                 ) SELECT 'demo', 'writer', printf('key-%d', value), 1, 'null', 0 FROM keys",
                [i64::try_from(MAX_STATE_KEYS_PER_INSTANCE).unwrap()],
            )
            .unwrap();

        let error = persistence
            .compare_and_swap_plugin_state(
                "demo",
                &InstanceId::new("writer"),
                "one-too-many",
                None,
                Some(&serde_json::Value::Null),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            HostError::StateQuotaExceeded {
                quota: "instance_key_count",
                requested: 4_097,
                maximum: 4_096,
            }
        ));
    }

    #[test]
    fn state_byte_quota_counts_utf8_bytes_not_unicode_scalars() {
        let temp = tempdir().expect("tempdir");
        let mut persistence =
            Persistence::open(&temp.path().join("state.sqlite3")).expect("database");
        let existing = "€".repeat(MAX_STATE_BYTES_PER_INSTANCE / 3);
        persistence
            .connection
            .execute(
                "INSERT INTO plugin_state(\
                    composition_id, instance_id, state_key, version, value_json, tombstone\
                 ) VALUES ('demo', 'writer', 'large', 1, ?1, 0)",
                [&existing],
            )
            .expect("seed multibyte state");

        let error = persistence
            .compare_and_swap_plugin_state(
                "demo",
                &InstanceId::new("writer"),
                "one-too-many",
                None,
                Some(&serde_json::Value::Null),
            )
            .expect_err("UTF-8 bytes must count toward the quota");
        assert!(matches!(
            error,
            HostError::StateQuotaExceeded {
                quota: "instance_live_bytes",
                maximum: MAX_STATE_BYTES_PER_INSTANCE,
                ..
            }
        ));
    }

    #[test]
    fn rollback_document_reads_are_bounded() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("installed-manifest.toml");
        std::fs::write(&path, vec![b'#'; 4 * 1024 * 1024 + 1]).expect("oversized document");

        assert!(matches!(
            read_optional(&path),
            Err(HostError::Loader(
                rsi_meta_loader::LoaderError::InputTooLarge {
                    maximum_bytes: 4_194_304,
                    ..
                }
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rollback_document_read_rejects_a_fifo_without_waiting_for_a_writer() {
        use std::process::Command;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("installed-manifest.fifo");
        assert!(
            Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );

        assert!(matches!(
            read_optional(&path),
            Err(HostError::Loader(
                rsi_meta_loader::LoaderError::UnsafeInputFile { .. }
            ))
        ));
    }
}
