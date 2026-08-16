use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use rsi_meta_loader::{LoaderError, read_bounded_file_following_symlinks};

use crate::host::MAX_COMPOSITION_DOCUMENT_BYTES;
use crate::model::{CompositionLock, DesiredState, GraphRevision, InstanceId};
use crate::protocol::{CommandOutcomeEnvelope, Event, EventEnvelope};
use crate::{HostError, Result};

mod operations;

pub(crate) const STORE_SCHEMA_VERSION: u32 = 5;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PendingEffect {
    Apply {
        requested_desired: DesiredState,
        graph_revision: GraphRevision,
    },
    Lock {
        lock_path: std::path::PathBuf,
        lock: CompositionLock,
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
    Legacy,
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
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluginStateValue {
    pub version: u64,
    pub value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CasResult {
    Applied(PluginStateValue),
    Conflict(Option<PluginStateValue>),
}

impl Persistence {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| HostError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        if let Some(version) = existing_schema_version(&connection)?
            && version > STORE_SCHEMA_VERSION
        {
            return Err(HostError::UnsupportedStoreSchema {
                found: version,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS store_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO store_meta(key, value) VALUES ('schema_version', '0');
            INSERT OR IGNORE INTO store_meta(key, value) VALUES ('token_generation', '0');
            INSERT OR IGNORE INTO store_meta(key, value) VALUES (
                'desired_state',
                '{"manifest_sha256":null,"lock_sha256":null,"applied":false,"last_rejection_code":null}'
            );
            CREATE TABLE IF NOT EXISTS plugin_state (
                composition_id TEXT NOT NULL,
                instance_id TEXT NOT NULL,
                state_key TEXT NOT NULL,
                version INTEGER NOT NULL CHECK (version > 0),
                value_json TEXT,
                tombstone INTEGER NOT NULL CHECK (tombstone IN (0, 1)),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                PRIMARY KEY (composition_id, instance_id, state_key)
            );
            CREATE TABLE IF NOT EXISTS control_event (
                cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                schema_version INTEGER NOT NULL DEFAULT 0,
                composition_id TEXT NOT NULL,
                graph_revision INTEGER NOT NULL CHECK (graph_revision >= 0),
                event_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            CREATE TABLE IF NOT EXISTS command_outcome (
                command_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL DEFAULT 0,
                composition_id TEXT NOT NULL,
                request_hash BLOB NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('pending', 'terminal')),
                outcome_json TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            CREATE TABLE IF NOT EXISTS apply_journal (
                command_id TEXT PRIMARY KEY REFERENCES command_outcome(command_id),
                composition_id TEXT NOT NULL,
                installed_manifest_path TEXT NOT NULL,
                installed_lock_path TEXT NOT NULL,
                candidate_manifest_path TEXT NOT NULL,
                candidate_lock_path TEXT NOT NULL,
                candidate_manifest_hash TEXT NOT NULL,
                candidate_lock_hash TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'aborted')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            "#,
        )?;
        migrate_store(&mut connection)?;
        Ok(Self { connection })
    }

    pub(crate) fn latest_graph_revision(&self) -> Result<GraphRevision> {
        let value: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(graph_revision), 0) FROM control_event",
            [],
            |row| row.get(0),
        )?;
        Ok(GraphRevision(to_u64(value, "graph revision")?))
    }

    pub(crate) fn latest_cursor(&self) -> Result<u64> {
        let value: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(cursor), 0) FROM control_event",
            [],
            |row| row.get(0),
        )?;
        to_u64(value, "event cursor")
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
        let encoded = self.connection.query_row(
            "SELECT event_json FROM control_event WHERE json_extract(event_json, '$.payload.type') = 'composition_committed' ORDER BY cursor DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        ).optional()?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(HostError::from))
            .transpose()
    }

    pub(crate) fn find_command(&self, command_id: &str) -> Result<Option<StoredCommand>> {
        let stored = self
            .connection
            .query_row(
                "SELECT request_hash, status, outcome_json
                 FROM command_outcome WHERE command_id = ?1",
                [command_id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        stored
            .map(|(request_hash, status, json)| match status.as_str() {
                "pending" => Ok(StoredCommand::Pending { request_hash }),
                "terminal" => match json {
                    Some(json) => Ok(StoredCommand::Terminal {
                        request_hash,
                        outcome: Box::new(serde_json::from_str(&json)?),
                    }),
                    None => Ok(StoredCommand::Legacy),
                },
                other => Err(HostError::InvalidEnvelope(format!(
                    "unknown command outcome status {other:?}"
                ))),
            })
            .transpose()
    }

    pub(crate) fn reserve_lock(
        &mut self,
        composition_id: &str,
        command_id: &str,
        request_hash: &[u8],
        lock_path: &Path,
        lock: &CompositionLock,
        graph_revision: GraphRevision,
    ) -> Result<()> {
        self.reserve_pending(
            composition_id,
            command_id,
            request_hash,
            &PendingEffect::Lock {
                lock_path: lock_path.to_owned(),
                lock: lock.clone(),
                graph_revision,
            },
            None,
        )
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
               command_id, schema_version, composition_id, request_hash, status,
               outcome_json, pending_kind, pending_json, operation_kind
             ) VALUES (?1, ?2, ?3, ?4, 'pending', NULL, ?5, ?6, ?5)",
            params![
                command_id,
                i64::from(STORE_SCHEMA_VERSION),
                composition_id,
                request_hash,
                match effect {
                    PendingEffect::Apply { .. } => "apply",
                    PendingEffect::Lock { .. } => "lock",
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
             SET schema_version = ?1, status = 'terminal', outcome_json = ?2
             WHERE command_id = ?3 AND status = 'pending'",
            params![
                i64::from(STORE_SCHEMA_VERSION),
                serde_json::to_string(outcome)?,
                command_id
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
               command_id, schema_version, composition_id, request_hash, status, outcome_json,
               operation_kind
             ) VALUES (?1, ?2, ?3, ?4, 'terminal', ?5, 'legacy')",
            params![
                command_id,
                i64::from(STORE_SCHEMA_VERSION),
                composition_id,
                request_hash,
                json
            ],
        )?;
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
               command_id, schema_version, composition_id, request_hash, status, outcome_json,
               operation_kind
             ) VALUES (?1, ?2, ?3, ?4, 'terminal', ?5, ?6)",
            params![
                operation_id,
                i64::from(STORE_SCHEMA_VERSION),
                composition_id,
                request_hash,
                serde_json::to_string(outcome)?,
                operation_kind,
            ],
        )?;
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
               command_id, schema_version, composition_id, request_hash, status, outcome_json,
               operation_kind
             ) VALUES (?1, ?2, ?3, ?4, 'terminal', ?5, 'token_rotation')",
            params![
                command_id,
                i64::from(STORE_SCHEMA_VERSION),
                composition_id,
                request_hash,
                serde_json::to_string(&outcome)?
            ],
        )?;
        transaction.commit()?;
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
               schema_version, composition_id, command_id, graph_revision, event_json
             ) VALUES (?1, ?2, ?3, ?4, '{}')",
            params![
                i64::from(STORE_SCHEMA_VERSION),
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
        transaction.execute(
            "INSERT INTO command_outcome(
               command_id, schema_version, composition_id, request_hash, status, outcome_json,
               operation_kind
             ) VALUES (?1, ?2, ?3, ?4, 'terminal', ?5, 'shutdown')",
            params![
                command_id,
                i64::from(STORE_SCHEMA_VERSION),
                composition_id,
                request_hash,
                serde_json::to_string(outcome)?
            ],
        )?;
        transaction.commit()?;
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
               schema_version, composition_id, command_id, graph_revision, event_json
             ) VALUES (?1, ?2, ?3, ?4, '{}')",
            params![
                i64::from(STORE_SCHEMA_VERSION),
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
        transaction.commit()?;
        Ok(envelope)
    }

    pub(crate) fn query_events(&self, after_cursor: u64, limit: u32) -> Result<Vec<EventEnvelope>> {
        self.query_events_through(after_cursor, u64::MAX, limit)
    }

    pub(crate) fn query_events_through(
        &self,
        after_cursor: u64,
        through_cursor: u64,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>> {
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
               terminal_outcome_json, terminal_desired_json, state, operation_kind
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
               ?13, ?14, ?15, ?16, 'pending', ?17
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
             WHERE command_id = ?1 AND state = 'pending'",
            [command_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO control_event(
               schema_version, composition_id, command_id, graph_revision, event_json
             ) VALUES (?1, ?2, ?3, ?4, '{}')",
            params![
                i64::from(STORE_SCHEMA_VERSION),
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
        let updated = transaction.execute(
            "UPDATE command_outcome SET status = 'terminal', outcome_json = ?1
             WHERE command_id = ?2 AND status = 'pending'",
            params![serde_json::to_string(outcome)?, command_id],
        )?;
        if updated != 1 {
            return Err(HostError::InvalidEnvelope(format!(
                "pending command outcome {command_id:?} disappeared"
            )));
        }
        transaction.execute(
            "UPDATE apply_journal SET state = 'committed',
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE command_id = ?1 AND state = 'pending'",
            [command_id],
        )?;
        transaction.execute(
            "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
            [serde_json::to_string(desired)?],
        )?;
        transaction.commit()?;
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
            "UPDATE command_outcome SET status = 'terminal', outcome_json = ?1
             WHERE command_id = ?2 AND status = 'pending' AND operation_kind = 'install'",
            params![serde_json::to_string(outcome)?, operation_id],
        )?;
        if updated != 1 {
            return Err(HostError::InvalidEnvelope(format!(
                "pending install operation {operation_id:?} disappeared"
            )));
        }
        transaction.execute(
            "UPDATE apply_journal SET state = 'committed',
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE command_id = ?1 AND state = 'pending' AND operation_kind = 'install'",
            [operation_id],
        )?;
        transaction.commit()?;
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
            "UPDATE command_outcome SET status = 'terminal', outcome_json = ?1
             WHERE command_id = ?2 AND status = 'pending'",
            params![serde_json::to_string(outcome)?, command_id],
        )?;
        transaction.execute(
            "UPDATE apply_journal SET state = 'aborted',
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE command_id = ?1 AND state = 'pending'",
            [command_id],
        )?;
        transaction.execute(
            "UPDATE store_meta SET value = ?1 WHERE key = 'desired_state'",
            [serde_json::to_string(desired)?],
        )?;
        transaction.commit()?;
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
             WHERE j.state = 'pending' AND o.status = 'pending'
             ORDER BY j.updated_at ASC",
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
        transaction.execute(
            "INSERT INTO plugin_state(
               composition_id, instance_id, state_key, version, value_json, tombstone
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(composition_id, instance_id, state_key) DO UPDATE SET
               version = excluded.version,
               value_json = excluded.value_json,
               tombstone = excluded.tombstone,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
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
    version
        .map(|version| {
            version.parse::<u32>().map_err(|_| {
                HostError::InvalidEnvelope("stored schema version is not a u32".to_owned())
            })
        })
        .transpose()
}

fn migrate_store(connection: &mut Connection) -> Result<()> {
    let version: String = connection.query_row(
        "SELECT value FROM store_meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    let mut version = version
        .parse::<u32>()
        .map_err(|_| HostError::InvalidEnvelope("stored schema version is not a u32".to_owned()))?;
    if version > STORE_SCHEMA_VERSION {
        return Err(HostError::UnsupportedStoreSchema {
            found: version,
            supported: STORE_SCHEMA_VERSION,
        });
    }
    if version == 0 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "ALTER TABLE control_event ADD COLUMN command_id TEXT;
             ALTER TABLE apply_journal ADD COLUMN previous_manifest_bytes BLOB;
             ALTER TABLE apply_journal ADD COLUMN previous_lock_bytes BLOB;
             ALTER TABLE apply_journal ADD COLUMN previous_manifest_hash TEXT;
             ALTER TABLE apply_journal ADD COLUMN previous_lock_hash TEXT;
             UPDATE store_meta SET value = '1' WHERE key = 'schema_version';",
        )?;
        transaction.commit()?;
        version = 1;
    }
    if version == 1 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "ALTER TABLE apply_journal ADD COLUMN terminal_graph_revision INTEGER;
             ALTER TABLE apply_journal ADD COLUMN terminal_event_json TEXT;
             ALTER TABLE apply_journal ADD COLUMN terminal_outcome_json TEXT;
             ALTER TABLE apply_journal ADD COLUMN terminal_desired_json TEXT;
             UPDATE store_meta SET value = '2' WHERE key = 'schema_version';",
        )?;
        transaction.commit()?;
        version = 2;
    }
    if version == 2 {
        migrate_event_command_ids(connection)?;
        version = 3;
    }
    if version == 3 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "ALTER TABLE command_outcome ADD COLUMN pending_kind TEXT;
             ALTER TABLE command_outcome ADD COLUMN pending_json TEXT;
             UPDATE store_meta SET value = '4' WHERE key = 'schema_version';",
        )?;
        transaction.commit()?;
        version = 4;
    }
    if version == 4 {
        migrate_domain_operations(connection)?;
    }
    Ok(())
}

