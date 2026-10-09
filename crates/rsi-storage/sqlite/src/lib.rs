//! `SQLite` backend for non-session storage domains.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage::{
    BackendLease, BackendOperations, KvBackend, MAXIMUM_STORAGE_DOMAIN_BYTES,
    MAXIMUM_STORAGE_IDENTIFIER_BYTES, MAXIMUM_STORAGE_RECORDS, MAXIMUM_STORAGE_VALUE_BYTES,
    RecordObjectSize, StorageError, StorageHubContract, StoredDomain, create_private_directories,
    encode_value, encoded_entry_bytes, validate_identifier, validate_value,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAXIMUM_SQLITE_DOMAINS: i64 = 1024;
fn domains_table_schema() -> String {
    format!("CREATE TABLE IF NOT EXISTS rsi_storage_domains (
  domain TEXT PRIMARY KEY NOT NULL,
  version INTEGER NOT NULL CHECK(version > 0),
  record_count INTEGER NOT NULL DEFAULT 0 CHECK(record_count >= 0 AND record_count <= {MAXIMUM_STORAGE_RECORDS}),
  record_bytes INTEGER NOT NULL DEFAULT 2 CHECK(record_bytes >= 2 AND record_bytes <= {MAXIMUM_STORAGE_DOMAIN_BYTES})
) STRICT")
}
const RECORDS_TABLE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rsi_storage_records (
  domain TEXT NOT NULL,
  key TEXT NOT NULL,
  value BLOB NOT NULL,
  PRIMARY KEY(domain, key),
  FOREIGN KEY(domain) REFERENCES rsi_storage_domains(domain) ON DELETE CASCADE
) STRICT";
const INSERT_TRIGGER_SCHEMA: &str = "CREATE TRIGGER IF NOT EXISTS rsi_storage_records_insert_count
AFTER INSERT ON rsi_storage_records
BEGIN
  UPDATE rsi_storage_domains SET record_count = record_count + 1 WHERE domain = NEW.domain;
END";
const DELETE_TRIGGER_SCHEMA: &str = "CREATE TRIGGER IF NOT EXISTS rsi_storage_records_delete_count
AFTER DELETE ON rsi_storage_records
BEGIN
  UPDATE rsi_storage_domains SET record_count = record_count - 1 WHERE domain = OLD.domain;
END";
fn schema_with_domains(domains: &str) -> String {
    format!("{domains}; {RECORDS_TABLE_SCHEMA}; {INSERT_TRIGGER_SCHEMA}; {DELETE_TRIGGER_SCHEMA};")
}

/// Configuration accepted by [`SqliteStorageFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteStorageConfig {
    /// Exact backend registration name.
    pub name: String,
    /// Absolute database path.
    pub path: PathBuf,
}

impl SqliteStorageConfig {
    fn validate(&self) -> Result<(), StorageError> {
        validate_identifier("backend", &self.name)?;
        if !self.path.is_absolute() {
            return Err(StorageError::InvalidInput(
                "SQLite storage path must be absolute".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SqliteBackend {
    connection: Arc<Mutex<Connection>>,
    operation: Arc<BackendOperations>,
}

impl SqliteBackend {
    fn open(path: &Path) -> Result<Self, StorageError> {
        let parent = path
            .parent()
            .ok_or_else(|| StorageError::Io("database has no parent".into()))?;
        create_private_directories(parent)?;
        let parent =
            std::fs::canonicalize(parent).map_err(|error| StorageError::Io(error.to_string()))?;
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| StorageError::Io("database has no file name".into()))?,
        );
        ensure_private_database_file(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| sqlite_io(&error))?;
        connection
            .busy_timeout(SQLITE_BUSY_TIMEOUT)
            .map_err(|error| sqlite_io(&error))?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .map_err(|error| sqlite_io(&error))?;
        connection
            .execute_batch("PRAGMA synchronous = FULL")
            .map_err(|error| sqlite_io(&error))?;
        initialize_or_validate_schema(&connection)?;
        validate_accounting(&connection)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL")
            .map_err(|error| sqlite_io(&error))?;
        set_sqlite_sidecar_permissions(&path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            operation: Arc::new(BackendOperations::default()),
        })
    }
}

impl SqliteBackend {
    async fn transaction<F>(&self, body: F) -> Result<(), StorageError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(), StorageError> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        self.operation
            .run(move || {
                let mut connection = connection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let transaction = connection
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|error| sqlite_io(&error))?;
                finish_transaction(transaction, body)
            })
            .await
    }
}

fn finish_transaction(
    transaction: Transaction<'_>,
    body: impl FnOnce(&Transaction<'_>) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let failure = match body(&transaction) {
        // Retain the transaction: consuming commit() hides rollback errors in Drop.
        Ok(()) => match transaction.execute_batch("COMMIT") {
            Ok(()) => return Ok(()),
            Err(error) => {
                let rejected = matches!(
                    error.sqlite_error_code(),
                    Some(
                        rusqlite::ErrorCode::DatabaseBusy
                            | rusqlite::ErrorCode::ConstraintViolation
                    )
                ) && !transaction.is_autocommit();
                // A WAL callback can return these same codes after commit has completed.
                if !rejected {
                    return Err(StorageError::OutcomeUnknown(format!(
                        "SQLite commit: {error}"
                    )));
                }
                StorageError::Io(format!("SQLite commit rejected: {error}"))
            }
        },
        Err(error) => error,
    };
    transaction.finish().map_err(|rollback| {
        StorageError::OutcomeUnknown(format!("{failure}; SQLite rollback: {rollback}"))
    })?;
    Err(failure)
}

