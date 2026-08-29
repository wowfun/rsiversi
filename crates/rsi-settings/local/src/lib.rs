//! Local atomic JSON Settings provider.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{
    Result, SettingsDocument, SettingsError, SettingsProvider, SettingsProviderContract,
    validate_namespace, validate_section,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const DEFAULT_MAXIMUM_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
const LOCK_ATTEMPTS: usize = 100;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const MAXIMUM_TEMPORARY_NAME_ATTEMPTS: usize = 64;
static NEXT_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Configuration accepted by [`LocalSettingsFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSettingsConfig {
    /// Absolute JSON Settings document path.
    pub path: PathBuf,
    /// Maximum complete encoded document bytes.
    #[serde(default = "default_maximum_document_bytes")]
    pub maximum_document_bytes: usize,
}

fn default_maximum_document_bytes() -> usize {
    DEFAULT_MAXIMUM_DOCUMENT_BYTES
}

impl LocalSettingsConfig {
    fn validate(&self) -> Result<()> {
        if !self.path.is_absolute() {
            return Err(SettingsError::InvalidInput(
                "local Settings path must be absolute".into(),
            ));
        }
        if self.maximum_document_bytes == 0
            || self.maximum_document_bytes > DEFAULT_MAXIMUM_DOCUMENT_BYTES
        {
            return Err(SettingsError::InvalidInput(format!(
                "maximum_document_bytes must be within 1..={DEFAULT_MAXIMUM_DOCUMENT_BYTES}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct LocalProvider {
    config: LocalSettingsConfig,
}

#[async_trait]
impl SettingsProvider for LocalProvider {
    fn writable(&self) -> bool {
        true
    }

    async fn load(&self) -> Result<SettingsDocument> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || read_document(&config))
            .await
            .map_err(|error| join_error(&error))?
    }

    async fn compare_and_set(
        &self,
        namespace: &str,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> Result<Option<Value>> {
        validate_namespace(namespace)?;
        if let Some(replacement) = replacement {
            validate_section(replacement)?;
        }
        let config = self.config.clone();
        let namespace = namespace.to_owned();
        let expected = expected.cloned();
        let replacement = replacement.cloned();
        tokio::task::spawn_blocking(move || {
            let _lock = acquire_lock(&config.path)?;
            let mut document = read_document(&config)?;
            if document.get(&namespace) != expected.as_ref() {
                return Err(SettingsError::ConcurrentDocumentChange);
            }
            match replacement {
                Some(value) => {
                    document.insert(namespace.clone(), value);
                }
                None => {
                    document.remove(&namespace);
                }
            }
            write_document(&config, &document)?;
            Ok(document.get(&namespace).cloned())
        })
        .await
        .map_err(|error| join_error(&error))?
    }
}

/// Ordinary plugin factory for one local Settings provider.
#[derive(Clone, Debug, Default)]
pub struct LocalSettingsFactory;

#[async_trait]
impl PluginFactory for LocalSettingsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: LocalSettingsConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config.path.as_os_str().len() + 32;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<LocalSettingsConfig>()?;
        let provider = LocalProvider { config };
        provider
            .load()
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let provider: Arc<dyn SettingsProvider> = Arc::new(provider);
        let supply = plan
            .context()
            .provide_local::<SettingsProviderContract>(provider)?;
        plan.defer(
            "withdraw local Settings provider",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn read_document(config: &LocalSettingsConfig) -> Result<SettingsDocument> {
    let bytes = match read_file_bounded(&config.path, config.maximum_document_bytes) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SettingsDocument::new());
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(SettingsError::Corrupt(error.to_string()));
        }
        Err(error) => return Err(SettingsError::Io(error.to_string())),
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| SettingsError::Corrupt(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| SettingsError::Corrupt("document root must be an object".into()))?;
    let mut document = SettingsDocument::new();
    for (namespace, section) in object {
        validate_namespace(namespace)
            .map_err(|_| SettingsError::Corrupt("invalid namespace".into()))?;
        validate_section(section)
            .map_err(|_| SettingsError::Corrupt("invalid namespace section".into()))?;
        document.insert(namespace.clone(), section.clone());
    }
    Ok(document)
}

fn read_file_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("document exceeds {maximum_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("document exceeds {maximum_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

fn write_document(config: &LocalSettingsConfig, document: &SettingsDocument) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| SettingsError::Io(error.to_string()))?;
    if bytes.len() > config.maximum_document_bytes {
        return Err(SettingsError::InvalidInput(format!(
            "document exceeds {} bytes",
            config.maximum_document_bytes
        )));
    }
    atomic_write(&config.path, &bytes)
}

#[derive(Debug)]
struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_lock(path: &Path) -> Result<FileLock> {
    let parent = path
        .parent()
        .ok_or_else(|| SettingsError::InvalidInput("Settings path has no parent".into()))?;
    create_private_directories(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SettingsError::InvalidInput("Settings file name is invalid".into()))?;
    let lock_path = parent.join(format!(".{name}.lock"));
    reject_lock_symlink(&lock_path)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&lock_path)
        .map_err(|error| SettingsError::Io(error.to_string()))?;
    validate_lock_file(&lock_path, &file)?;
    set_open_file_permissions(&file)?;
    for _ in 0..LOCK_ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Ok(FileLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(SettingsError::Io(error.to_string()));
            }
        }
    }
    Err(SettingsError::Io(
        "timed out acquiring the Settings writer lock".into(),
    ))
}

fn join_error(error: &tokio::task::JoinError) -> SettingsError {
    SettingsError::Io(format!("Settings blocking task failed: {error}"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| SettingsError::InvalidInput("Settings path has no parent".into()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SettingsError::InvalidInput("Settings file name is invalid".into()))?;
    let (temporary, mut file) = create_temporary_file(parent, name)?;
    let result = (|| {
        set_open_file_permissions(&file)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| SettingsError::Io(error.to_string()))?;
        fs::rename(&temporary, path).map_err(|error| SettingsError::Io(error.to_string()))?;
        sync_parent_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn reject_lock_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(SettingsError::InvalidInput(
            "Settings writer lock must not be a symbolic link".into(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SettingsError::Io(error.to_string())),
    }
}

fn validate_lock_file(path: &Path, file: &File) -> Result<()> {
    let path_metadata =
        fs::symlink_metadata(path).map_err(|error| SettingsError::Io(error.to_string()))?;
    let file_metadata = file
        .metadata()
        .map_err(|error| SettingsError::Io(error.to_string()))?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !file_metadata.file_type().is_file()
    {
        return Err(SettingsError::InvalidInput(
            "Settings writer lock must be a real regular file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(SettingsError::InvalidInput(
                "Settings writer lock changed while opening".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| SettingsError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    Ok(())
}

fn create_temporary_file(parent: &Path, name: &str) -> Result<(PathBuf, File)> {
    for _ in 0..MAXIMUM_TEMPORARY_NAME_ATTEMPTS {
        let sequence = NEXT_TEMPORARY_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| SettingsError::Io("Settings temporary sequence exhausted".into()))?;
        let path = parent.join(format!(".{name}.{}.{sequence}.tmp", std::process::id()));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(SettingsError::Io(error.to_string())),
        }
    }
    Err(SettingsError::Io(
        "Settings could not allocate a private temporary file".into(),
    ))
}

#[cfg(unix)]
fn create_private_directories(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|error| SettingsError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn create_private_directories(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| SettingsError::Io(error.to_string()))
}

#[cfg(unix)]
fn set_open_file_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| SettingsError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn set_open_file_permissions(_file: &File) -> Result<()> {
    Ok(())
}
