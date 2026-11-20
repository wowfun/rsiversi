//! `SQLite` backend for non-session storage domains.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage::{
    BackendLease, BackendOperations, Capabilities, KvBackend, KvUnit, Layout,
    MAXIMUM_STORAGE_DOMAIN_BYTES, MAXIMUM_STORAGE_IDENTIFIER_BYTES, MAXIMUM_STORAGE_RECORDS,
    MAXIMUM_STORAGE_VALUE_BYTES, Mutation, OpenedUnit, RecordObjectSize, StorageError,
    StorageHubContract, UnitDescriptor, UnitLease, UnitNames, admit_mutations,
    create_private_directories, validate_identifier,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Durable backend layout version, independent of consumer domain versions.
pub const SQLITE_STORAGE_FORMAT: u32 = 2;
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
  table_name TEXT NOT NULL,
  key TEXT NOT NULL,
  value BLOB NOT NULL,
  CHECK(table_name != '' OR key = ''),
  PRIMARY KEY(domain, table_name, key),
  FOREIGN KEY(domain) REFERENCES rsi_storage_domains(domain) ON DELETE CASCADE
) STRICT";
fn schema_with_domains(domains: &str) -> String {
    format!("{domains}; {RECORDS_TABLE_SCHEMA};")
}
mod unit;

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
        #[cfg(windows)]
        rsi_files_native_fs::resolve_absolute_root_alias(&self.path, true)
            .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
        require_native_platform()
    }
}

#[derive(Clone, Debug)]
struct SqliteBackend {
    connection: Arc<Mutex<ConnectionOwner>>,
    operation: Arc<BackendOperations>,
    units: Arc<UnitNames>,
}