fn load_domain(
    connection: &mut Connection,
    domain: &str,
    after_header: impl FnOnce(),
) -> Result<Option<StoredDomain>, StorageError> {
    let snapshot = connection
        .transaction()
        .map_err(|error| sqlite_io(&error))?;
    let stored_header = snapshot
        .query_row(
            "SELECT version, record_count, record_bytes FROM rsi_storage_domains WHERE domain = ?1",
            [&domain],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_io(&error))?;
    let Some((version, stored_record_count, stored_bytes)) = stored_header else {
        return Ok(None);
    };
    let stored_bytes = usize::try_from(stored_bytes).map_err(|_| domain_byte_bound(domain))?;
    let stored_record_count = usize::try_from(stored_record_count).map_err(|_| {
        StorageError::Corrupt(format!(
            "domain `{domain}` has an invalid backend record count"
        ))
    })?;
    if stored_record_count > MAXIMUM_STORAGE_RECORDS {
        return Err(StorageError::Corrupt(format!(
            "domain `{domain}` exceeds the backend record bound"
        )));
    }
    after_header();
    let mut size = RecordObjectSize::default();
    let mut statement = snapshot
        .prepare(
            "SELECT key, length(value), value
         FROM rsi_storage_records WHERE domain = ?1 ORDER BY key",
        )
        .map_err(|error| sqlite_io(&error))?;
    let mut rows = statement
        .query([&domain])
        .map_err(|error| sqlite_io(&error))?;
    let mut records = BTreeMap::new();
    while let Some(row) = rows.next().map_err(|error| sqlite_io(&error))? {
        if records.len() == MAXIMUM_STORAGE_RECORDS {
            return Err(StorageError::Corrupt(format!(
                "domain `{domain}` exceeds the backend record bound"
            )));
        }
        let key = row.get::<_, String>(0).map_err(|error| sqlite_io(&error))?;
        let encoded_len = row.get::<_, i64>(1).map_err(|error| sqlite_io(&error))?;
        if encoded_len < 0
            || usize::try_from(encoded_len)
                .map_or(true, |length| length > MAXIMUM_STORAGE_VALUE_BYTES)
        {
            return Err(StorageError::Corrupt(format!(
                "domain `{domain}` contains an oversized stored value"
            )));
        }
        validate_identifier("record key", &key)
            .map_err(|_| StorageError::Corrupt("invalid record key".into()))?;
        let encoded_len = usize::try_from(encoded_len).map_err(|_| {
            StorageError::Corrupt(format!(
                "domain `{domain}` contains an invalid stored value length"
            ))
        })?;
        size = size
            .with_entry(
                None,
                encoded_entry_bytes(&key, encoded_len).map_err(|_| domain_byte_bound(domain))?,
            )
            .map_err(|_| domain_byte_bound(domain))?;
        if size.bytes() > MAXIMUM_STORAGE_DOMAIN_BYTES {
            return Err(domain_byte_bound(domain));
        }
        let bytes = row
            .get::<_, Vec<u8>>(2)
            .map_err(|error| sqlite_io(&error))?;
        let value = serde_json::from_slice(&bytes)
            .map_err(|error| StorageError::Corrupt(error.to_string()))?;
        let compact_bytes = validate_value(&value)
            .map_err(|_| StorageError::Corrupt("invalid stored value".into()))?;
        if compact_bytes != encoded_len {
            return Err(StorageError::Corrupt(
                "stored value is not compact JSON".into(),
            ));
        }
        records.insert(key, value);
    }
    if records.len() != stored_record_count || size.bytes() != stored_bytes {
        return Err(StorageError::Corrupt(format!(
            "domain `{domain}` has inconsistent record-accounting metadata"
        )));
    }
    drop(rows);
    drop(statement);
    snapshot.commit().map_err(|error| sqlite_io(&error))?;
    Ok(Some(StoredDomain { version, records }))
}

#[async_trait]
impl KvBackend for SqliteBackend {
    fn ensure_available(&self) -> Result<(), StorageError> {
        self.operation.ensure_available()
    }

    async fn load(&self, domain: &str) -> Result<Option<StoredDomain>, StorageError> {
        self.ensure_available()?;
        validate_identifier("domain", domain)?;
        let connection = Arc::clone(&self.connection);
        let domain = domain.to_owned();
        self.operation
            .run(move || {
                let mut connection = connection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                load_domain(&mut connection, &domain, || {})
            })
            .await
    }

    async fn put(
        &self,
        domain: &str,
        version: u32,
        key: &str,
        value: &Value,
    ) -> Result<(), StorageError> {
        self.ensure_available()?;
        validate_identifier("domain", domain)?;
        validate_identifier("record key", key)?;
        if version == 0 {
            return Err(StorageError::InvalidInput(
                "domain version must be nonzero".into(),
            ));
        }
        let bytes = encode_value(value)?;
        let domain = domain.to_owned();
        let key = key.to_owned();
        self.transaction(move |transaction| {
            insert_domain(transaction, &domain, version)?;
            let stored = stored_record(transaction, &domain, version, &key)?
                .ok_or_else(|| StorageError::Corrupt("inserted domain is missing".into()))?;
            let projected = stored
                .size
                .with_entry(stored.entry_bytes, encoded_entry_bytes(&key, bytes.len())?)?;
            if projected.records() > MAXIMUM_STORAGE_RECORDS {
                return Err(StorageError::InvalidInput(format!(
                    "domain `{domain}` record bound exceeded"
                )));
            }
            if projected.bytes() > MAXIMUM_STORAGE_DOMAIN_BYTES {
                return Err(StorageError::InvalidInput(format!(
                    "domain `{domain}` aggregate bound exceeded"
                )));
            }
            transaction
                .execute(
                    "INSERT INTO rsi_storage_records(domain, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT(domain, key) DO UPDATE SET value = excluded.value",
                    params![domain, key, bytes],
                )
                .map_err(|error| sqlite_io(&error))?;
            publish_size(transaction, &domain, projected)?;
            Ok(())
        })
        .await
    }

