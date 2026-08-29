//! `SQLite` backend for non-session storage domains.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage::{
    BackendLease, KvBackend, MAXIMUM_STORAGE_DOMAIN_BYTES, MAXIMUM_STORAGE_RECORDS,
    MAXIMUM_STORAGE_VALUE_BYTES, StorageError, StorageHubContract, StoredDomain,
    validate_identifier, validate_value,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const DOMAINS_TABLE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rsi_storage_domains (
  domain TEXT PRIMARY KEY NOT NULL,
  version INTEGER NOT NULL CHECK(version > 0),
  record_count INTEGER NOT NULL DEFAULT 0 CHECK(record_count >= 0 AND record_count <= 65536)
) STRICT";
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
const STORAGE_SCHEMA: &str = concat!(
    "CREATE TABLE IF NOT EXISTS rsi_storage_domains (domain TEXT PRIMARY KEY NOT NULL,version INTEGER NOT NULL CHECK(version > 0),record_count INTEGER NOT NULL DEFAULT 0 CHECK(record_count >= 0 AND record_count <= 65536)) STRICT;",
    "CREATE TABLE IF NOT EXISTS rsi_storage_records (domain TEXT NOT NULL,key TEXT NOT NULL,value BLOB NOT NULL,PRIMARY KEY(domain, key),FOREIGN KEY(domain) REFERENCES rsi_storage_domains(domain) ON DELETE CASCADE) STRICT;",
    "CREATE TRIGGER IF NOT EXISTS rsi_storage_records_insert_count AFTER INSERT ON rsi_storage_records BEGIN UPDATE rsi_storage_domains SET record_count = record_count + 1 WHERE domain = NEW.domain; END;",
    "CREATE TRIGGER IF NOT EXISTS rsi_storage_records_delete_count AFTER DELETE ON rsi_storage_records BEGIN UPDATE rsi_storage_domains SET record_count = record_count - 1 WHERE domain = OLD.domain; END;",
);

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
    operation: AsyncMutex<()>,
}

impl SqliteBackend {
    fn open(path: &PathBuf) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            create_private_directories(parent)?;
        }
        ensure_private_database_file(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| sqlite_io(&error))?;
        connection
            .busy_timeout(SQLITE_BUSY_TIMEOUT)
            .map_err(|error| sqlite_io(&error))?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .map_err(|error| sqlite_io(&error))?;
        initialize_or_validate_schema(&connection)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL")
            .map_err(|error| sqlite_io(&error))?;
        set_sqlite_sidecar_permissions(path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            operation: AsyncMutex::new(()),
        })
    }
}

