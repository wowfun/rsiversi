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
use std::fs::File;
#[cfg(not(windows))]
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const DEFAULT_MAXIMUM_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
const LOCK_ATTEMPTS: usize = 100;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
#[cfg(not(windows))]
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
        #[cfg(windows)]
        rsi_files_native_fs::resolve_absolute_root_alias(&self.path, true)
            .map_err(|e| SettingsError::InvalidInput(e.to_string()))?;
        require_native_platform()
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
        config.validate().map_err(|error| match error {
            SettingsError::Unsupported(_) => MetaError::Activation(error.to_string()),
            _ => MetaError::InvalidInput(error.to_string()),
        })?;
        let retained = config.path.as_os_str().len() + 32;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        require_native_platform().map_err(|error| MetaError::Activation(error.to_string()))?;
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
    config.validate()?;
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
    #[cfg(not(windows))]
    let file = File::open(path)?;
    #[cfg(windows)]
    let file = {
        let parent = rsi_files_native_fs::open_absolute_directory_no_follow(
            path.parent()
                .ok_or_else(|| std::io::Error::other("Settings path has no parent"))?,
        )?;
        rsi_files_native_fs::open_relative_file_no_follow(
            &parent,
            Path::new(
                path.file_name()
                    .ok_or_else(|| std::io::Error::other("Settings path has no name"))?,
            ),
        )?
    };
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
    config.validate()?;
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
    #[cfg(windows)]
    _namespace: rsi_files_native_fs::NamespaceLease,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(not(windows))]
fn acquire_lock(path: &Path) -> Result<FileLock> {
    require_native_platform()?;
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

#[cfg(not(windows))]
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

#[cfg(not(windows))]
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

#[cfg(not(windows))]
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

#[cfg(not(any(unix, windows)))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    require_native_platform()
}

#[cfg(not(windows))]
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

#[cfg(not(any(unix, windows)))]
fn create_private_directories(_path: &Path) -> Result<()> {
    require_native_platform()
}

#[cfg(unix)]
fn set_open_file_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| SettingsError::Io(error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn set_open_file_permissions(_file: &File) -> Result<()> {
    require_native_platform()
}

fn require_native_platform() -> Result<()> {
    if cfg!(any(unix, windows)) {
        Ok(())
    } else {
        Err(SettingsError::Unsupported(
            "Settings native filesystem protection is unsupported on this platform".into(),
        ))
    }
}

#[cfg(all(test, not(any(unix, windows))))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn refusal_precedes_existing_document_reads_and_missing_writer_lock_creation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("existing.json");
        fs::write(&path, b"retained").unwrap();
        let config = LocalSettingsConfig {
            path: path.clone(),
            maximum_document_bytes: 1024,
        };
        assert!(matches!(
            read_document(&config),
            Err(SettingsError::Unsupported(_))
        ));
        assert!(matches!(
            write_document(&config, &SettingsDocument::new()),
            Err(SettingsError::Unsupported(_))
        ));
        assert!(matches!(
            acquire_lock(&root.path().join("missing/settings.json")),
            Err(SettingsError::Unsupported(_))
        ));
        assert!(
            LocalSettingsFactory
                .prepare(&serde_json::json!({ "path": path }))
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"retained");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(!root.path().join("missing").exists());
    }
}

#[cfg(test)]
mod config_validation_tests {
    use super::*;
    use std::fs;

    #[test]
    fn malformed_configuration_precedes_platform_admission() {
        let root = tempfile::tempdir().unwrap();
        for (path, maximum_document_bytes) in [
            (PathBuf::from("relative.json"), 1024),
            (root.path().join("missing/settings.json"), 0),
            (
                root.path().join("missing/settings.json"),
                DEFAULT_MAXIMUM_DOCUMENT_BYTES + 1,
            ),
        ] {
            assert!(matches!(
                LocalSettingsConfig {
                    path,
                    maximum_document_bytes
                }
                .validate(),
                Err(SettingsError::InvalidInput(_))
            ));
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn valid_configuration_checks_platform_without_filesystem_effects() {
        let root = tempfile::tempdir().unwrap();
        let result = LocalSettingsConfig {
            path: root.path().join("missing/settings.json"),
            maximum_document_bytes: 1024,
        }
        .validate();
        if cfg!(any(unix, windows)) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(SettingsError::Unsupported(_))));
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(test)]
mod factory_preparation_tests {
    use super::*;