    async fn delete(&self, domain: &str, version: u32, key: &str) -> Result<(), StorageError> {
        self.ensure_available()?;
        validate_identifier("domain", domain)?;
        validate_identifier("record key", key)?;
        if version == 0 {
            return Err(StorageError::InvalidInput(
                "domain version must be nonzero".into(),
            ));
        }
        let domain = domain.to_owned();
        let key = key.to_owned();
        self.transaction(move |transaction| {
            let Some(stored) = stored_record(transaction, &domain, version, &key)? else {
                return Ok(());
            };
            let Some(previous) = stored.entry_bytes else {
                return Ok(());
            };
            let projected = stored.size.without_entry(previous)?;
            transaction
                .execute(
                    "DELETE FROM rsi_storage_records WHERE domain = ?1 AND key = ?2",
                    params![domain, key],
                )
                .map_err(|error| sqlite_io(&error))?;
            publish_size(transaction, &domain, projected)?;
            Ok(())
        })
        .await
    }
}

/// Ordinary plugin factory for one exact-name `SQLite` backend.
#[derive(Clone, Debug, Default)]
pub struct SqliteStorageFactory;

#[async_trait]
impl PluginFactory for SqliteStorageFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: SqliteStorageConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config.name.len() + config.path.as_os_str().len() + 32;
        Ok(
            PreparedActivation::with_state(desired.clone(), config, retained)
                .requiring_local::<StorageHubContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<SqliteStorageConfig>()?;
        let name = config.name.clone();
        let backend = tokio::task::spawn_blocking(move || SqliteBackend::open(&config.path))
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let backend = Arc::new(backend);
        let lease: BackendLease = plan
            .local::<StorageHubContract>()?
            .register(&name, backend.clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let registration = backend.operation.retain_registration(lease);
        plan.defer(
            "withdraw SQLite storage backend",
            Box::new(move || {
                Box::pin(async move {
                    registration.close().await;
                    Ok(())
                })
            }),
        )
    }
}

fn validate_schema(connection: &Connection) -> Result<(), StorageError> {
    let foreign_keys = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
        .map_err(|error| sqlite_io(&error))?;
    if !foreign_keys {
        return Err(StorageError::Corrupt(
            "SQLite storage requires foreign-key enforcement".into(),
        ));
    }
    let expected_objects = BTreeSet::from([
        (
            "index".to_owned(),
            "sqlite_autoindex_rsi_storage_domains_1".to_owned(),
        ),
        (
            "index".to_owned(),
            "sqlite_autoindex_rsi_storage_records_1".to_owned(),
        ),
        ("table".to_owned(), "rsi_storage_domains".to_owned()),
        ("table".to_owned(), "rsi_storage_records".to_owned()),
        (
            "trigger".to_owned(),
            "rsi_storage_records_delete_count".to_owned(),
        ),
        (
            "trigger".to_owned(),
            "rsi_storage_records_insert_count".to_owned(),
        ),
    ]);
    let mut statement = connection
        .prepare(
            "SELECT type, name FROM sqlite_master
             WHERE name GLOB 'rsi_storage_*'
                OR tbl_name IN ('rsi_storage_domains', 'rsi_storage_records')",
        )
        .map_err(|error| sqlite_io(&error))?;
    let actual_objects = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| sqlite_io(&error))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(|error| sqlite_io(&error))?;
    if actual_objects != expected_objects {
        return Err(StorageError::Corrupt(
            "SQLite storage has an incompatible schema object set".into(),
        ));
    }
    let domains_schema = domains_table_schema();
    for (object_type, name, expected) in [
        ("table", "rsi_storage_domains", domains_schema.as_str()),
        ("table", "rsi_storage_records", RECORDS_TABLE_SCHEMA),
        (
            "trigger",
            "rsi_storage_records_insert_count",
            INSERT_TRIGGER_SCHEMA,
        ),
        (
            "trigger",
            "rsi_storage_records_delete_count",
            DELETE_TRIGGER_SCHEMA,
        ),
    ] {
        let actual = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2",
                params![object_type, name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| sqlite_io(&error))?;
        let Some(actual) = actual else {
            return Err(StorageError::Corrupt(format!(
                "SQLite storage schema is missing {object_type} `{name}`"
            )));
        };
        if normalize_schema(&actual) != normalize_schema(expected) {
            return Err(StorageError::Corrupt(format!(
                "SQLite storage {object_type} `{name}` has an incompatible schema"
            )));
        }
    }
    Ok(())
}

fn initialize_or_validate_schema(connection: &Connection) -> Result<(), StorageError> {
    let owned_objects = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE name GLOB 'rsi_storage_*'
                OR tbl_name IN ('rsi_storage_domains', 'rsi_storage_records')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| sqlite_io(&error))?;
    if owned_objects == 0 {
        connection
            .execute_batch(&format!(
                "BEGIN IMMEDIATE; {} COMMIT;",
                schema_with_domains(&domains_table_schema())
            ))
            .map_err(|error| sqlite_io(&error))?;
    }
    validate_schema(connection)
}

fn normalize_schema(schema: &str) -> String {
    schema
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("ifnotexists", "")
}

fn domain_byte_bound(domain: &str) -> StorageError {
    StorageError::Corrupt(format!(
        "domain `{domain}` exceeds the {MAXIMUM_STORAGE_DOMAIN_BYTES}-byte backend bound"
    ))
}
fn checked_stored_size(
    domain: &str,
    records: i64,
    bytes: i64,
) -> Result<RecordObjectSize, StorageError> {
    let records = usize::try_from(records).map_err(|_| domain_record_bound(domain))?;
    let bytes = usize::try_from(bytes).map_err(|_| domain_byte_bound(domain))?;
    if records > MAXIMUM_STORAGE_RECORDS {
        return Err(domain_record_bound(domain));
    }
    if bytes > MAXIMUM_STORAGE_DOMAIN_BYTES {
        return Err(domain_byte_bound(domain));
    }
    RecordObjectSize::from_parts(records, bytes)
        .map_err(|_| StorageError::Corrupt(format!("domain `{domain}` has invalid accounting")))
}

fn domain_record_bound(domain: &str) -> StorageError {
    StorageError::Corrupt(format!(
        "domain `{domain}` has an invalid record count (maximum {MAXIMUM_STORAGE_RECORDS})"
    ))
}