fn migrate_domain_operations(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "ALTER TABLE command_outcome ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'legacy';
         ALTER TABLE apply_journal ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'apply'; DELETE FROM apply_journal WHERE state = 'aborted';",
    )?;
    // Aborted v4 journals have no recoverable side effect, but still retain a
    // foreign key to their rejected outcome. Remove the inert journal record
    // before read/rejection outcomes are discarded below.
    let outcomes = {
        let mut statement = transaction.prepare(
            "SELECT command_id, outcome_json FROM command_outcome
             WHERE status = 'terminal' AND outcome_json IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (operation_id, json) in outcomes {
        let value: serde_json::Value = serde_json::from_str(&json)?;
        let outcome_type = value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(serde_json::Value::as_str);
        let side_effect = matches!(
            outcome_type,
            Some(
                "applied"
                    | "no_change"
                    | "daemon_restarting"
                    | "restart_required"
                    | "installed"
                    | "token_rotated"
                    | "shutting_down"
            )
        );
        if side_effect {
            transaction.execute(
                "UPDATE command_outcome
                 SET operation_kind = 'legacy', outcome_json = NULL
                 WHERE command_id = ?1",
                [&operation_id],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM command_outcome WHERE command_id = ?1",
                [&operation_id],
            )?;
        }
    }
    transaction.execute(
        "UPDATE command_outcome SET operation_kind = pending_kind
         WHERE status = 'pending' AND pending_kind IS NOT NULL",
        [],
    )?;
    transaction.execute(
        "UPDATE store_meta SET value = '5' WHERE key = 'schema_version'",
        [],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate_event_command_ids(connection: &mut Connection) -> Result<()> {
    const EVENT_COMMAND_ID_SCHEMA_VERSION: u32 = 3;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let events = {
        let mut statement = transaction.prepare(
            "SELECT cursor, command_id, event_json FROM control_event ORDER BY cursor ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (cursor, stored_command_id, json) in events {
        let mut envelope: serde_json::Value = serde_json::from_str(&json)?;
        let object = envelope.as_object_mut().ok_or_else(|| {
            HostError::InvalidEnvelope(format!(
                "stored control event at cursor {cursor} is not a JSON object"
            ))
        })?;
        let command_id = stored_command_id
            .filter(|command_id| !command_id.is_empty())
            .unwrap_or_else(|| format!("system/legacy/{cursor}"));
        object.insert(
            "command_id".to_owned(),
            serde_json::Value::String(command_id.clone()),
        );
        transaction.execute(
            "UPDATE control_event
             SET schema_version = ?1, command_id = ?2, event_json = ?3
             WHERE cursor = ?4",
            params![
                i64::from(EVENT_COMMAND_ID_SCHEMA_VERSION),
                command_id,
                serde_json::to_string(&envelope)?,
                cursor
            ],
        )?;
    }
    transaction.execute(
        "UPDATE store_meta SET value = ?1 WHERE key = 'schema_version'",
        [EVENT_COMMAND_ID_SCHEMA_VERSION.to_string()],
    )?;
    transaction.commit()?;
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

fn read_plugin_state(
    connection: &Connection,
    composition_id: &str,
    instance_id: &InstanceId,
    key: &str,
) -> Result<Option<PluginStateValue>> {
    let row = connection
        .query_row(
            "SELECT version, value_json, tombstone FROM plugin_state
             WHERE composition_id = ?1 AND instance_id = ?2 AND state_key = ?3",
            params![composition_id, instance_id.0, key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(version, value_json, tombstone)| {
        let value = if tombstone == 0 {
            value_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?
        } else {
            None
        };
        Ok(PluginStateValue {
            version: to_u64(version, "plugin state version")?,
            value,
        })
    })
    .transpose()
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