#[derive(Debug)]
struct ConnectionOwner {
    connection: Connection,
    #[cfg(windows)]
    _namespace: rsi_files_native_fs::NamespaceLease,
    #[cfg(windows)]
    _database: std::fs::File,
}
impl std::ops::Deref for ConnectionOwner {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}
impl std::ops::DerefMut for ConnectionOwner {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

impl SqliteBackend {
    fn open(path: &Path) -> Result<Self, StorageError> {
        require_native_platform()?;
        let parent = path
            .parent()
            .ok_or_else(|| StorageError::Io("database has no parent".into()))?;
        create_private_directories(parent)?;
        #[cfg(not(windows))]
        let parent =
            std::fs::canonicalize(parent).map_err(|error| StorageError::Io(error.to_string()))?;
        #[cfg(windows)]
        let parent = parent.to_owned();
        #[cfg(windows)]
        let namespace = rsi_files_native_fs::open_absolute_directory_no_follow(&parent)
            .and_then(|directory| directory.pin_namespace())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| StorageError::Io("database has no file name".into()))?,
        );
        #[cfg(not(windows))]
        ensure_private_database_file(&path)?;
        #[cfg(windows)]
        let database = private_windows_file(&path, true)?;
        #[cfg(windows)]
        prepare_windows_sidecars(&path)?;
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
        unit::validate_accounting(&connection)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL")
            .map_err(|error| sqlite_io(&error))?;
        set_sqlite_sidecar_permissions(&path)?;
        #[cfg(windows)]
        namespace
            .sync_all()
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(Self {
            connection: Arc::new(Mutex::new(ConnectionOwner {
                connection,
                #[cfg(windows)]
                _namespace: namespace,
                #[cfg(windows)]
                _database: database,
            })),
            operation: Arc::new(BackendOperations::default()),
            units: Arc::default(),
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

#[async_trait]
impl KvBackend for SqliteBackend {
    fn ensure_available(&self) -> Result<(), StorageError> {
        self.operation.ensure_available()
    }
    fn capabilities(&self, _: Layout) -> Capabilities {
        Capabilities {
            atomic_batch: true,
            backup_record: false,
        }
    }
    async fn open(&self, descriptor: UnitDescriptor) -> Result<OpenedUnit, StorageError> {
        self.open_unit(descriptor, || {}).await
    }
}
impl SqliteBackend {
    async fn open_unit(
        &self,
        descriptor: UnitDescriptor,
        after_header: impl FnOnce() + Send + 'static,
    ) -> Result<OpenedUnit, StorageError> {
        let slot = self.operation.acquire().await?;
        let lease = self.units.reserve(descriptor.name())?;
        let descriptor = Arc::new(descriptor);
        let retained = Arc::clone(&descriptor);
        let connection = Arc::clone(&self.connection);
        let worker_lease = lease.clone();
        let snapshot = slot
            .run(move || {
                let _lease = worker_lease;
                let mut connection = connection
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                unit::load(&mut connection, &retained, after_header)
            })
            .await?;
        Ok(OpenedUnit {
            snapshot,
            unit: Arc::new(SqliteUnit {
                backend: self.clone(),
                descriptor,
                lease,
            }),
        })
    }
}

#[derive(Debug)]
struct SqliteUnit {
    backend: SqliteBackend,
    descriptor: Arc<UnitDescriptor>,
    lease: UnitLease,
}
#[async_trait]
impl KvUnit for SqliteUnit {
    fn descriptor(&self) -> &UnitDescriptor {
        &self.descriptor
    }
    fn ensure_available(&self) -> Result<(), StorageError> {
        self.backend.ensure_available()
    }
    async fn write(&self, changes: Vec<Mutation>) -> Result<(), StorageError> {
        self.ensure_available()?;
        admit_mutations(
            &self.descriptor,
            &changes,
            self.backend.capabilities(self.descriptor.layout()),
        )?;
        let descriptor = Arc::clone(&self.descriptor);
        let lease = self.lease.clone();
        self.backend
            .transaction(move |transaction| {
                let _lease = lease;
                unit::write(transaction, &descriptor, changes)
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
        config.validate().map_err(|error| match error {
            StorageError::Unsupported(_) => MetaError::Activation(error.to_string()),
            _ => MetaError::InvalidInput(error.to_string()),
        })?;
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
    let mut missing = vec![
        ("index", "sqlite_autoindex_rsi_storage_domains_1"),
        ("index", "sqlite_autoindex_rsi_storage_records_1"),
        ("table", "rsi_storage_domains"),
        ("table", "rsi_storage_records"),
    ];
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(CAST(type AS BLOB)) <= ?1 THEN type END,
                    CASE WHEN length(CAST(name AS BLOB)) <= ?2 THEN name END FROM sqlite_master
             WHERE name GLOB 'rsi_storage_*'
                OR tbl_name IN ('rsi_storage_domains', 'rsi_storage_records') LIMIT ?3",
        )
        .map_err(|error| sqlite_io(&error))?;
    let sql_bound = |n| i64::try_from(n).expect("writer schema bound fits SQLite integer");
    let mut rows = statement
        .query(params![
            sql_bound(
                missing
                    .iter()
                    .map(|(kind, _)| kind.len())
                    .max()
                    .unwrap_or(0)
            ),
            sql_bound(
                missing
                    .iter()
                    .map(|(_, name)| name.len())
                    .max()
                    .unwrap_or(0)
            ),
            sql_bound(missing.len() + 1),
        ])
        .map_err(|error| sqlite_io(&error))?;
    let incompatible =
        || StorageError::Corrupt("SQLite storage has an incompatible schema object set".into());
    while let Some(row) = rows.next().map_err(|error| sqlite_io(&error))? {
        let text = |column| {
            row.get_ref(column)
                .map_err(|error| sqlite_io(&error))?
                .as_str()
                .map_err(|_| incompatible())
        };
        let entry = (text(0)?, text(1)?);
        let index = missing
            .iter()
            .position(|&expected| expected == entry)
            .ok_or_else(incompatible)?;
        missing.swap_remove(index);
    }
    if !missing.is_empty() {
        return Err(incompatible());
    }
    let domains_schema = domains_table_schema();
    for (object_type, name, expected) in [
        ("table", "rsi_storage_domains", domains_schema.as_str()),
        ("table", "rsi_storage_records", RECORDS_TABLE_SCHEMA),
    ] {
        // SQLite removes this fixed writer prefix when saving CREATE TABLE SQL.
        let expected = expected.replacen("CREATE TABLE IF NOT EXISTS ", "CREATE TABLE ", 1);
        let matches = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2 AND sql=?3 COLLATE BINARY)",
                params![object_type, name, expected],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| sqlite_io(&error))?;
        if !matches {
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
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE name GLOB 'rsi_storage_*'
                 OR tbl_name IN ('rsi_storage_domains', 'rsi_storage_records'))",
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
    RecordObjectSize::from_parts(records, bytes).map_err(|_| unit::inconsistent(domain))
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

#[cfg(not(any(unix, windows)))]
fn ensure_private_database_file(_path: &std::path::Path) -> Result<(), StorageError> {
    require_native_platform()
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

#[cfg(not(any(unix, windows)))]
fn set_sqlite_sidecar_permissions(_path: &std::path::Path) -> Result<(), StorageError> {
    require_native_platform()
}

fn require_native_platform() -> Result<(), StorageError> {
    if cfg!(any(unix, windows)) {
        Ok(())
    } else {
        Err(StorageError::Unsupported(
            "SQLite native filesystem protection is unsupported on this platform".into(),
        ))
    }
}

#[cfg(windows)]
fn private_windows_file(path: &Path, create: bool) -> Result<std::fs::File, StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::InvalidInput("database file has no parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| StorageError::InvalidInput("database file has no name".into()))?;
    let directory = rsi_files_native_fs::open_absolute_directory_no_follow(parent)
        .map_err(|error| StorageError::Io(error.to_string()))?;
    if create {
        match rsi_files_native_fs::create_relative_private_file_no_follow(
            &directory,
            Path::new(name),
        ) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StorageError::Io(error.to_string())),
        }
    }
    rsi_files_native_fs::set_relative_private_file_permissions_no_follow(
        &directory,
        Path::new(name),
    )
    .map_err(|error| StorageError::Io(error.to_string()))?;
    rsi_files_native_fs::open_relative_private_file_no_follow(&directory, Path::new(name))
        .map_err(|error| StorageError::Io(error.to_string()))
}
#[cfg(windows)]
fn prepare_windows_sidecars(path: &Path) -> Result<(), StorageError> {
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        private_windows_file(&PathBuf::from(name), true)?;
    }
    Ok(())
}

#[cfg(windows)]
fn set_sqlite_sidecar_permissions(path: &Path) -> Result<(), StorageError> {
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => {
                private_windows_file(&sidecar, false)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::Io(error.to_string())),
        }
    }
    Ok(())
}

// Fault-injection tests below address the SQL engine directly. These helpers
// keep their fixtures small while exercising the same unit load/write bodies.
#[cfg(test)]
fn test_unit(name: &str, version: u32) -> UnitDescriptor {
    UnitDescriptor::new(
        name.into(),
        version,
        BTreeSet::new(),
        BTreeMap::from([("rows".into(), rsi_storage::RecordPolicy::Reject)]),
        false,
        Layout::Single,
    )
    .unwrap()
}
#[cfg(test)]
fn test_records(snapshot: rsi_storage::UnitSnapshot) -> BTreeMap<String, Value> {
    snapshot
        .records
        .into_iter()
        .map(|(address, value)| (address.key().to_owned(), value.decode().unwrap()))
        .collect()
}
#[cfg(test)]
#[derive(Debug, PartialEq)]
struct TestRows {
    records: BTreeMap<String, Value>,
}
#[cfg(test)]
impl SqliteBackend {
    async fn load(&self, name: &str) -> Result<Option<TestRows>, StorageError> {
        let opened = <Self as KvBackend>::open(self, test_unit(name, 1)).await?;
        Ok(opened.snapshot.version.map(|_| TestRows {
            records: test_records(opened.snapshot),
        }))
    }
    async fn put(
        &self,
        name: &str,
        version: u32,
        key: &str,
        value: &Value,
    ) -> Result<(), StorageError> {
        self.ensure_available()?;
        let descriptor = test_unit(name, version);
        let change = Mutation {
            address: rsi_storage::Address::record("rows", key)?,
            value: Some(rsi_storage::EncodedValue::encode(value)?),
        };
        self.transaction(move |tx| unit::write(tx, &descriptor, vec![change]))
            .await
    }
    #[cfg(unix)]
    async fn delete(&self, name: &str, version: u32, key: &str) -> Result<(), StorageError> {
        let descriptor = test_unit(name, version);
        let change = Mutation {
            address: rsi_storage::Address::record("rows", key)?,
            value: None,
        };
        self.transaction(move |tx| unit::write(tx, &descriptor, vec![change]))
            .await
    }
}

#[cfg(all(test, not(any(unix, windows))))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn acquisition_rejects_before_creating_parents_or_touching_an_existing_database() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing/database.sqlite3");
        assert!(matches!(
            SqliteBackend::open(&missing),
            Err(StorageError::Unsupported(_))
        ));
        assert!(!missing.parent().unwrap().exists());
        let existing = root.path().join("existing.sqlite3");
        std::fs::write(&existing, b"retained").unwrap();
        assert!(matches!(
            SqliteBackend::open(&existing),
            Err(StorageError::Unsupported(_))
        ));
        assert_eq!(std::fs::read(existing).unwrap(), b"retained");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod config_validation_tests {
    use super::*;

    #[test]
    fn malformed_configuration_precedes_platform_admission() {
        let root = tempfile::tempdir().unwrap();
        for (name, path) in [
            ("valid", PathBuf::from("relative.sqlite3")),
            ("bad/name", root.path().join("missing/database.sqlite3")),
        ] {
            assert!(matches!(
                SqliteStorageConfig {
                    name: name.into(),
                    path
                }
                .validate(),
                Err(StorageError::InvalidInput(_))
            ));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn valid_configuration_checks_platform_without_filesystem_effects() {
        let root = tempfile::tempdir().unwrap();
        let result = SqliteStorageConfig {
            name: "valid".into(),
            path: root.path().join("missing/database.sqlite3"),
        }
        .validate();
        if cfg!(any(unix, windows)) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(StorageError::Unsupported(_))));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[tokio::test]
    async fn native_private_database_persists_with_independent_connections_and_namespace_pins() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("private-storage");
        let path = parent.join("domains.sqlite3");
        let first = SqliteBackend::open(&path).unwrap();
        first
            .put("fixture", 1, "first", &serde_json::json!({"value": 21}))
            .await
            .unwrap();
        let directory = rsi_files_native_fs::open_absolute_directory_no_follow(&parent).unwrap();
        for sidecar in ["domains.sqlite3-wal", "domains.sqlite3-shm"] {
            rsi_files_native_fs::open_relative_private_file_no_follow(
                &directory,
                Path::new(sidecar),
            )
            .expect("SQLite sidecars retain the exact private owner and protected DACL");
        }
        drop(directory);
        let second = SqliteBackend::open(&path).unwrap();
        second
            .put("fixture", 1, "second", &serde_json::json!({"value": 22}))
            .await
            .unwrap();
        assert_eq!(
            first.load("fixture").await.unwrap().unwrap().records.len(),
            2
        );
        assert!(std::fs::rename(&parent, temporary.path().join("replaced")).is_err());
        first.operation.close().await;
        drop(first);
        assert!(std::fs::rename(&parent, temporary.path().join("replaced")).is_err());
        second.operation.close().await;
        drop(second);
        let reopened = SqliteBackend::open(&path).unwrap();
        assert_eq!(
            reopened.load("fixture").await.unwrap().unwrap().records["first"],
            serde_json::json!({"value":21})
        );
        reopened.operation.close().await;
        drop(reopened);
        std::fs::rename(&parent, temporary.path().join("replaced")).unwrap();
    }

    #[test]
    fn private_directory_admission_does_not_repair_an_unowned_parent_policy() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("domains.sqlite3");
        assert!(SqliteBackend::open(&path).is_err());
        assert!(!path.exists());
    }
}