fn bounded_domain_count(connection: &Connection) -> Result<i64, StorageError> {
    connection
        .query_row(
            "SELECT count(*) FROM (SELECT 1 FROM rsi_storage_domains ORDER BY domain LIMIT ?1)",
            [MAXIMUM_SQLITE_DOMAINS + 1],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_io(&error))
}

fn validate_accounting(connection: &Connection) -> Result<(), StorageError> {
    let identifier_limit = i64::try_from(MAXIMUM_STORAGE_IDENTIFIER_BYTES)
        .expect("storage identifier bound fits SQLite");
    let record_limit =
        i64::try_from(MAXIMUM_STORAGE_RECORDS + 1).expect("storage record bound fits SQLite");
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| sqlite_io(&error))?;
    if bounded_domain_count(&transaction)? > MAXIMUM_SQLITE_DOMAINS {
        return Err(StorageError::Corrupt(format!(
            "SQLite domain count exceeds {MAXIMUM_SQLITE_DOMAINS}"
        )));
    }
    let mut domains = transaction.prepare(
        "SELECT CASE WHEN length(CAST(domain AS BLOB)) <= ?1 THEN domain END, record_count, record_bytes, version FROM rsi_storage_domains ORDER BY domain LIMIT ?2",
    ).map_err(|error| sqlite_io(&error))?;
    let mut rows = domains
        .query([identifier_limit, MAXIMUM_SQLITE_DOMAINS])
        .map_err(|error| sqlite_io(&error))?;
    let mut records = transaction.prepare(
        "SELECT CASE WHEN length(CAST(key AS BLOB)) <= ?2 THEN key END, length(value) FROM rsi_storage_records WHERE domain=?1 ORDER BY key LIMIT ?3",
    ).map_err(|error| sqlite_io(&error))?;
    while let Some(row) = rows.next().map_err(|error| sqlite_io(&error))? {
        let domain: String = row
            .get::<_, Option<String>>(0)
            .map_err(|error| sqlite_io(&error))?
            .ok_or_else(|| StorageError::Corrupt("oversized stored domain".into()))?;
        validate_identifier("domain", &domain)
            .map_err(|_| StorageError::Corrupt("invalid stored domain".into()))?;
        let version: u32 = row.get(3).map_err(|_| {
            StorageError::Corrupt(format!("domain `{domain}` has an invalid stored version"))
        })?;
        if version == 0 {
            return Err(StorageError::Corrupt(format!(
                "domain `{domain}` has an invalid stored version"
            )));
        }
        let stored = checked_stored_size(
            &domain,
            row.get(1).map_err(|error| sqlite_io(&error))?,
            row.get(2).map_err(|error| sqlite_io(&error))?,
        )?;
        let mut measured = RecordObjectSize::default();
        let mut entries = records
            .query(params![domain, identifier_limit, record_limit])
            .map_err(|error| sqlite_io(&error))?;
        while let Some(row) = entries.next().map_err(|error| sqlite_io(&error))? {
            let key: String = row
                .get::<_, Option<String>>(0)
                .map_err(|error| sqlite_io(&error))?
                .ok_or_else(|| StorageError::Corrupt("oversized stored record key".into()))?;
            validate_identifier("record key", &key)
                .map_err(|_| StorageError::Corrupt("invalid stored record key".into()))?;
            let length: i64 = row.get(1).map_err(|error| sqlite_io(&error))?;
            let length = usize::try_from(length).map_err(|_| domain_byte_bound(&domain))?;
            if length > MAXIMUM_STORAGE_VALUE_BYTES {
                return Err(StorageError::Corrupt(format!(
                    "domain `{domain}` contains an oversized stored value"
                )));
            }
            measured = measured
                .with_entry(None, encoded_entry_bytes(&key, length)?)
                .map_err(|_| domain_byte_bound(&domain))?;
            if measured.records() > MAXIMUM_STORAGE_RECORDS {
                return Err(domain_record_bound(&domain));
            }
            if measured.bytes() > MAXIMUM_STORAGE_DOMAIN_BYTES {
                return Err(domain_byte_bound(&domain));
            }
        }
        if measured != stored {
            return Err(StorageError::Corrupt(format!(
                "domain `{domain}` has inconsistent record-accounting metadata"
            )));
        }
    }
    Ok(())
}

fn insert_domain(
    transaction: &Transaction<'_>,
    domain: &str,
    version: u32,
) -> Result<(), StorageError> {
    let inserted = transaction
        .execute(
            "INSERT INTO rsi_storage_domains(domain, version) VALUES (?1, ?2)
         ON CONFLICT(domain) DO NOTHING",
            params![domain, version],
        )
        .map_err(|error| sqlite_io(&error))?;
    if inserted != 0 && bounded_domain_count(transaction)? > MAXIMUM_SQLITE_DOMAINS {
        return Err(StorageError::InvalidInput(format!(
            "SQLite domain count would exceed {MAXIMUM_SQLITE_DOMAINS}"
        )));
    }
    Ok(())
}

struct StoredRecord {
    size: RecordObjectSize,
    entry_bytes: Option<usize>,
}

fn stored_record(
    connection: &Connection,
    domain: &str,
    version: u32,
    key: &str,
) -> Result<Option<StoredRecord>, StorageError> {
    let Some((actual, records, bytes, length)) = connection
        .query_row(
            "SELECT d.version, d.record_count, d.record_bytes, length(r.value)
             FROM rsi_storage_domains d
             LEFT JOIN rsi_storage_records r ON r.domain=d.domain AND r.key=?2
             WHERE d.domain=?1",
            params![domain, key],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_io(&error))?
    else {
        return Ok(None);
    };
    if actual != version {
        return Err(StorageError::Corrupt(format!(
            "domain `{domain}` has version {actual}, expected {version}"
        )));
    }
    let size = checked_stored_size(domain, records, bytes)?;
    let entry_bytes = length
        .map(|length| {
            let length = usize::try_from(length).map_err(|_| domain_byte_bound(domain))?;
            encoded_entry_bytes(key, length)
        })
        .transpose()?;
    Ok(Some(StoredRecord { size, entry_bytes }))
}

