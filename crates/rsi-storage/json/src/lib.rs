//! Unix JSON persistence for typed non-session domains.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use io::{atomic_write, read_file_bounded, require_native_platform, sync_parent_directory};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage::{
    Address, BackendLease, BackendOperations, BoundedWriter, Capabilities, EncodedValue, KvBackend,
    KvUnit, Layout, MAXIMUM_STORAGE_DOMAIN_BYTES, MAXIMUM_STORAGE_RECORDS, Mutation, OpenedUnit,
    RecordObjectSize, RecordPolicy, Recovery, RecoveryCause, StorageError, StorageHubContract,
    UnitDescriptor, UnitLease, UnitNames, UnitSnapshot, admit_mutations,
    create_private_directories, validate_identifier,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

mod io;
mod records;
mod single;
#[cfg(test)]
mod tests;

const DEFAULT_MAXIMUM_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
/// Durable physical layout, independent of business versions.
pub const JSON_STORAGE_FORMAT: u32 = 2;
const MAXIMUM_TEMPORARY_NAME_ATTEMPTS: usize = 64;
static NEXT_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An explicitly routed backend root and physical document limit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonStorageConfig {
    /// Exact registration name.
    pub name: String,
    /// Absolute directory containing this backend's units.
    pub path: PathBuf,
    /// Maximum bytes for one physical document, including its envelope.
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
                "JSON storage root must be an absolute named directory".into(),
            ));
        }
        if self.maximum_document_bytes == 0
            || self.maximum_document_bytes > DEFAULT_MAXIMUM_DOCUMENT_BYTES
        {
            return Err(StorageError::InvalidInput(format!(
                "maximum_document_bytes must be within 1..={DEFAULT_MAXIMUM_DOCUMENT_BYTES}"
            )));
        }
        require_native_platform()
    }
}
#[derive(Clone, Debug)]
struct JsonBackend {
    config: Arc<JsonStorageConfig>,
    operation: Arc<BackendOperations>,
    units: Arc<UnitNames>,
}
impl JsonBackend {
    fn open(config: JsonStorageConfig) -> Result<Self, StorageError> {
        config.validate()?;
        directory_exists(&config.path)?;
        Ok(Self {
            config: Arc::new(config),
            operation: Arc::default(),
            units: Arc::default(),
        })
    }
}
#[async_trait]
impl KvBackend for JsonBackend {
    fn ensure_available(&self) -> Result<(), StorageError> {
        self.operation.ensure_available()
    }
    fn capabilities(&self, layout: Layout) -> Capabilities {
        Capabilities {
            atomic_batch: layout == Layout::Single,
            backup_record: layout == Layout::PerRecord,
        }
    }
    async fn open(&self, descriptor: UnitDescriptor) -> Result<OpenedUnit, StorageError> {
        let slot = self.operation.acquire().await?;
        let lease = self.units.reserve(descriptor.name())?;
        let owner = Arc::new(UnitOwner {
            backend: self.clone(),
            descriptor,
            _lease: lease,
        });
        let retained = owner.clone();
        let (snapshot, state) = slot
            .run(move || {
                let id = hash(retained.descriptor.name().as_bytes());
                let single = retained.backend.config.path.join(format!("{id}.json"));
                let records = retained.backend.config.path.join(id);
                match retained.descriptor.layout() {
                    Layout::Single => {
                        if directory_exists(&records)? {
                            return Err(corrupt("unit already uses per-record layout"));
                        }
                        single::load(&retained, &single)
                    }
                    Layout::PerRecord => {
                        if single.try_exists().map_err(io_error)? {
                            return Err(corrupt("unit already uses single layout"));
                        }
                        records::load(&retained, &records)
                    }
                }
            })
            .await?;
        Ok(OpenedUnit {
            snapshot,
            unit: Arc::new(JsonUnit {
                owner,
                state: Arc::new(Mutex::new(state)),
            }),
        })
    }
}
#[derive(Debug)]
struct UnitOwner {
    backend: JsonBackend,
    descriptor: UnitDescriptor,
    _lease: UnitLease,
}
#[derive(Debug)]
struct JsonUnit {
    owner: Arc<UnitOwner>,
    state: Arc<Mutex<UnitState>>,
}
#[derive(Debug)]
enum UnitState {
    Single {
        path: PathBuf,
        records: BTreeMap<Address, EncodedValue>,
        size: RecordObjectSize,
    },
    PerRecord {
        path: PathBuf,
        lengths: BTreeMap<Address, usize>,
        size: RecordObjectSize,
    },
}
#[async_trait]
impl KvUnit for JsonUnit {
    fn descriptor(&self) -> &UnitDescriptor {
        &self.owner.descriptor
    }
    fn ensure_available(&self) -> Result<(), StorageError> {
        self.owner.backend.ensure_available()
    }
    async fn write(&self, changes: Vec<Mutation>) -> Result<(), StorageError> {
        self.publish(changes, |path, limit, document| {
            atomic_write(path, limit, document)
        })
        .await
    }
    async fn backup_record(&self, address: &Address) -> Result<PathBuf, StorageError> {
        if address.is_global()
            || self.descriptor().tables().get(address.table()) != Some(&RecordPolicy::BackupAndSkip)
        {
            return Err(StorageError::Unsupported(
                "only disposable table records can be preserved and skipped".into(),
            ));
        }
        let address = address.clone();
        let owner = self.owner.clone();
        let state = self.state.clone();
        self.owner
            .backend
            .operation
            .run(move || {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let UnitState::PerRecord {
                    path,
                    lengths,
                    size,
                } = &mut *state
                else {
                    return Err(StorageError::Unsupported(
                        "single JSON has no individual source document".into(),
                    ));
                };
                let backup = records::backup(&owner, &records::record_path(path, &address))?;
                if let Some(length) = lengths.remove(&address) {
                    *size = size.without_entry(address.entry_bytes(length)?)?;
                }
                Ok(backup)
            })
            .await
    }
}
impl JsonUnit {
    // Fault injection keeps cancellation and uncertain-publication tests on the
    // real serialization and owned worker path.
    async fn publish<P>(&self, changes: Vec<Mutation>, persist: P) -> Result<(), StorageError>
    where
        P: FnOnce(&Path, usize, &single::Document<'_>) -> Result<(), StorageError> + Send + 'static,
    {
        self.ensure_available()?;
        admit_mutations(
            self.descriptor(),
            &changes,
            self.owner.backend.capabilities(self.descriptor().layout()),
        )?;
        let owner = self.owner.clone();
        let state = self.state.clone();
        self.owner
            .backend
            .operation
            .run(move || {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &mut *state {
                    UnitState::Single {
                        path,
                        records,
                        size,
                    } => single::write(&owner, path, records, size, changes, persist),
                    UnitState::PerRecord {
                        path,
                        lengths,
                        size,
                    } => records::write(&owner, path, lengths, size, changes),
                }
            })
            .await
    }
}
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn directory_exists(path: &Path) -> Result<bool, StorageError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(corrupt(
            "JSON storage requires a directory, not a file or symlink",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(e)),
    }
}
fn ensure_directory(path: &Path) -> Result<(), StorageError> {
    directory_exists(path)?;
    create_private_directories(path)
}
fn io_error(error: impl std::fmt::Display) -> StorageError {
    StorageError::Io(error.to_string())
}
fn corrupt(error: impl std::fmt::Display) -> StorageError {
    StorageError::Corrupt(error.to_string())
}
fn read_optional(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, StorageError> {
    match read_file_bounded(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Err(corrupt(e)),
        Err(e) => Err(io_error(e)),
    }
}
fn check_size(size: RecordObjectSize) -> Result<(), StorageError> {
    if size.records() > MAXIMUM_STORAGE_RECORDS || size.bytes() > MAXIMUM_STORAGE_DOMAIN_BYTES {
        Err(StorageError::InvalidInput(
            "domain exceeds its storage record or byte bound".into(),
        ))
    } else {
        Ok(())
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
        config.validate().map_err(|error| match error {
            StorageError::Unsupported(_) => MetaError::Activation(error.to_string()),
            _ => MetaError::InvalidInput(error.to_string()),
        })?;
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

#[cfg(all(test, not(unix)))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn acquisition_rejects_before_creating_parents_or_reading_an_existing_document() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            root.path().join("missing/document.json"),
            root.path().join("existing.json"),
        ] {
            if path.file_name().unwrap() == "existing.json" {
                std::fs::write(&path, b"retained").unwrap();
            }
            let config = JsonStorageConfig {
                name: "test".into(),
                path,
                maximum_document_bytes: 1024,
            };
            assert!(matches!(
                JsonBackend::open(config),
                Err(StorageError::Unsupported(_))
            ));
        }
        assert!(!root.path().join("missing").exists());
        assert_eq!(
            std::fs::read(root.path().join("existing.json")).unwrap(),
            b"retained"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod factory_preparation_tests {
    use super::*;

    #[test]
    fn preparation_distinguishes_invalid_input_from_unavailable_native_capability() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            JsonStorageFactory.prepare(&serde_json::json!({"name":"test","path":"relative.json"})),
            Err(MetaError::InvalidInput(_))
        ));
        let result = JsonStorageFactory.prepare(&serde_json::json!({"name":"test","path":root.path().join("missing/state.json"),"maximum_document_bytes":1024}));
        if cfg!(unix) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(MetaError::Activation(_))));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