    #[test]
    fn preparation_distinguishes_invalid_input_from_unavailable_native_capability() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            LocalSettingsFactory.prepare(&serde_json::json!({"path":"relative.json"})),
            Err(MetaError::InvalidInput(_))
        ));
        let result = LocalSettingsFactory.prepare(&serde_json::json!({"path":root.path().join("missing/settings.json"),"maximum_document_bytes":1024}));
        if cfg!(any(unix, windows)) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(MetaError::Activation(_))));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(windows)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the native error."
)]
fn windows_error(error: std::io::Error) -> SettingsError {
    SettingsError::Io(error.to_string())
}

#[cfg(windows)]
fn acquire_lock(path: &Path) -> Result<FileLock> {
    use rsi_files_native_fs as native;
    let parent = path
        .parent()
        .ok_or_else(|| SettingsError::InvalidInput("Settings path has no parent".into()))?;
    let directory =
        native::create_absolute_directory_and_sync_no_follow(parent).map_err(windows_error)?;
    let namespace = directory
        .pin_namespace()
        .map_err(|e| SettingsError::Io(format!("Settings writer namespace: {e}")))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| SettingsError::InvalidInput("Settings name must be UTF-8".into()))?;
    let lock_name = std::path::PathBuf::from(format!(".{name}.lock"));
    let file = match native::create_relative_private_file_no_follow(&directory, &lock_name) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            native::set_relative_private_file_permissions_no_follow(&directory, &lock_name)
                .map_err(windows_error)?;
            native::open_relative_private_file_no_follow(&directory, &lock_name)
                .map_err(windows_error)?
        }
        Err(e) => return Err(windows_error(e)),
    };
    for _ in 0..LOCK_ATTEMPTS {
        match file.try_lock() {
            Ok(()) => {
                return Ok(FileLock {
                    file,
                    _namespace: namespace,
                });
            }
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(LOCK_RETRY_DELAY),
            Err(std::fs::TryLockError::Error(e)) => return Err(windows_error(e)),
        }
    }
    Err(SettingsError::Io(
        "timed out acquiring the Settings writer lock".into(),
    ))
}

#[cfg(windows)]
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with_sequence(path, bytes, &NEXT_TEMPORARY_SEQUENCE)
}

#[cfg(windows)]
fn atomic_write_with_sequence(path: &Path, bytes: &[u8], sequence: &AtomicU64) -> Result<()> {
    use rsi_files_native_fs as native;
    let parent = path
        .parent()
        .ok_or_else(|| SettingsError::InvalidInput("Settings path has no parent".into()))?;
    let directory = native::open_absolute_directory_no_follow(parent)
        .map_err(|e| SettingsError::Io(format!("Settings staging parent: {e}")))?;
    let namespace = directory
        .pin_namespace()
        .map_err(|e| SettingsError::Io(format!("Settings staging namespace: {e}")))?;
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::InvalidInput("Settings path has no name".into()))?;
    match native::open_relative_file_no_follow(&directory, Path::new(name)) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(windows_error(e)),
    }
    let (staging_name, mut file) = create_windows_staging(&directory, sequence)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(windows_error);
    drop(file);
    let result = result
        .and_then(|()| {
            namespace
                .publish_child(
                    &staging_name,
                    Path::new(name),
                    native::EntryKind::File,
                    true,
                )
                .map_err(|e| SettingsError::Io(format!("Settings publication: {e}")))
        })
        .and_then(|()| {
            namespace
                .sync_all()
                .map_err(|e| SettingsError::Io(format!("Settings directory sync: {e}")))
        });
    if result.is_err() {
        let _ = namespace.remove_child(&staging_name);
    }
    result
}