#[cfg(all(test, unix))]
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
            let loaded = unit::load(&mut connection, &test_unit("domain", 1), || {
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
            .unwrap();
            assert_eq!(
                test_records(loaded),
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
                "INSERT INTO rsi_storage_records(domain,table_name,key,value) VALUES ('domain','rows',printf('%0257d',0),X'6e756c6c'); UPDATE rsi_storage_domains SET record_count=1,record_bytes=271",
                "oversized stored address",
            ),
            (
                "UPDATE rsi_storage_domains SET domain='invalid / domain'",
                "domain",
            ),
            (
                "INSERT INTO rsi_storage_records(domain,table_name,key,value) VALUES ('domain','invalid/table','key',X'6e756c6c'); UPDATE rsi_storage_domains SET record_count=1,record_bytes=20",
                "table",
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

    fn deny_rollback(connection: &mut Connection) {
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
                 INSERT INTO rsi_storage_records(domain,table_name,key,value) VALUES ('absent','rows','a',x'6e756c6c')"
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
        type Hook = fn(&mut Wal, i32) -> rusqlite::Result<()>;
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
            backend
                .connection
                .lock()
                .unwrap()
                .wal_hook(Some(hook))
                .unwrap();
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
        deny_rollback(&mut backend.connection.lock().unwrap());
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
        deny_rollback(&mut backend.connection.lock().unwrap());
        let result = backend.transaction(|transaction| {
            insert_domain(transaction, "uncommitted", 1)?;
            transaction.execute_batch(
                "PRAGMA defer_foreign_keys=ON;
                 INSERT INTO rsi_storage_records(domain,table_name,key,value) VALUES ('absent','rows','a',x'6e756c6c')"
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
            .busy_handler(None::<fn(i32) -> bool>)
            .unwrap();
        BUSY_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod portable_sql_tests {
    use super::*;

    #[test]
    #[cfg(any(unix, windows))]
    fn activation_refuses_offline_orphans_without_changing_rows() {
        for mixed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("private").join("state.sqlite3");
            drop(SqliteBackend::open(&path).unwrap());
            let offline = Connection::open(&path).unwrap();
            offline.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
            if mixed {
                offline
                    .execute(
                        "INSERT INTO rsi_storage_domains(domain,version) VALUES ('valid',1)",
                        [],
                    )
                    .unwrap();
            }
            offline
                .execute(
                    "INSERT INTO rsi_storage_records VALUES ('orphan','rows','old',X'6e756c6c')",
                    [],
                )
                .unwrap();
            drop(offline);
            assert!(matches!(
                SqliteBackend::open(&path),
                Err(StorageError::Corrupt(_))
            ));
            let offline = Connection::open(&path).unwrap();
            let count: i64 = offline
                .query_row("SELECT count(*) FROM rsi_storage_records", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1);
            let parents: i64 = offline
                .query_row("SELECT count(*) FROM rsi_storage_domains", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(parents, i64::from(mixed));
        }
    }

    #[test]
    fn orphan_domains_are_refused_before_empty_reads_or_first_writes() {
        for mixed in [false, true] {
            let mut connection = Connection::open_in_memory().unwrap();
            initialize_or_validate_schema(&connection).unwrap();
            if mixed {
                connection
                    .execute(
                        "INSERT INTO rsi_storage_domains(domain,version) VALUES ('valid',1)",
                        [],
                    )
                    .unwrap();
            }
            connection
                .execute_batch(
                    "PRAGMA foreign_keys=OFF;
                INSERT INTO rsi_storage_records VALUES ('orphan','rows','old',X'6e756c6c');
                PRAGMA foreign_keys=ON;",
                )
                .unwrap();
            validate_schema(&connection).unwrap();
            assert!(matches!(
                unit::validate_accounting(&connection),
                Err(StorageError::Corrupt(_))
            ));
            let descriptor = rsi_storage::UnitDescriptor::new(
                "orphan".into(),
                1,
                BTreeSet::new(),
                BTreeMap::from([("rows".into(), rsi_storage::RecordPolicy::Reject)]),
                false,
                Layout::Single,
            )
            .unwrap();
            assert!(matches!(
                unit::load(&mut connection, &descriptor, || {}),
                Err(StorageError::Corrupt(_))
            ));
            let before = connection.total_changes();
            let transaction = connection.transaction().unwrap();
            assert!(matches!(
                unit::write(
                    &transaction,
                    &descriptor,
                    vec![Mutation {
                        address: rsi_storage::Address::record("rows", "new").unwrap(),
                        value: Some(rsi_storage::EncodedValue::encode(&Value::Bool(true)).unwrap()),
                    }]
                ),
                Err(StorageError::Corrupt(_))
            ));
            transaction.rollback().unwrap();
            assert_eq!(connection.total_changes(), before);
        }
    }

    #[test]
    fn schema_literal_whitespace_and_oversized_owned_objects_are_refused() {
        for ddl in [
            RECORDS_TABLE_SCHEMA.replace(" OR ", " or "),
            RECORDS_TABLE_SCHEMA.replace(
                "table_name != '' OR key = ''",
                "table_name != ' ' OR key = ' '",
            ),
            format!(
                "{RECORDS_TABLE_SCHEMA}; CREATE INDEX rsi_storage_{} ON rsi_storage_records(key)",
                "x".repeat(1024)
            ),
            format!("{RECORDS_TABLE_SCHEMA}; CREATE INDEX extra ON rsi_storage_records(key)"),
            format!(
                "{RECORDS_TABLE_SCHEMA}; CREATE TRIGGER extra AFTER INSERT ON rsi_storage_records BEGIN SELECT 1; END"
            ),
        ] {
            let connection = Connection::open_in_memory().unwrap();
            connection
                .pragma_update(None, "foreign_keys", true)
                .unwrap();
            connection
                .execute_batch(&format!("{}; {ddl};", domains_table_schema()))
                .unwrap();
            let before = connection.total_changes();
            assert!(matches!(
                initialize_or_validate_schema(&connection),
                Err(StorageError::Corrupt(reason)) if reason.contains("incompatible schema")
            ));
            assert_eq!(connection.total_changes(), before);
        }
    }

    #[test]
    fn one_navigation_edit_touches_two_payload_rows_and_one_unit_header() {
        use rsi_storage::{Address, EncodedValue, Layout, Mutation, RecordPolicy, UnitDescriptor};
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_or_validate_schema(&connection).unwrap();
        let unit = UnitDescriptor::new(
            "navigation".into(),
            2,
            BTreeSet::new(),
            BTreeMap::from([("metadata".into(), RecordPolicy::Reject)]),
            true,
            Layout::Single,
        )
        .unwrap();
        let original = EncodedValue::encode(
            &serde_json::json!({"title":"original","archived":false,"rank":null}),
        )
        .unwrap();
        let mut changes = vec![Mutation {
            address: Address::global(),
            value: Some(EncodedValue::encode(&serde_json::json!({"revision":1})).unwrap()),
        }];
        changes.extend((0..8192).map(|index| Mutation {
            address: Address::record("metadata", format!("row-{index:04}")).unwrap(),
            value: Some(original.clone()),
        }));
        let transaction = connection.transaction().unwrap();
        unit::write(&transaction, &unit, changes).unwrap();
        transaction.commit().unwrap();
        let before = connection.total_changes();
        let transaction = connection.transaction().unwrap();
        unit::write(
            &transaction,
            &unit,
            vec![
                Mutation {
                    address: Address::global(),
                    value: Some(EncodedValue::encode(&serde_json::json!({"revision":2})).unwrap()),
                },
                Mutation {
                    address: Address::record("metadata", "row-0001").unwrap(),
                    value: Some(
                        EncodedValue::encode(
                            &serde_json::json!({"title":"changed","archived":false,"rank":null}),
                        )
                        .unwrap(),
                    ),
                },
            ],
        )
        .unwrap();
        transaction.commit().unwrap();
        assert_eq!(connection.total_changes() - before, 3);
        let untouched: Vec<u8> = connection.query_row(
            "SELECT value FROM rsi_storage_records WHERE domain='navigation' AND table_name='metadata' AND key='row-0002'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(untouched, original.as_bytes());
        unit::validate_accounting(&connection).unwrap();
    }

    #[test]
    fn recomputed_record_overflow_reports_record_count() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_or_validate_schema(&connection).unwrap();
        connection.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO rsi_storage_domains(domain,version) VALUES ('domain',1);").unwrap();
        let transaction = connection.transaction().unwrap();
        {
            let mut insert = transaction.prepare("INSERT INTO rsi_storage_records(domain,table_name,key,value) VALUES ('domain','rows',?1,X'6e756c6c')").unwrap();
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
        assert!(matches!(unit::validate_accounting(&connection),
            Err(StorageError::Corrupt(reason)) if reason.contains("record count")));
    }
}

#[cfg(test)]
mod factory_preparation_tests {
    use super::*;

    #[test]
    fn preparation_distinguishes_invalid_input_from_unavailable_native_capability() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            SqliteStorageFactory
                .prepare(&serde_json::json!({"name":"test","path":"relative.sqlite3"})),
            Err(MetaError::InvalidInput(_))
        ));
        let result = SqliteStorageFactory.prepare(
            &serde_json::json!({"name":"test","path":root.path().join("missing/database.sqlite3")}),
        );
        if cfg!(any(unix, windows)) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(MetaError::Activation(_))));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(all(test, any(unix, windows)))]
mod open_cancellation_tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_open_retries_after_its_actual_read_releases_the_name() {
        use std::{
            future::Future,
            pin::pin,
            task::{Context, Poll, Waker},
        };
        let root = tempfile::tempdir().unwrap();
        let backend =
            Arc::new(SqliteBackend::open(&root.path().join("private/data.sqlite3")).unwrap());
        backend.put("domain", 1, "a", &Value::Null).await.unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first = tokio::spawn({
            let backend = backend.clone();
            async move {
                backend
                    .open_unit(test_unit("domain", 1), move || {
                        entered.send(()).unwrap();
                        released.blocking_recv().unwrap();
                    })
                    .await
            }
        });
        started.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let mut retry = pin!(backend.open_unit(test_unit("domain", 1), || {}));
        assert!(matches!(
            retry.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        release.send(()).unwrap();
        assert_eq!(retry.await.unwrap().snapshot.records.len(), 1);
    }
}
