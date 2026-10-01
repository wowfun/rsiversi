//! Atomic JSON-file backend for non-session storage domains.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage::{
    BackendLease, BackendOperations, BoundedWriter, KvBackend, MAXIMUM_STORAGE_RECORDS,
    StorageError, StorageHubContract, StoredDomain, create_private_directories,
    validate_identifier, validate_value,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
        if !self.path.is_absolute()
            || self.path.parent().is_none()
            || self.path.file_name().is_none()
        {
            return Err(StorageError::InvalidInput(
                "JSON storage path must be absolute and name a file".into(),
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
    operation: Arc<BackendOperations>,
}

impl JsonBackend {
    fn open(config: JsonStorageConfig) -> Result<Self, StorageError> {
        config.validate()?;
        let document = match read_file_bounded(&config.path, config.maximum_document_bytes) {
            Ok(bytes) => {
                let document: Document = serde_json::from_slice(&bytes)
                    .map_err(|error| StorageError::Corrupt(error.to_string()))?;
                validate_document(&document)?;
                sync_parent_directory(config.path.parent().ok_or_else(|| {
                    StorageError::InvalidInput("JSON storage path has no parent directory".into())
                })?)?;
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
            operation: Arc::new(BackendOperations::default()),
        })
    }

    async fn mutate(
        &self,
        domain: &str,
        version: u32,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), StorageError> {
        validate_identifier("domain", domain)?;
        validate_identifier("record key", key)?;
        if version == 0 {
            return Err(StorageError::InvalidInput(
                "domain version must be nonzero".into(),
            ));
        }
        let domain = domain.to_owned();
        let key = key.to_owned();
        self.publish(
            move |current| {
                let stored = current.domains.get(&domain);
                if let Some(stored) = stored {
                    if stored.version != version {
                        return Err(version_mismatch(&domain, stored.version, version));
                    }
                    if value.is_some()
                        && !stored.records.contains_key(&key)
                        && stored.records.len() == MAXIMUM_STORAGE_RECORDS
                    {
                        return Err(StorageError::InvalidInput(
                            "storage domain reached the backend record bound".into(),
                        ));
                    }
                }
                if value.is_none() && stored.is_none_or(|stored| !stored.records.contains_key(&key))
                {
                    return Ok(None);
                }
                let mut candidate = current.clone();
                let stored = candidate
                    .domains
                    .entry(domain)
                    .or_insert_with(|| StoredDomain {
                        version,
                        records: BTreeMap::new(),
                    });
                if let Some(value) = value {
                    stored.records.insert(key, value);
                } else {
                    stored.records.remove(&key);
                }
                Ok(Some(candidate))
            },
            atomic_write,
        )
        .await
    }

    // The persistence seam lets tests fail before or after replacement while using
    // the real publication and fencing path. Production always supplies atomic_write.
    async fn publish<F, P>(&self, change: F, persist: P) -> Result<(), StorageError>
    where
        F: FnOnce(&Document) -> Result<Option<Document>, StorageError> + Send + 'static,
        P: FnOnce(&JsonStorageConfig, &Document) -> Result<(), StorageError> + Send + 'static,
    {
        let document = Arc::clone(&self.document);
        let config = Arc::clone(&self.config);
        self.operation
            .run(move || {
                let current = Arc::clone(
                    &document
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                // The operation slot owns the read/modify/publish interval across both locks.
                let Some(candidate) = change(&current)? else {
                    return Ok(());
                };
                persist(&config, &candidate)?;
                *document
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(candidate);
                Ok(())
            })
            .await
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
    fn ensure_available(&self) -> Result<(), StorageError> {
        self.operation.ensure_available()
    }

    async fn load(&self, domain: &str) -> Result<Option<StoredDomain>, StorageError> {
        self.ensure_available()?;
        validate_identifier("domain", domain)?;
        let document = Arc::clone(&self.document);
        let domain = domain.to_owned();
        self.operation
            .run(move || {
                let snapshot = Arc::clone(
                    &document
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                Ok(snapshot.domains.get(&domain).cloned())
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
        validate_value(value)?;
        self.mutate(domain, version, key, Some(value.clone())).await
    }

    async fn delete(&self, domain: &str, version: u32, key: &str) -> Result<(), StorageError> {
        self.ensure_available()?;
        self.mutate(domain, version, key, None).await
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
        let backend = Arc::new(backend);
        let lease: BackendLease = plan
            .local::<StorageHubContract>()?
            .register(&name, backend.clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let registration = backend.operation.retain_registration(lease);
        plan.defer(
            "withdraw JSON storage backend",
            Box::new(move || {
                Box::pin(async move {
                    registration.close().await;
                    Ok(())
                })
            }),
        )
    }
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

fn atomic_write(config: &JsonStorageConfig, document: &Document) -> Result<(), StorageError> {
    atomic_write_with_sync(config, document, |directory| {
        if let Some(directory) = directory {
            directory.sync_all()?;
        }
        Ok(())
    })
}

// The final-directory-sync seam exercises a failure after the real file replacement.
fn atomic_write_with_sync(
    config: &JsonStorageConfig,
    document: &Document,
    sync: impl FnOnce(Option<File>) -> std::io::Result<()>,
) -> Result<(), StorageError> {
    let path = &config.path;
    let parent = path.parent().ok_or_else(|| {
        StorageError::InvalidInput("JSON storage path has no parent directory".into())
    })?;
    create_private_directories(parent)?;
    #[cfg(unix)]
    let directory = Some(File::open(parent).map_err(|error| StorageError::Io(error.to_string()))?);
    #[cfg(not(unix))]
    let directory = None;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StorageError::InvalidInput("JSON storage file name is invalid".into()))?;
    let (temporary, file) = create_temporary_file(parent, file_name)?;
    let result = (|| {
        set_file_permissions(&file)?;
        let mut writer = BoundedWriter::new(BufWriter::new(&file), config.maximum_document_bytes);
        encode_document(document, &mut writer)?;
        writer
            .flush()
            .and_then(|()| file.sync_all())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        fs::rename(&temporary, path).map_err(|error| StorageError::Io(error.to_string()))?;
        sync(directory).map_err(|error| StorageError::OutcomeUnknown(error.to_string()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn encode_document(
    document: &Document,
    writer: &mut BoundedWriter<impl Write>,
) -> Result<(), StorageError> {
    serde_json::to_writer_pretty(&mut *writer, document).map_err(|error| {
        if writer.limit_exceeded() {
            StorageError::InvalidInput(error.to_string())
        } else {
            StorageError::Io(error.to_string())
        }
    })
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
fn set_file_permissions(file: &File) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn set_file_permissions(_file: &File) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(path: PathBuf) -> JsonStorageConfig {
        JsonStorageConfig {
            name: "test".into(),
            path,
            maximum_document_bytes: 1024 * 1024,
        }
    }
    #[tokio::test]
    async fn cancelled_publication_keeps_the_file_slot_until_memory_and_disk_agree() {
        use std::{
            future::Future,
            pin::pin,
            task::{Context, Poll, Waker},
        };
        for delete in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let config = config(directory.path().join("state.json"));
            let backend = Arc::new(JsonBackend::open(config.clone()).unwrap());
            backend.put("domain", 1, "a", &Value::Null).await.unwrap();
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let first = tokio::spawn({
                let backend = backend.clone();
                async move {
                    backend
                        .publish(
                            move |current| {
                                let mut candidate = current.clone();
                                let records =
                                    &mut candidate.domains.get_mut("domain").unwrap().records;
                                if delete {
                                    records.remove("a");
                                } else {
                                    records.insert("a".into(), Value::Bool(true));
                                }
                                Ok(Some(candidate))
                            },
                            move |config, candidate| {
                                entered.send(()).unwrap();
                                released.blocking_recv().unwrap();
                                atomic_write(config, candidate)
                            },
                        )
                        .await
                }
            });
            started.await.unwrap();
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            let mut second = pin!(backend.put("domain", 1, "b", &Value::Null));
            assert!(matches!(
                second
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
            release.send(()).unwrap();
            second.await.unwrap();
            let live = backend.load("domain").await.unwrap().unwrap();
            let durable = JsonBackend::open(config)
                .unwrap()
                .load("domain")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(live, durable);
            assert_eq!(live.records.contains_key("a"), !delete);
            assert!(live.records.contains_key("b"));
        }
    }

    #[tokio::test]
    async fn post_replace_failure_fences_all_domains_until_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let backend = JsonBackend::open(config.clone()).unwrap();
        backend
            .put("other", 1, "before", &Value::Bool(true))
            .await
            .unwrap();
        let result = backend
            .publish(
                |current| {
                    let mut candidate = current.clone();
                    candidate.domains.insert(
                        "changed".into(),
                        StoredDomain {
                            version: 1,
                            records: BTreeMap::from([("visible".into(), Value::Bool(true))]),
                        },
                    );
                    Ok(Some(candidate))
                },
                |config, candidate| {
                    atomic_write_with_sync(config, candidate, |_| {
                        Err(std::io::Error::other("injected directory sync failure"))
                    })
                },
            )
            .await;
        assert!(matches!(result, Err(StorageError::OutcomeUnknown(_))));
        assert_eq!(
            backend.load("other").await,
            Err(StorageError::RecoveryRequired)
        );
        assert_eq!(
            backend.put("other", 1, "lost", &Value::Null).await,
            Err(StorageError::RecoveryRequired)
        );
        assert_eq!(
            backend.delete("missing", 1, "absent").await,
            Err(StorageError::RecoveryRequired)
        );
        let reopened = JsonBackend::open(config).unwrap();
        assert_eq!(
            reopened.load("other").await.unwrap().unwrap().records.len(),
            1
        );
        assert_eq!(
            reopened.load("changed").await.unwrap().unwrap().records["visible"],
            Value::Bool(true)
        );
    }

    #[tokio::test]
    async fn rejected_encoding_preserves_file_and_admission() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(directory.path().join("state.json"));
        config.maximum_document_bytes = 256;
        let backend = JsonBackend::open(config.clone()).unwrap();
        backend.put("domain", 1, "a", &Value::Null).await.unwrap();
        let before = fs::read(&config.path).unwrap();
        assert!(matches!(
            backend
                .put("domain", 1, "b", &Value::String("x".repeat(256)))
                .await,
            Err(StorageError::InvalidInput(_))
        ));
        assert_eq!(fs::read(&config.path).unwrap(), before);
        backend.ensure_available().unwrap();
        backend
            .put("domain", 1, "a", &Value::Bool(true))
            .await
            .unwrap();
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn accepted_depth_survives_real_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let backend = JsonBackend::open(config.clone()).unwrap();
        let mut value = Value::Null;
        for _ in 1..rsi_storage::MAXIMUM_STORAGE_VALUE_DEPTH {
            value = Value::Array(vec![value]);
        }
        backend.put("domain", 1, "a", &value).await.unwrap();
        assert!(
            backend
                .put("domain", 1, "b", &Value::Array(vec![value.clone()]))
                .await
                .is_err()
        );
        backend.operation.close().await;
        assert!(backend.load("domain").await.is_err());
        let reopened = JsonBackend::open(config).unwrap();
        assert_eq!(
            reopened.load("domain").await.unwrap().unwrap().records["a"],
            value
        );
    }
    struct InvalidInputWriter;
    impl Write for InvalidInputWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "underlying I/O rejected write",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn byte_limit_and_underlying_invalid_input_are_distinct() {
        let mut io = BoundedWriter::new(InvalidInputWriter, 1024);
        assert!(matches!(
            encode_document(&Document::default(), &mut io),
            Err(StorageError::Io(_))
        ));
        assert!(!io.limit_exceeded());
        let mut bounded = BoundedWriter::new(Vec::new(), 1);
        assert!(matches!(
            encode_document(&Document::default(), &mut bounded),
            Err(StorageError::InvalidInput(_))
        ));
        assert!(bounded.limit_exceeded());
    }
    #[test]
    fn configuration_requires_an_absolute_file_path_before_opening() {
        for path in [
            PathBuf::new(),
            PathBuf::from("relative.json"),
            std::env::current_dir()
                .unwrap()
                .ancestors()
                .last()
                .unwrap()
                .to_path_buf(),
        ] {
            assert!(matches!(
                JsonBackend::open(config(path)),
                Err(StorageError::InvalidInput(_))
            ));
        }
    }
    #[tokio::test]
    async fn absent_deletes_preserve_snapshot_identity_and_schema_checks() {
        let directory = tempfile::tempdir().unwrap();
        let backend = JsonBackend::open(config(directory.path().join("state.json"))).unwrap();
        backend
            .put("domain", 1, "present", &Value::Null)
            .await
            .unwrap();
        let before = backend.document.lock().unwrap().clone();
        backend.delete("domain", 1, "absent").await.unwrap();
        backend.delete("absent", 1, "absent").await.unwrap();
        assert!(Arc::ptr_eq(&before, &backend.document.lock().unwrap()));
        assert!(matches!(
            backend.delete("domain", 2, "absent").await,
            Err(StorageError::Corrupt(_))
        ));
    }
}
