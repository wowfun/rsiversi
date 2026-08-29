//! Atomic JSON-file backend for non-session storage domains.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage::{
    BackendLease, KvBackend, MAXIMUM_STORAGE_RECORDS, StorageError, StorageHubContract,
    StoredDomain, validate_identifier, validate_value,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_MAXIMUM_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_TEMPORARY_NAME_ATTEMPTS: usize = 64;
static NEXT_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Configuration accepted by [`JsonStorageFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonStorageConfig {
    /// Exact backend registration name.
    pub name: String,
    /// Absolute JSON document path.
    pub path: PathBuf,
    /// Maximum encoded document bytes.
    #[serde(default = "default_maximum_document_bytes")]
    pub maximum_document_bytes: usize,
}

fn default_maximum_document_bytes() -> usize {
    DEFAULT_MAXIMUM_DOCUMENT_BYTES
}

impl JsonStorageConfig {
    fn validate(&self) -> Result<(), StorageError> {
        validate_identifier("backend", &self.name)?;
        if !self.path.is_absolute() {
            return Err(StorageError::InvalidInput(
                "JSON storage path must be absolute".into(),
            ));
        }
        if self.maximum_document_bytes == 0
            || self.maximum_document_bytes > DEFAULT_MAXIMUM_DOCUMENT_BYTES
        {
            return Err(StorageError::InvalidInput(format!(
                "maximum_document_bytes must be within 1..={DEFAULT_MAXIMUM_DOCUMENT_BYTES}"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default = "document_format")]
    format: u32,
    #[serde(default)]
    domains: BTreeMap<String, StoredDomain>,
}

const fn document_format() -> u32 {
    1
}

#[derive(Debug)]
struct JsonBackend {
    config: Arc<JsonStorageConfig>,
    document: Arc<Mutex<Arc<Document>>>,
    operation: AsyncMutex<()>,
}

impl JsonBackend {
    fn open(config: JsonStorageConfig) -> Result<Self, StorageError> {
        let document = match read_file_bounded(&config.path, config.maximum_document_bytes) {
            Ok(bytes) => {
                let document: Document = serde_json::from_slice(&bytes)
                    .map_err(|error| StorageError::Corrupt(error.to_string()))?;
                validate_document(&document)?;
                document
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Document {
                format: document_format(),
                domains: BTreeMap::new(),
            },
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(StorageError::Corrupt(error.to_string()));
            }
            Err(error) => return Err(StorageError::Io(error.to_string())),
        };
        Ok(Self {
            config: Arc::new(config),
            document: Arc::new(Mutex::new(Arc::new(document))),
            operation: AsyncMutex::new(()),
        })
    }

    fn persist(config: &JsonStorageConfig, candidate: &Document) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec_pretty(candidate)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        if bytes.len() > config.maximum_document_bytes {
            return Err(StorageError::InvalidInput(format!(
                "JSON storage document exceeds {} bytes",
                config.maximum_document_bytes
            )));
        }
        atomic_write(&config.path, &bytes)
    }
}

fn read_file_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    let file = open_unchanged_regular_file(path, "JSON storage document")?;
    if file.metadata()?.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("JSON storage document exceeds {maximum_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("JSON storage document exceeds {maximum_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

fn open_unchanged_regular_file(path: &Path, label: &str) -> std::io::Result<File> {
    let initial = fs::symlink_metadata(path)?;
    if !initial.file_type().is_file() {
        return Err(not_regular_file(label));
    }
    let file = open_file_no_follow(path)?;
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !opened.file_type().is_file() || !current.file_type().is_file() {
        return Err(not_regular_file(label));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if initial.dev() != opened.dev()
            || initial.ino() != opened.ino()
            || current.dev() != opened.dev()
            || current.ino() != opened.ino()
        {
            return Err(changed_file(label));
        }
    }
    #[cfg(windows)]
    {
        let opened_identity = same_file::Handle::from_file(file.try_clone()?)?;
        let current_identity = same_file::Handle::from_file(open_file_no_follow(path)?)?;
        if opened_identity != current_identity {
            return Err(changed_file(label));
        }
    }
    #[cfg(not(any(unix, windows)))]
    if initial.len() != opened.len()
        || current.len() != opened.len()
        || initial.modified().ok() != opened.modified().ok()
        || current.modified().ok() != opened.modified().ok()
    {
        return Err(changed_file(label));
    }
    Ok(file)
}

fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn not_regular_file(label: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{label} must be a regular non-symlink file"),
    )
}

fn changed_file(label: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{label} changed while opening"),
    )
}

#[async_trait]
impl KvBackend for JsonBackend {
    async fn load(&self, domain: &str) -> Result<Option<StoredDomain>, StorageError> {
        validate_identifier("domain", domain)?;
        let snapshot = Arc::clone(
            &self
                .document
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let domain = domain.to_owned();
        tokio::task::spawn_blocking(move || Ok(snapshot.domains.get(&domain).cloned()))
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
        if version == 0 {
            return Err(StorageError::InvalidInput(
                "domain version must be nonzero".into(),
            ));
        }
        validate_value(value)?;
        let _operation = self.operation.lock().await;
        let config = Arc::clone(&self.config);
        let document = Arc::clone(&self.document);
        let domain = domain.to_owned();
        let key = key.to_owned();
        let value = value.clone();
        tokio::task::spawn_blocking(move || {
            let current = Arc::clone(
                &document
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            let mut candidate = (*current).clone();
            let stored = candidate
                .domains
                .entry(domain.clone())
                .or_insert_with(|| StoredDomain {
                    version,
                    records: BTreeMap::new(),
                });
            if stored.version != version {
                return Err(version_mismatch(&domain, stored.version, version));
            }
            if !stored.records.contains_key(&key) && stored.records.len() == MAXIMUM_STORAGE_RECORDS
            {
                return Err(StorageError::InvalidInput(
                    "storage domain reached the backend record bound".into(),
                ));
            }
            stored.records.insert(key, value);
            Self::persist(&config, &candidate)?;
            *document
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(candidate);
            Ok(())
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    async fn delete(&self, domain: &str, version: u32, key: &str) -> Result<(), StorageError> {
        validate_identifier("domain", domain)?;
        validate_identifier("record key", key)?;
        let _operation = self.operation.lock().await;
        let config = Arc::clone(&self.config);
        let document = Arc::clone(&self.document);
        let domain = domain.to_owned();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let current = Arc::clone(
                &document
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            let Some(stored) = current.domains.get(&domain) else {
                return Ok(());
            };
            if stored.version != version {
                return Err(version_mismatch(&domain, stored.version, version));
            }
            if !stored.records.contains_key(&key) {
                return Ok(());
            }
            let mut candidate = (*current).clone();
            candidate
                .domains
                .get_mut(&domain)
                .expect("checked domain")
                .records
                .remove(&key);
            Self::persist(&config, &candidate)?;
            *document
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(candidate);
            Ok(())
        })
        .await
        .map_err(|error| join_error(&error))?
    }
}

/// Ordinary plugin factory for one exact-name JSON backend.
#[derive(Clone, Debug, Default)]
pub struct JsonStorageFactory;

#[async_trait]
impl PluginFactory for JsonStorageFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: JsonStorageConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config.name.len() + config.path.as_os_str().len() + 64;
        Ok(
            PreparedActivation::with_state(desired.clone(), config, retained)
                .requiring_local::<StorageHubContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<JsonStorageConfig>()?;
        let name = config.name.clone();
        let backend = tokio::task::spawn_blocking(move || JsonBackend::open(config))
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let backend: Arc<dyn KvBackend> = Arc::new(backend);
        let lease: BackendLease = plan
            .local::<StorageHubContract>()?
            .register(&name, backend)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw JSON storage backend",
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

fn validate_document(document: &Document) -> Result<(), StorageError> {
    if document.format != document_format() {
        return Err(StorageError::Corrupt(format!(
            "unsupported JSON storage format {}",
            document.format
        )));
    }
    for (domain, stored) in &document.domains {
        validate_identifier("domain", domain)?;
        if stored.version == 0 || stored.records.len() > MAXIMUM_STORAGE_RECORDS {
            return Err(StorageError::Corrupt(format!(
                "domain `{domain}` has invalid bounds or version"
            )));
        }
        for (key, value) in &stored.records {
            validate_identifier("record key", key)?;
            if validate_value(value).is_err() {
                return Err(StorageError::Corrupt(format!(
                    "domain `{domain}` contains an invalid value"
                )));
            }
        }
    }
    Ok(())
}

fn version_mismatch(domain: &str, actual: u32, expected: u32) -> StorageError {
    StorageError::Corrupt(format!(
        "domain `{domain}` has version {actual}, expected {expected}"
    ))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let parent = path.parent().ok_or_else(|| {
        StorageError::InvalidInput("JSON storage path has no parent directory".into())
    })?;
    create_private_directories(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StorageError::InvalidInput("JSON storage file name is invalid".into()))?;
    let (temporary, mut file) = create_temporary_file(parent, file_name)?;
    let result = (|| {
        set_file_permissions(&file)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        fs::rename(&temporary, path).map_err(|error| StorageError::Io(error.to_string()))?;
        sync_parent_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), StorageError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), StorageError> {
    Ok(())
}

fn create_temporary_file(parent: &Path, file_name: &str) -> Result<(PathBuf, File), StorageError> {
    for _ in 0..MAXIMUM_TEMPORARY_NAME_ATTEMPTS {
        let sequence = NEXT_TEMPORARY_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| StorageError::Io("JSON storage temporary sequence exhausted".into()))?;
        let path = parent.join(format!(
            ".{file_name}.{}.{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StorageError::Io(error.to_string())),
        }
    }
    Err(StorageError::Io(
        "JSON storage could not allocate a private temporary file".into(),
    ))
}

#[cfg(unix)]
fn create_private_directories(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn create_private_directories(path: &Path) -> Result<(), StorageError> {
    fs::create_dir_all(path).map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(unix)]
fn set_file_permissions(file: &File) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn set_file_permissions(_file: &File) -> Result<(), StorageError> {
    Ok(())
}