fn publish_size(
    connection: &Connection,
    domain: &str,
    size: RecordObjectSize,
) -> Result<(), StorageError> {
    let bytes = i64::try_from(size.bytes()).map_err(|_| domain_byte_bound(domain))?;
    connection
        .execute(
            "UPDATE rsi_storage_domains SET record_bytes=?2 WHERE domain=?1",
            params![domain, bytes],
        )
        .map_err(|error| sqlite_io(&error))?;
    Ok(())
}

fn sqlite_io(error: &rusqlite::Error) -> StorageError {
    match error {
        rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::FromSqlConversionFailure(..) => StorageError::Corrupt(error.to_string()),
        _ => StorageError::Io(error.to_string()),
    }
}

#[cfg(unix)]
fn ensure_private_database_file(path: &std::path::Path) -> Result<(), StorageError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    match options.open(path) {
        Ok(file) => validate_and_privatize_open_file(path, &file, "SQLite database"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing_private_file(path, "SQLite database")
        }
        Err(error) => Err(StorageError::Io(error.to_string())),
    }
}

#[cfg(not(unix))]
fn ensure_private_database_file(_path: &std::path::Path) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(unix)]
fn set_sqlite_sidecar_permissions(path: &std::path::Path) -> Result<(), StorageError> {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = std::path::PathBuf::from(sidecar);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => open_existing_private_file(&sidecar, "SQLite sidecar")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::Io(error.to_string())),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_existing_private_file(path: &std::path::Path, label: &str) -> Result<(), StorageError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| StorageError::Io(error.to_string()))?;
    validate_and_privatize_open_file(path, &file, label)
}