#[async_trait]
impl KvBackend for SqliteBackend {
    async fn load(&self, domain: &str) -> Result<Option<StoredDomain>, StorageError> {
        validate_identifier("domain", domain)?;
        let _operation = self.operation.lock().await;
        let connection = Arc::clone(&self.connection);
        let domain = domain.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let stored_header = connection
                .query_row(
                    "SELECT version, record_count FROM rsi_storage_domains WHERE domain = ?1",
                    [&domain],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(|error| sqlite_io(&error))?;
            let Some((version, stored_record_count)) = stored_header else {
                return Ok(None);
            };
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
            let mut retained_bytes = serde_json::to_vec(&StoredDomain {
                version,
                records: BTreeMap::new(),
            })
            .map_err(|error| StorageError::Corrupt(error.to_string()))?
            .len();
            let mut statement = connection
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
                let entry_bytes = key
                    .len()
                    .checked_add(encoded_len)
                    .and_then(|length| length.checked_add(if records.is_empty() { 3 } else { 4 }))
                    .ok_or_else(|| domain_byte_bound(&domain))?;
                retained_bytes = retained_bytes
                    .checked_add(entry_bytes)
                    .ok_or_else(|| domain_byte_bound(&domain))?;
                if retained_bytes > MAXIMUM_STORAGE_DOMAIN_BYTES {
                    return Err(domain_byte_bound(&domain));
                }
                let bytes = row
                    .get::<_, Vec<u8>>(2)
                    .map_err(|error| sqlite_io(&error))?;
                let value = serde_json::from_slice(&bytes)
                    .map_err(|error| StorageError::Corrupt(error.to_string()))?;
                validate_value(&value)
                    .map_err(|_| StorageError::Corrupt("invalid stored value".into()))?;
                records.insert(key, value);
            }
            if records.len() != stored_record_count {
                return Err(StorageError::Corrupt(format!(
                    "domain `{domain}` has inconsistent record-count metadata"
                )));
            }
            Ok(Some(StoredDomain { version, records }))
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    async fn put(
        &self,
        domain: &str,
        version: u32,
        key: &str,
        value: &Value,
    ) -> Result<(), StorageError> {
        validate_identifier("domain", domain)?;
        validate_identifier("record key", key)?;
        validate_value(value)?;
        let bytes =
            serde_json::to_vec(value).map_err(|error| StorageError::Io(error.to_string()))?;
        let _operation = self.operation.lock().await;
        let connection = Arc::clone(&self.connection);
        let domain = domain.to_owned();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let transaction = connection
                .transaction()
                .map_err(|error| sqlite_io(&error))?;
            ensure_version(&transaction, &domain, version)?;
            let (record_count, key_exists) = transaction
                .query_row(
                    "SELECT record_count, EXISTS(
                       SELECT 1 FROM rsi_storage_records WHERE domain = ?1 AND key = ?2
                     ) FROM rsi_storage_domains WHERE domain = ?1",
                    params![domain, key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
                )
                .map_err(|error| sqlite_io(&error))?;
            let record_count = usize::try_from(record_count).map_err(|_| {
                StorageError::Corrupt(format!(
                    "domain `{domain}` has an invalid backend record count"
                ))
            })?;
            if record_count > MAXIMUM_STORAGE_RECORDS {
                return Err(StorageError::Corrupt(format!(
                    "domain `{domain}` exceeds the backend record bound"
                )));
            }
            if record_count == MAXIMUM_STORAGE_RECORDS && !key_exists {
                return Err(StorageError::InvalidInput(format!(
                    "domain `{domain}` reached the {MAXIMUM_STORAGE_RECORDS}-record bound"
                )));
            }
            transaction
                .execute(
                    "INSERT INTO rsi_storage_records(domain, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT(domain, key) DO UPDATE SET value = excluded.value",
                    params![domain, key, bytes],
                )
                .map_err(|error| sqlite_io(&error))?;
            transaction.commit().map_err(|error| sqlite_io(&error))
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    async fn delete(&self, domain: &str, version: u32, key: &str) -> Result<(), StorageError> {
        validate_identifier("domain", domain)?;
        validate_identifier("record key", key)?;
        let _operation = self.operation.lock().await;
        let connection = Arc::clone(&self.connection);
        let domain = domain.to_owned();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let transaction = connection
                .transaction()
                .map_err(|error| sqlite_io(&error))?;
            if transaction
                .query_row(
                    "SELECT version FROM rsi_storage_domains WHERE domain = ?1",
                    [&domain],
                    |row| row.get::<_, u32>(0),
                )
                .optional()
                .map_err(|error| sqlite_io(&error))?
                .is_some_and(|actual| actual != version)
            {
                return Err(StorageError::Corrupt(format!(
                    "domain `{domain}` has an incompatible schema version"
                )));
            }
            transaction
                .execute(
                    "DELETE FROM rsi_storage_records WHERE domain = ?1 AND key = ?2",
                    params![domain, key],
                )
                .map_err(|error| sqlite_io(&error))?;
            transaction.commit().map_err(|error| sqlite_io(&error))
        })
        .await
        .map_err(|error| join_error(&error))?
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
        let backend: Arc<dyn KvBackend> = Arc::new(backend);
        let lease: BackendLease = plan
            .local::<StorageHubContract>()?
            .register(&name, backend)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw SQLite storage backend",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

fn join_error(error: &tokio::task::JoinError) -> StorageError {
    StorageError::Io(format!("storage blocking task failed: {error}"))
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
    for (object_type, name, expected) in [
        ("table", "rsi_storage_domains", DOMAINS_TABLE_SCHEMA),
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
            .execute_batch(&format!("BEGIN IMMEDIATE; {STORAGE_SCHEMA} COMMIT;"))
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

fn ensure_version(
    transaction: &Transaction<'_>,
    domain: &str,
    version: u32,
) -> Result<(), StorageError> {
    if version == 0 {
        return Err(StorageError::InvalidInput(
            "domain version must be nonzero".into(),
        ));
    }
    transaction
        .execute(
            "INSERT INTO rsi_storage_domains(domain, version) VALUES (?1, ?2)
             ON CONFLICT(domain) DO NOTHING",
            params![domain, version],
        )
        .map_err(|error| sqlite_io(&error))?;
    let actual = transaction
        .query_row(
            "SELECT version FROM rsi_storage_domains WHERE domain = ?1",
            [domain],
            |row| row.get::<_, u32>(0),
        )
        .map_err(|error| sqlite_io(&error))?;
    if actual != version {
        return Err(StorageError::Corrupt(format!(
            "domain `{domain}` has version {actual}, expected {version}"
        )));
    }
    Ok(())
}

fn sqlite_io(error: &rusqlite::Error) -> StorageError {
    StorageError::Io(error.to_string())
}

#[cfg(unix)]
fn create_private_directories(path: &std::path::Path) -> Result<(), StorageError> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|error| StorageError::Io(error.to_string()))
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

#[cfg(not(unix))]
fn create_private_directories(path: &std::path::Path) -> Result<(), StorageError> {
    std::fs::create_dir_all(path).map_err(|error| StorageError::Io(error.to_string()))
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