#[cfg(windows)]
fn create_windows_staging(
    directory: &rsi_files_native_fs::Directory,
    sequence: &AtomicU64,
) -> Result<(PathBuf, File)> {
    for _ in 0..64 {
        let value = sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| SettingsError::Io("Settings sequence exhausted".into()))?;
        let name = PathBuf::from(format!(".settings-{}-{value}.tmp", std::process::id()));
        match rsi_files_native_fs::create_relative_private_file_no_follow(directory, &name) {
            Ok(file) => return Ok((name, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(SettingsError::Io(format!("Settings staging create: {e}"))),
        }
    }
    Err(SettingsError::Io(
        "Settings staging collision limit exceeded".into(),
    ))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[test]
    fn stale_staging_names_are_skipped_without_removing_their_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("private");
        rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(&parent).unwrap();
        let stale = parent.join(format!(".settings-{}-0.tmp", std::process::id()));
        std::fs::write(&stale, b"another writer").unwrap();
        let path = parent.join("settings.json");
        atomic_write_with_sequence(&path, b"new document", &AtomicU64::new(0)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new document");
        assert_eq!(std::fs::read(&stale).unwrap(), b"another writer");
    }

    #[test]
    fn staging_collision_limit_preserves_the_document_and_other_writers() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("private");
        rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(&parent).unwrap();
        for sequence in 0..64 {
            std::fs::write(
                parent.join(format!(".settings-{}-{sequence}.tmp", std::process::id())),
                b"stale",
            )
            .unwrap();
        }
        let path = parent.join("settings.json");
        std::fs::write(&path, b"old document").unwrap();
        let sequence = AtomicU64::new(0);
        assert!(atomic_write_with_sequence(&path, b"new document", &sequence).is_err());
        assert_eq!(sequence.load(Ordering::Relaxed), 64);
        assert_eq!(std::fs::read(path).unwrap(), b"old document");
        for sequence in 0..64 {
            assert_eq!(
                std::fs::read(
                    parent.join(format!(".settings-{}-{sequence}.tmp", std::process::id()))
                )
                .unwrap(),
                b"stale"
            );
        }
    }
    #[tokio::test]
    async fn native_settings_cas_preserves_unloaded_namespaces_and_reopens() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private");
        rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(&directory).unwrap();
        let config = LocalSettingsConfig {
            path: directory.join("settings.json"),
            maximum_document_bytes: 4096,
        };
        let provider = LocalProvider {
            config: config.clone(),
        };
        provider
            .compare_and_set("first", None, Some(&serde_json::json!({"value":1})))
            .await
            .unwrap();
        provider
            .compare_and_set("second", None, Some(&serde_json::json!({"value":2})))
            .await
            .unwrap();
        assert!(matches!(
            provider
                .compare_and_set("first", None, Some(&serde_json::json!({})))
                .await,
            Err(SettingsError::ConcurrentDocumentChange)
        ));
        let reopened = LocalProvider { config }.load().await.unwrap();
        assert_eq!(reopened["first"], serde_json::json!({"value":1}));
        assert_eq!(reopened["second"], serde_json::json!({"value":2}));
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 3);
    }
    #[tokio::test]
    async fn native_settings_replaces_an_existing_inherited_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private");
        rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(&directory).unwrap();
        let path = directory.join("settings.json");
        std::fs::write(&path, br#"{"other":{"keep":true}}"#).unwrap();
        let provider = LocalProvider {
            config: LocalSettingsConfig {
                path,
                maximum_document_bytes: 4096,
            },
        };
        provider
            .compare_and_set(
                "rsi.client",
                None,
                Some(&serde_json::json!({"appearance":{"theme":"light"}})),
            )
            .await
            .unwrap();
        let document = provider.load().await.unwrap();
        assert_eq!(document["other"], serde_json::json!({"keep":true}));
        assert_eq!(document["rsi.client"]["appearance"]["theme"], "light");
    }
    #[tokio::test]
    async fn native_settings_writes_refuse_unprotected_parent_before_creating_lock() {
        let temp = tempfile::tempdir().unwrap();
        let config = LocalSettingsConfig {
            path: temp.path().join("settings.json"),
            maximum_document_bytes: 4096,
        };
        let provider = LocalProvider { config };
        assert!(
            provider
                .compare_and_set("first", None, Some(&serde_json::json!({})))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