#[cfg(unix)]
fn validate_and_privatize_open_file(
    path: &std::path::Path,
    file: &std::fs::File,
    label: &str,
) -> Result<(), StorageError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let path_metadata =
        std::fs::symlink_metadata(path).map_err(|error| StorageError::Io(error.to_string()))?;
    let file_metadata = file
        .metadata()
        .map_err(|error| StorageError::Io(error.to_string()))?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !file_metadata.file_type().is_file()
    {
        return Err(StorageError::InvalidInput(format!(
            "{label} must be a real regular file"
        )));
    }
    if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino() {
        return Err(StorageError::InvalidInput(format!(
            "{label} changed while opening"
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn set_sqlite_sidecar_permissions(_path: &std::path::Path) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_uses_one_wal_snapshot_while_another_backend_commits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("snapshot.sqlite3");
        let first = SqliteBackend::open(&path).unwrap();
        first
            .put("domain", 1, "old", &Value::Bool(true))
            .await
            .unwrap();
        let other = SqliteBackend::open(&path).unwrap();
        {
            let mut connection = first.connection.lock().unwrap();
            assert_eq!(
                connection
                    .query_row("PRAGMA synchronous", [], |row| row.get::<_, u32>(0))
                    .unwrap(),
                2
            );
            let loaded = load_domain(&mut connection, "domain", || {
                let mut writer = other.connection.lock().unwrap();
                let transaction = writer
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .unwrap();
                transaction
                    .execute(
                        "UPDATE rsi_storage_records SET key='new' WHERE domain='domain'",
                        [],
                    )
                    .unwrap();
                transaction.commit().unwrap();
            })
            .unwrap()
            .unwrap();
            assert_eq!(
                loaded.records,
                BTreeMap::from([("old".into(), Value::Bool(true))])
            );
        }
        assert_eq!(
            first.load("domain").await.unwrap().unwrap().records,
            BTreeMap::from([("new".into(), Value::Bool(true))])
        );
        first.operation.close().await;
        other.operation.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_backend_writers_serialize_accounting_before_read_modify_write() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("writers.sqlite3");
        let first = SqliteBackend::open(&path).unwrap();
        let second = SqliteBackend::open(&path).unwrap();
        let (a, b) = tokio::join!(
            first.put("domain", 1, "a", &Value::Bool(true)),
            second.put("domain", 1, "b", &Value::Bool(false))
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(
            first.load("domain").await.unwrap().unwrap().records.len(),
            2
        );
        assert_eq!(
            second.load("domain").await.unwrap().unwrap().records.len(),
            2
        );
        first.operation.close().await;
        second.operation.close().await;
    }

    fn seed_empty_domains(path: &Path, count: i64) {
        drop(SqliteBackend::open(path).unwrap());
        let mut connection = Connection::open(path).unwrap();
        let transaction = connection.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO rsi_storage_domains(domain,version) VALUES (?1,1)")
                .unwrap();
            for index in 0..count {
                insert.execute([format!("domain-{index:04}")]).unwrap();
            }
        }
        transaction.commit().unwrap();
    }

    #[tokio::test]
    async fn namespace_ceiling_keeps_existing_domains_writable_and_reopens() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domains.sqlite3");
        seed_empty_domains(&path, MAXIMUM_SQLITE_DOMAINS - 1);
        let backend = SqliteBackend::open(&path).unwrap();
        backend.put("final", 1, "a", &Value::Null).await.unwrap();
        assert!(matches!(
            backend.put("excess", 1, "a", &Value::Null).await,
            Err(StorageError::InvalidInput(reason)) if reason.contains("domain count")
        ));
        assert!(backend.load("excess").await.unwrap().is_none());
        backend
            .put("final", 1, "a", &Value::Bool(true))
            .await
            .unwrap();
        backend
            .put("domain-0000", 1, "b", &Value::Null)
            .await
            .unwrap();
        backend.delete("final", 1, "a").await.unwrap();
        assert!(matches!(
            backend.put("excess", 1, "a", &Value::Null).await,
            Err(StorageError::InvalidInput(reason)) if reason.contains("domain count")
        ));
        backend
            .put("final", 1, "c", &Value::Bool(false))
            .await
            .unwrap();
        backend.operation.close().await;
        drop(backend);
        let backend = SqliteBackend::open(&path).unwrap();
        assert_eq!(
            backend.load("final").await.unwrap().unwrap().records,
            BTreeMap::from([("c".into(), Value::Bool(false))])
        );
        assert_eq!(
            backend.load("domain-0000").await.unwrap().unwrap().records,
            BTreeMap::from([("b".into(), Value::Null)])
        );
        assert!(backend.load("excess").await.unwrap().is_none());
        let count: i64 = backend
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM rsi_storage_domains", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MAXIMUM_SQLITE_DOMAINS);
        backend.operation.close().await;
    }

    #[test]
    fn excess_durable_domains_are_rejected_before_record_validation_without_rewrites() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domains.sqlite3");
        seed_empty_domains(&path, MAXIMUM_SQLITE_DOMAINS + 1);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE rsi_storage_domains SET version=0 WHERE domain='domain-0000';").unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(SqliteBackend::open(&path),
            Err(StorageError::Corrupt(reason)) if reason.contains("domain count")));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn recomputed_record_overflow_reports_record_count() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_or_validate_schema(&connection).unwrap();
        connection.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO rsi_storage_domains(domain,version) VALUES ('domain',1);").unwrap();
        let transaction = connection.transaction().unwrap();
        {
            let mut insert = transaction.prepare("INSERT INTO rsi_storage_records(domain,key,value) VALUES ('domain',?1,X'6e756c6c')").unwrap();
            for index in 0..=MAXIMUM_STORAGE_RECORDS {
                insert.execute([format!("record-{index:05}")]).unwrap();
            }
        }
        transaction
            .execute(
                "UPDATE rsi_storage_domains SET record_count=?1,record_bytes=2097152",
                [i64::try_from(MAXIMUM_STORAGE_RECORDS).unwrap()],
            )
            .unwrap();
        transaction.commit().unwrap();
        assert!(matches!(validate_accounting(&connection),
            Err(StorageError::Corrupt(reason)) if reason.contains("record count")));
    }

    #[test]
    fn activation_rejects_invalid_durable_versions_counters_and_identifiers_without_rewrites() {
        for (update, message) in [
            (
                "UPDATE rsi_storage_domains SET version = 4294967296",
                "version",
            ),
            ("UPDATE rsi_storage_domains SET version = 0", "version"),
            ("UPDATE rsi_storage_domains SET version = -1", "version"),
            (
                "UPDATE rsi_storage_domains SET record_count = -1",
                "record count",
            ),
            (
                "UPDATE rsi_storage_domains SET record_count = 65537",
                "record count",
            ),
            ("UPDATE rsi_storage_domains SET record_bytes = -1", "byte"),
            (
                "UPDATE rsi_storage_domains SET record_bytes = 268435457",
                "byte",
            ),
            (
                "UPDATE rsi_storage_domains SET domain = printf('%0257d', 0)",
                "oversized stored domain",
            ),
            (
                "INSERT INTO rsi_storage_records(domain,key,value) VALUES ('domain',printf('%0257d',0),X'6e756c6c'); UPDATE rsi_storage_domains SET record_bytes=266",
                "oversized stored record key",
            ),
            (
                "UPDATE rsi_storage_domains SET domain='invalid / domain'",
                "invalid stored domain",
            ),
            (
                "INSERT INTO rsi_storage_records(domain,key,value) VALUES ('domain','invalid/key',X'6e756c6c'); UPDATE rsi_storage_domains SET record_bytes=20",
                "invalid stored record key",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("invalid.sqlite3");
            drop(SqliteBackend::open(&path).unwrap());
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("PRAGMA ignore_check_constraints = ON; INSERT INTO rsi_storage_domains(domain,version) VALUES ('domain',1);").unwrap();
            connection.execute_batch(update).unwrap();
            drop(connection);
            let before = std::fs::read(&path).unwrap();
            assert!(
                matches!(SqliteBackend::open(&path), Err(StorageError::Corrupt(reason)) if reason.contains(message)),
                "{update}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{update}");
        }
    }
    #[tokio::test]
    async fn accounting_activation_preserves_complete_bounded_identifiers() {
        for identifier in [
            "a".repeat(MAXIMUM_STORAGE_IDENTIFIER_BYTES),
            "a".repeat(MAXIMUM_STORAGE_IDENTIFIER_BYTES / 2 + 1),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("identifiers.sqlite3");
            let backend = SqliteBackend::open(&path).unwrap();
            backend
                .put(&identifier, 1, &identifier, &Value::Null)
                .await
                .unwrap();
            backend.operation.close().await;
            drop(backend);
            let backend = SqliteBackend::open(&path).unwrap();
            let stored = backend.load(&identifier).await.unwrap().unwrap();
            assert_eq!(stored.records.get(&identifier), Some(&Value::Null));
            backend.operation.close().await;
        }
    }
    #[tokio::test]
    async fn offline_accounting_corruption_is_rejected_before_raw_mutation_admission() {
        for (column, corrupt) in [
            ("record_bytes", 6),
            ("record_bytes", 1024),
            ("record_count", 2),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("offline.sqlite3");
            let backend = SqliteBackend::open(&path).unwrap();
            backend
                .put("domain", 1, "key", &Value::String("x".repeat(64)))
                .await
                .unwrap();
            backend.operation.close().await;
            drop(backend);
            let connection = Connection::open(&path).unwrap();
            connection
                .execute(
                    &format!("UPDATE rsi_storage_domains SET {column}=?1"),
                    [corrupt],
                )
                .unwrap();
            drop(connection);
            let before = std::fs::read(&path).unwrap();
            let reopened = SqliteBackend::open(&path);
            assert!(
                matches!(reopened, Err(StorageError::Corrupt(_))),
                "offline {column} corruption reached a live backend"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
    #[tokio::test]
    async fn corrupt_accounting_is_rejected_and_old_schema_is_preserved() {
        const OLD_DOMAINS: &str = "CREATE TABLE rsi_storage_domains (
            domain TEXT PRIMARY KEY NOT NULL,
            version INTEGER NOT NULL CHECK(version > 0),
            record_count INTEGER NOT NULL DEFAULT 0 CHECK(record_count >= 0 AND record_count <= 65536)
        ) STRICT";
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("accounting.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        backend.put("domain", 1, "key", &Value::Null).await.unwrap();
        backend
            .connection
            .lock()
            .unwrap()
            .execute("UPDATE rsi_storage_domains SET record_bytes=2", [])
            .unwrap();
        assert!(matches!(
            backend.load("domain").await,
            Err(StorageError::Corrupt(message))
                if message.contains("`domain`") && message.contains("record-accounting metadata")
        ));
        drop(backend);
        let old = root.path().join("old.sqlite3");
        let connection = Connection::open(&old).unwrap();
        connection
            .execute_batch(&schema_with_domains(OLD_DOMAINS))
            .unwrap();
        connection
            .execute(
                "INSERT INTO rsi_storage_domains(domain,version) VALUES ('kept',1)",
                [],
            )
            .unwrap();
        drop(connection);
        let before = std::fs::read(&old).unwrap();
        assert!(matches!(
            SqliteBackend::open(&old),
            Err(StorageError::Corrupt(_))
        ));
        assert_eq!(std::fs::read(&old).unwrap(), before);
    }
    use std::sync::{Condvar, Mutex as StdMutex};

    struct BusyProbe {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        released: StdMutex<bool>,
        release_changed: Condvar,
    }

    static BUSY_PROBE: StdMutex<Option<Arc<BusyProbe>>> = StdMutex::new(None);

    fn deny_rollback(connection: &Connection) {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
        connection
            .authorizer(Some(|context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Transaction {
                        operation: TransactionOperation::Rollback
                    }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))
            .unwrap();
    }

    fn gated_busy_handler(_attempt: i32) -> bool {
        let probe = BUSY_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .cloned()
            .expect("test busy handler requires an installed probe");
        let _ = probe.entered.send(());
        let released = probe
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (released, _) = probe
            .release_changed
            .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A failed test must not leave a blocking SQLite worker holding shutdown.
        *released
    }

    #[tokio::test]
    async fn body_and_commit_rejection_preserve_admission_after_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        assert!(matches!(
            backend
                .transaction(|transaction| {
                    insert_domain(transaction, "rolled-back", 1)?;
                    Err(StorageError::Io("injected body failure".into()))
                })
                .await,
            Err(StorageError::Io(_))
        ));
        assert!(backend.load("rolled-back").await.unwrap().is_none());
        backend.put("kept", 1, "a", &Value::Null).await.unwrap();
        let result = backend.transaction(|transaction| {
            transaction.execute_batch(
                "PRAGMA defer_foreign_keys=ON;
                 INSERT INTO rsi_storage_records(domain,key,value) VALUES ('absent','a',x'6e756c6c')"
            ).map_err(|error| sqlite_io(&error))?;
            Ok(())
        }).await;
        assert!(matches!(result, Err(StorageError::Io(_))), "{result:?}");
        assert!(backend.connection.lock().unwrap().is_autocommit());
        assert!(backend.load("absent").await.unwrap().is_none());
        backend.put("kept", 1, "b", &Value::Null).await.unwrap();
        backend.operation.close().await;
        drop(backend);
        let reopened = SqliteBackend::open(&path).unwrap();
        assert!(reopened.load("absent").await.unwrap().is_none());
        assert_eq!(
            reopened.load("kept").await.unwrap().unwrap().records.len(),
            2
        );
    }

    #[tokio::test]
    async fn commit_busy_rolls_back_before_the_connection_accepts_another_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        backend.put("kept", 1, "a", &Value::Null).await.unwrap();
        {
            let connection = backend.connection.lock().unwrap();
            // A rollback-journal reader permits the write body but blocks COMMIT.
            connection
                .execute_batch("PRAGMA journal_mode=DELETE")
                .unwrap();
            connection.busy_timeout(Duration::ZERO).unwrap();
        }
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT * FROM rsi_storage_records;")
            .unwrap();
        let result = backend
            .transaction(|transaction| {
                insert_domain(transaction, "rejected", 1)?;
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(StorageError::Io(_))), "{result:?}");
        assert!(backend.connection.lock().unwrap().is_autocommit());
        reader.execute_batch("ROLLBACK").unwrap();
        assert!(backend.load("rejected").await.unwrap().is_none());
        backend.put("kept", 1, "b", &Value::Null).await.unwrap();
    }

    #[tokio::test]
    async fn wal_error_after_commit_fences_even_when_its_code_resembles_rejection() {
        use rusqlite::{ffi, hooks::Wal};
        type Hook = fn(&Wal, i32) -> rusqlite::Result<()>;
        let hooks: [Hook; 3] = [
            |_, _| {
                Err(rusqlite::Error::SqliteFailure(
                    ffi::Error::new(ffi::SQLITE_IOERR_FSYNC),
                    None,
                ))
            },
            |_, _| {
                Err(rusqlite::Error::SqliteFailure(
                    ffi::Error::new(ffi::SQLITE_CONSTRAINT),
                    None,
                ))
            },
            |_, _| {
                Err(rusqlite::Error::SqliteFailure(
                    ffi::Error::new(ffi::SQLITE_BUSY),
                    None,
                ))
            },
        ];
        for hook in hooks {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("state.sqlite3");
            let backend = SqliteBackend::open(&path).unwrap();
            backend.put("kept", 1, "a", &Value::Null).await.unwrap();
            backend.connection.lock().unwrap().wal_hook(Some(hook));
            assert!(matches!(
                backend
                    .put("changed", 1, "visible", &Value::Bool(true))
                    .await,
                Err(StorageError::OutcomeUnknown(_))
            ));
            assert!(backend.connection.lock().unwrap().is_autocommit());
            assert_eq!(
                backend.load("kept").await,
                Err(StorageError::RecoveryRequired)
            );
            assert_eq!(
                backend.put("kept", 1, "b", &Value::Null).await,
                Err(StorageError::RecoveryRequired)
            );
            backend.operation.close().await;
            drop(backend);
            let reopened = SqliteBackend::open(&path).unwrap();
            assert_eq!(
                reopened.load("changed").await.unwrap().unwrap().records["visible"],
                Value::Bool(true)
            );
            assert_eq!(
                reopened.load("kept").await.unwrap().unwrap().records.len(),
                1
            );
        }
    }

    #[tokio::test]
    async fn transaction_panic_fences_before_any_poisoned_connection_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        backend.put("kept", 1, "a", &Value::Null).await.unwrap();
        let result = backend
            .transaction(|transaction| {
                insert_domain(transaction, "uncommitted", 1)?;
                panic!("injected transaction panic");
            })
            .await;
        assert!(matches!(result, Err(StorageError::OutcomeUnknown(_))));
        assert!(backend.connection.is_poisoned());
        assert_eq!(
            backend.load("kept").await,
            Err(StorageError::RecoveryRequired)
        );
        assert_eq!(
            backend.put("uncommitted", 1, "b", &Value::Null).await,
            Err(StorageError::RecoveryRequired)
        );
        backend.operation.close().await;
        drop(backend);
        let reopened = SqliteBackend::open(&path).unwrap();
        assert!(reopened.load("uncommitted").await.unwrap().is_none());
        assert!(
            reopened
                .load("kept")
                .await
                .unwrap()
                .unwrap()
                .records
                .contains_key("a")
        );
    }

    #[tokio::test]
    async fn rejected_rollback_fences_the_connection_and_reopen_recovers_durable_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        backend.put("kept", 1, "a", &Value::Null).await.unwrap();
        deny_rollback(&backend.connection.lock().unwrap());
        let result = backend
            .transaction(|transaction| {
                insert_domain(transaction, "uncommitted", 1)?;
                Err(StorageError::Io("injected body failure".into()))
            })
            .await;
        let Err(StorageError::OutcomeUnknown(message)) = result else {
            panic!("rollback failure must be unknown: {result:?}");
        };
        assert!(message.contains("injected body failure") && message.contains("rollback"));
        assert_eq!(
            backend.load("kept").await,
            Err(StorageError::RecoveryRequired)
        );
        backend.operation.close().await;
        drop(backend);
        let reopened = SqliteBackend::open(&path).unwrap();
        assert!(reopened.load("uncommitted").await.unwrap().is_none());
        assert_eq!(
            reopened.load("kept").await.unwrap().unwrap().records.len(),
            1
        );
    }

    #[tokio::test]
    async fn rejected_commit_with_failed_rollback_still_fences_the_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        backend.put("kept", 1, "a", &Value::Null).await.unwrap();
        deny_rollback(&backend.connection.lock().unwrap());
        let result = backend.transaction(|transaction| {
            insert_domain(transaction, "uncommitted", 1)?;
            transaction.execute_batch(
                "PRAGMA defer_foreign_keys=ON;
                 INSERT INTO rsi_storage_records(domain,key,value) VALUES ('absent','a',x'6e756c6c')"
            ).map_err(|error| sqlite_io(&error))
        }).await;
        let Err(StorageError::OutcomeUnknown(message)) = result else {
            panic!("failed rollback must fence: {result:?}");
        };
        assert!(message.contains("commit rejected") && message.contains("rollback"));
        assert_eq!(
            backend.load("kept").await,
            Err(StorageError::RecoveryRequired)
        );
        backend.operation.close().await;
        drop(backend);
        let reopened = SqliteBackend::open(&path).unwrap();
        assert!(reopened.load("uncommitted").await.unwrap().is_none());
        assert!(reopened.load("absent").await.unwrap().is_none());
        assert_eq!(
            reopened.load("kept").await.unwrap().unwrap().records.len(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_depth_survives_real_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let backend = SqliteBackend::open(&path).unwrap();
        let mut value = Value::Null;
        for depth in 1..rsi_storage::MAXIMUM_STORAGE_VALUE_DEPTH {
            value = if depth % 2 == 0 {
                serde_json::json!({"child":value})
            } else {
                Value::Array(vec![value])
            };
        }
        backend.put("domain", 1, "a", &value).await.unwrap();
        assert!(
            backend
                .put("domain", 1, "b", &Value::Array(vec![value.clone()]))
                .await
                .is_err()
        );
        backend.operation.close().await;
        let reopened = SqliteBackend::open(&path).unwrap();
        assert_eq!(
            reopened.load("domain").await.unwrap().unwrap().records["a"],
            value
        );
    }

    #[tokio::test]
    async fn writer_contention_waits_after_a_real_sqlite_busy_callback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("domains.sqlite3");
        let backend = Arc::new(SqliteBackend::open(&path).unwrap());
        {
            let connection = backend
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let timeout_ms = connection
                .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))
                .unwrap();
            assert_eq!(timeout_ms, 5_000);
        }

        let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let probe = Arc::new(BusyProbe {
            entered,
            released: StdMutex::new(false),
            release_changed: Condvar::new(),
        });
        {
            let mut installed = BUSY_PROBE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(installed.replace(Arc::clone(&probe)).is_none());
        }
        backend
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .busy_handler(Some(gated_busy_handler))
            .unwrap();

        let locking = Connection::open(&path).unwrap();
        locking.execute_batch("BEGIN IMMEDIATE").unwrap();
        let blocked_put = tokio::spawn({
            let backend = Arc::clone(&backend);
            async move {
                backend
                    .put("projection", 1, "busy", &Value::Bool(true))
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
            .await
            .expect("SQLite did not invoke the busy callback")
            .expect("busy probe closed before contention");
        assert!(
            !blocked_put.is_finished(),
            "the operation returned while its real busy callback was gated"
        );

        locking.execute_batch("COMMIT").unwrap();
        *probe
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        probe.release_changed.notify_all();
        tokio::time::timeout(Duration::from_secs(2), blocked_put)
            .await
            .expect("writer did not resume after releasing the real lock")
            .unwrap()
            .unwrap();
        backend
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .busy_handler(None)
            .unwrap();
        BUSY_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}
