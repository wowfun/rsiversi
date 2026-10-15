//! Local immutable CAS backend for canonical Media objects.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_api_protocol::{ByteBudget, RetainedBytes};
use rsi_media_protocol::{
    MAXIMUM_IMAGE_DESCRIPTOR_BYTES, MediaBackend, MediaBackendContract, MediaError, MediaId,
    MediaRef, Result, StoredMedia,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(not(windows), test))]
use std::fs;
use std::fs::File;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::Read;
#[cfg(not(windows))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(windows)]
mod windows;

const MAXIMUM_HEADER_BYTES: usize = 4 * 1024;
const MAXIMUM_ENVELOPE_BYTES: u64 =
    MAXIMUM_IMAGE_DESCRIPTOR_BYTES + MAXIMUM_HEADER_BYTES as u64 + 1;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Configuration accepted by [`LocalMediaBackendFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalMediaConfig {
    /// Absolute CAS root.
    pub root: PathBuf,
}

impl LocalMediaConfig {
    fn validate(&self) -> Result<()> {
        if !self.root.is_absolute() {
            return Err(MediaError::InvalidInput(
                "local Media root must be absolute".into(),
            ));
        }
        #[cfg(windows)]
        rsi_files_native_fs::resolve_absolute_root_alias(&self.root, true)
            .map_err(|error| MediaError::InvalidInput(error.to_string()))?;
        require_native_platform()
    }
}

#[derive(Debug)]
struct Backend {
    #[cfg(not(windows))]
    root: PathBuf,
    reads: ByteBudget,
    io: Arc<tokio::sync::Semaphore>,
    #[cfg(windows)]
    store: Arc<windows::Store>,
}

#[async_trait]
impl MediaBackend for Backend {
    async fn put(&self, media: StoredMedia) -> Result<()> {
        verify_shape(&media)?;
        #[cfg(not(windows))]
        let root = self.root.clone();
        let permit = self
            .io
            .clone()
            .try_acquire_owned()
            .map_err(|_| MediaError::AdmissionFull("local Media I/O tasks".into()))?;
        let reads = self.reads.clone();
        #[cfg(windows)]
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            verify(&media)?;
            #[cfg(windows)]
            {
                store.put(&media, &reads)
            }
            #[cfg(not(windows))]
            {
                let path = object_path(&root, &media.reference.id);
                match read_object(&path, &media.reference.id, &reads) {
                    Ok(existing) => {
                        if existing.reference == media.reference && existing.bytes == media.bytes {
                            return sync_publication(&path, sync_directory)
                                .map_err(|_| publication_unknown());
                        }
                        return Err(MediaError::Corrupt(
                            "existing MediaId has different content".into(),
                        ));
                    }
                    Err(MediaError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                }
                write_object(&path, &media, &reads)?;
                Ok(())
            }
        })
        .await
        .map_err(|_| publication_unknown())?
    }

    async fn get(&self, id: &MediaId) -> Result<StoredMedia> {
        #[cfg(not(windows))]
        let root = self.root.clone();
        let id = id.clone();
        let permit = self
            .io
            .clone()
            .try_acquire_owned()
            .map_err(|_| MediaError::AdmissionFull("local Media I/O tasks".into()))?;
        let reads = self.reads.clone();
        #[cfg(windows)]
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(windows)]
            {
                store.get(&id, &reads)
            }
            #[cfg(not(windows))]
            {
                read_object(&object_path(&root, &id), &id, &reads)
            }
        })
        .await
        .map_err(|error| join_error(&error))?
    }
}

/// Ordinary plugin factory for one local Media backend.
#[derive(Clone, Debug, Default)]
pub struct LocalMediaBackendFactory;

#[async_trait]
impl PluginFactory for LocalMediaBackendFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: LocalMediaConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config.validate().map_err(|error| match error {
            MediaError::Unsupported(_) => MetaError::Activation(error.to_string()),
            _ => MetaError::InvalidInput(error.to_string()),
        })?;
        let retained = config.root.as_os_str().len() + 32;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        require_native_platform().map_err(|error| MetaError::Activation(error.to_string()))?;
        let config = plan.take_state::<LocalMediaConfig>()?;
        let root = config.root;
        let setup_root = root.clone();
        #[cfg(windows)]
        let store = tokio::task::spawn_blocking(move || windows::Store::open(&setup_root))
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        #[cfg(not(windows))]
        tokio::task::spawn_blocking(move || prepare_objects(&setup_root))
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let backend: Arc<dyn MediaBackend> = Arc::new(Backend {
            #[cfg(windows)]
            store: Arc::new(store),
            #[cfg(not(windows))]
            root,
            reads: ByteBudget::default(),
            io: Arc::new(tokio::sync::Semaphore::new(64)),
        });
        let supply = plan
            .context()
            .provide_local::<MediaBackendContract>(backend)?;
        plan.defer(
            "withdraw local Media backend",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn join_error(error: &tokio::task::JoinError) -> MediaError {
    MediaError::Io(format!("Media blocking task failed: {error}"))
}

#[cfg(any(not(windows), test))]
fn object_path(root: &Path, id: &MediaId) -> PathBuf {
    root.join("objects")
        .join(&id.as_str()[..2])
        .join(format!("{}.rsi-media", id.as_str()))
}

#[cfg(not(windows))]
fn read_object(path: &Path, expected_id: &MediaId, reads: &ByteBudget) -> Result<StoredMedia> {
    require_native_platform()?;
    let bytes = read_file_bounded(path, expected_id, reads)?;
    decode_object(bytes, expected_id)
}

fn decode_object(bytes: RetainedBytes, expected_id: &MediaId) -> Result<StoredMedia> {
    let newline = bytes
        .as_bytes()
        .iter()
        .take(MAXIMUM_HEADER_BYTES + 1)
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| MediaError::Corrupt("object header terminator is missing".into()))?;
    let reference: MediaRef = serde_json::from_slice(&bytes.as_bytes()[..newline])
        .map_err(|error| MediaError::Corrupt(error.to_string()))?;
    if reference.id != *expected_id {
        return Err(MediaError::Corrupt(
            "object path identity does not match its header".into(),
        ));
    }
    let media = StoredMedia {
        reference,
        bytes: bytes.into_bytes().slice(newline + 1..),
    };
    verify(&media)?;
    Ok(media)
}

#[cfg(not(windows))]
fn read_file_bounded(path: &Path, id: &MediaId, reads: &ByteBudget) -> Result<RetainedBytes> {
    let file = open_unchanged_regular_file(path, "Media object").map_err(|e| read_error(&e, id))?;
    read_opened_file(file, id, reads)
}

fn read_error(error: &std::io::Error, id: &MediaId) -> MediaError {
    match error.kind() {
        std::io::ErrorKind::NotFound => MediaError::NotFound(id.clone()),
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
            MediaError::Corrupt(error.to_string())
        }
        _ => MediaError::Io(error.to_string()),
    }
}

fn read_opened_file(mut file: File, id: &MediaId, reads: &ByteBudget) -> Result<RetainedBytes> {
    let map_io = |error| read_error(&error, id);
    let length = file.metadata().map_err(map_io)?.len();
    if length > MAXIMUM_ENVELOPE_BYTES {
        return Err(MediaError::Corrupt("object envelope is too large".into()));
    }
    let length = usize::try_from(length).expect("bounded file length");
    let capacity = reads
        .reserve(length)
        .map_err(|_| MediaError::AdmissionFull("local Media read bytes".into()))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes).map_err(map_io)?;
    if file.read(&mut [0]).map_err(map_io)? != 0 {
        return Err(MediaError::Corrupt(
            "object envelope changed length while reading".into(),
        ));
    }
    capacity
        .retain_vec(bytes)
        .map_err(|error| MediaError::Io(error.to_string()))
}

#[cfg(not(windows))]
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

#[cfg(not(windows))]
fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[cfg(not(windows))]
fn not_regular_file(label: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{label} must be a regular non-symlink file"),
    )
}

#[cfg(not(windows))]
fn changed_file(label: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{label} changed while opening"),
    )
}

#[cfg(not(windows))]
fn write_object(path: &Path, media: &StoredMedia, reads: &ByteBudget) -> Result<()> {
    write_object_with_sync(path, media, reads, sync_directory)
}

#[cfg(unix)]
fn sync_directory(parent: &Path) -> std::io::Result<()> {
    File::open(parent).and_then(|directory| directory.sync_all())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_parent: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Media native directory synchronization is unsupported on this platform",
    ))
}

fn publication_unknown() -> MediaError {
    MediaError::Api(rsi_api_protocol::ApiError::OutcomeUnknown)
}

#[cfg(not(windows))]
fn prepare_objects(root: &Path) -> Result<()> {
    require_native_platform()?;
    let objects = root.join("objects");
    fs::create_dir_all(&objects).map_err(|error| MediaError::Io(error.to_string()))?;
    set_directory_permissions(&objects)?;
    // Establish every namespace entry created by activation, including the CAS root.
    for directory in objects.ancestors() {
        sync_directory(directory).map_err(|error| MediaError::Io(error.to_string()))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_publication(
    path: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let shard = path.parent().expect("owned object path");
    sync(shard)?;
    sync(shard.parent().expect("owned objects directory"))
}

#[cfg(not(windows))]
fn write_object_with_sync(
    path: &Path,
    media: &StoredMedia,
    reads: &ByteBudget,
    sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<()> {
    require_native_platform()?;
    let parent = path
        .parent()
        .ok_or_else(|| MediaError::InvalidInput("object path has no parent".into()))?;
    fs::create_dir(parent)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(|error| MediaError::Io(error.to_string()))?;
    set_directory_permissions(parent)?;
    let header =
        serde_json::to_vec(&media.reference).map_err(|error| MediaError::Io(error.to_string()))?;
    if header.len() > MAXIMUM_HEADER_BYTES {
        return Err(MediaError::InvalidInput(
            "object header is too large".into(),
        ));
    }
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        media.reference.id,
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        #[cfg(not(windows))]
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| MediaError::Io(error.to_string()))?;
        set_open_file_permissions(&file)?;
        file.write_all(&header)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.write_all(&media.bytes))
            .and_then(|()| file.sync_all())
            .map_err(|error| MediaError::Io(error.to_string()))?;
        drop(file);
        match fs::hard_link(&temporary, path) {
            Ok(()) => {
                fs::remove_file(&temporary).map_err(|_| publication_unknown())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary).map_err(|_| publication_unknown())?;
                let existing = read_object(path, &media.reference.id, reads)?;
                if existing.reference != media.reference || existing.bytes != media.bytes {
                    return Err(MediaError::Corrupt(
                        "concurrent MediaId publication differs".into(),
                    ));
                }
            }
            Err(error) => return Err(MediaError::Io(error.to_string())),
        }
        sync_publication(path, sync).map_err(|_| publication_unknown())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn verify(media: &StoredMedia) -> Result<()> {
    verify_shape(media)?;
    let digest = hex::encode(Sha256::digest(&media.bytes));
    if digest != media.reference.id.as_str() {
        return Err(MediaError::Corrupt(
            "media bytes do not match their SHA-256 identity".into(),
        ));
    }
    Ok(())
}
fn verify_shape(media: &StoredMedia) -> Result<()> {
    media.reference.validate()?;
    if media.bytes.len()
        != usize::try_from(media.reference.bytes)
            .map_err(|_| MediaError::Corrupt("media length overflows usize".into()))?
    {
        return Err(MediaError::Corrupt(
            "media byte length does not match its reference".into(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| MediaError::Io(error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn set_directory_permissions(_path: &Path) -> Result<()> {
    require_native_platform()
}

#[cfg(unix)]
fn set_open_file_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MediaError::Io(error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn set_open_file_permissions(_file: &File) -> Result<()> {
    require_native_platform()
}

fn require_native_platform() -> Result<()> {
    if cfg!(any(unix, windows)) {
        Ok(())
    } else {
        Err(MediaError::Unsupported(
            "Media native filesystem protection is unsupported on this platform".into(),
        ))
    }
}

#[cfg(all(test, not(any(unix, windows))))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn refusal_precedes_cas_creation_and_existing_object_reads() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            LocalMediaBackendFactory
                .prepare(&serde_json::json!({ "root": root.path().join("missing") }))
                .is_err()
        );
        assert!(matches!(
            prepare_objects(&root.path().join("missing")),
            Err(MediaError::Unsupported(_))
        ));
        let path = root.path().join("object");
        fs::write(&path, b"retained").unwrap();
        let id = MediaId::new("a".repeat(64)).unwrap();
        assert!(matches!(
            read_object(&path, &id, &ByteBudget::default()),
            Err(MediaError::Unsupported(_))
        ));
        assert!(!root.path().join("missing").exists());
        assert_eq!(fs::read(path).unwrap(), b"retained");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod config_validation_tests {
    use super::*;

    #[test]
    fn malformed_configuration_precedes_platform_admission() {
        assert!(matches!(
            LocalMediaConfig {
                root: "relative".into()
            }
            .validate(),
            Err(MediaError::InvalidInput(_))
        ));
    }

    #[test]
    fn valid_configuration_checks_platform_without_filesystem_effects() {
        let root = tempfile::tempdir().unwrap();
        let result = LocalMediaConfig {
            root: root.path().join("missing"),
        }
        .validate();
        if cfg!(any(unix, windows)) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(MediaError::Unsupported(_))));
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
            LocalMediaBackendFactory.prepare(&serde_json::json!({"root":"relative"})),
            Err(MetaError::InvalidInput(_))
        ));
        let result = LocalMediaBackendFactory
            .prepare(&serde_json::json!({"root":root.path().join("missing/media")}));
        if cfg!(any(unix, windows)) {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(MetaError::Activation(_))));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn backend(root: &Path) -> Backend {
        Backend {
            reads: ByteBudget::default(),
            io: Arc::new(tokio::sync::Semaphore::new(64)),
            store: Arc::new(windows::Store::open(root).unwrap()),
        }
    }

    #[tokio::test]
    async fn native_media_publication_reuses_exact_bytes_and_rejects_corruption() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("private-media");
        let backend = backend(&root);
        let bytes = bytes::Bytes::from_static(
            b"canonical body identity belongs to the caller's normalizer",
        );
        let reference = MediaRef {
            id: MediaId::new(hex::encode(Sha256::digest(&bytes))).unwrap(),
            mime: "image/png".into(),
            bytes: bytes.len() as u64,
            width: 1,
            height: 1,
        };
        let media = StoredMedia {
            reference: reference.clone(),
            bytes: bytes.clone(),
        };
        let shard = object_path(&root, &reference.id)
            .parent()
            .unwrap()
            .to_owned();
        let mut sync_count = 0;
        let result = backend
            .store
            .put_with_sync(&media, &backend.reads, |namespace| {
                assert!(std::fs::rename(root.join("objects"), root.join("moved-objects")).is_err());
                assert!(std::fs::rename(&shard, root.join("moved-shard")).is_err());
                sync_count += 1;
                if sync_count == 2 {
                    return Err(std::io::Error::other("injected sync failure"));
                }
                namespace.sync_all()
            });
        assert!(matches!(
            result,
            Err(MediaError::Api(rsi_api_protocol::ApiError::OutcomeUnknown))
        ));
        assert_eq!(sync_count, 2);
        assert_eq!(backend.get(&reference.id).await.unwrap().bytes, bytes);
        backend.put(media.clone()).await.unwrap();
        backend.put(media).await.unwrap();
        assert_eq!(backend.get(&reference.id).await.unwrap().bytes, bytes);
        let path = object_path(&root, &reference.id);
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(backend.get(&reference.id).await.is_err());
        assert!(std::fs::rename(&root, temporary.path().join("moved")).is_err());
        drop(backend);
        std::fs::rename(&root, temporary.path().join("moved")).unwrap();
    }
}
