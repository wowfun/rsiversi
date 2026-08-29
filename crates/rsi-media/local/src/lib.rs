//! Local immutable CAS backend for canonical Media objects.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_media_protocol::{
    MAXIMUM_IMAGE_DESCRIPTOR_BYTES, MediaBackend, MediaBackendContract, MediaError, MediaId,
    MediaRef, Result, StoredMedia,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const MAXIMUM_HEADER_BYTES: usize = 4 * 1024;
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
        Ok(())
    }
}

#[derive(Debug)]
struct Backend {
    root: PathBuf,
}

#[async_trait]
impl MediaBackend for Backend {
    async fn put(&self, media: StoredMedia) -> Result<()> {
        verify(&media)?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let path = object_path(&root, &media.reference.id);
            match read_object(&path, &media.reference.id) {
                Ok(existing) => {
                    if existing.reference == media.reference && existing.bytes == media.bytes {
                        return Ok(());
                    }
                    return Err(MediaError::Corrupt(
                        "existing MediaId has different content".into(),
                    ));
                }
                Err(MediaError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
            write_object(&path, &media)?;
            Ok(())
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    async fn get(&self, id: &MediaId) -> Result<StoredMedia> {
        let root = self.root.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || read_object(&object_path(&root, &id), &id))
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
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config.root.as_os_str().len() + 32;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<LocalMediaConfig>()?;
        let root = config.root;
        let setup_root = root.clone();
        tokio::task::spawn_blocking(move || {
            fs::create_dir_all(setup_root.join("objects"))
                .map_err(|error| MediaError::Io(error.to_string()))?;
            set_directory_permissions(&setup_root.join("objects"))
        })
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let backend: Arc<dyn MediaBackend> = Arc::new(Backend { root });
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

fn object_path(root: &Path, id: &MediaId) -> PathBuf {
    root.join("objects")
        .join(&id.as_str()[..2])
        .join(format!("{}.rsi-media", id.as_str()))
}

fn read_object(path: &Path, expected_id: &MediaId) -> Result<StoredMedia> {
    let maximum_bytes = MAXIMUM_HEADER_BYTES
        + 1
        + usize::try_from(MAXIMUM_IMAGE_DESCRIPTOR_BYTES).unwrap_or(usize::MAX);
    let bytes = match read_file_bounded(path, maximum_bytes) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(MediaError::NotFound(expected_id.clone()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(MediaError::Corrupt(error.to_string()));
        }
        Err(error) => return Err(MediaError::Io(error.to_string())),
    };
    let newline = bytes
        .iter()
        .take(MAXIMUM_HEADER_BYTES + 1)
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| MediaError::Corrupt("object header terminator is missing".into()))?;
    let reference: MediaRef = serde_json::from_slice(&bytes[..newline])
        .map_err(|error| MediaError::Corrupt(error.to_string()))?;
    if reference.id != *expected_id {
        return Err(MediaError::Corrupt(
            "object path identity does not match its header".into(),
        ));
    }
    let media = StoredMedia {
        reference,
        bytes: Arc::from(bytes[newline + 1..].to_vec()),
    };
    verify(&media)?;
    Ok(media)
}

fn read_file_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    let file = open_unchanged_regular_file(path, "Media object")?;
    if file.metadata()?.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "object envelope is too large",
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "object envelope is too large",
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

fn write_object(path: &Path, media: &StoredMedia) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MediaError::InvalidInput("object path has no parent".into()))?;
    fs::create_dir_all(parent).map_err(|error| MediaError::Io(error.to_string()))?;
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
        match fs::hard_link(&temporary, path) {
            Ok(()) => {
                fs::remove_file(&temporary).map_err(|error| MediaError::Io(error.to_string()))?;
            }
            Err(_error) if path.exists() => {
                let _ = fs::remove_file(&temporary);
                let existing = read_object(path, &media.reference.id)?;
                if existing.reference != media.reference || existing.bytes != media.bytes {
                    return Err(MediaError::Corrupt(
                        "concurrent MediaId publication differs".into(),
                    ));
                }
                return Ok(());
            }
            Err(error) => return Err(MediaError::Io(error.to_string())),
        }
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| MediaError::Io(error.to_string()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn verify(media: &StoredMedia) -> Result<()> {
    media.reference.validate()?;
    if media.bytes.len()
        != usize::try_from(media.reference.bytes)
            .map_err(|_| MediaError::Corrupt("media length overflows usize".into()))?
    {
        return Err(MediaError::Corrupt(
            "media byte length does not match its reference".into(),
        ));
    }
    let digest = hex::encode(Sha256::digest(&media.bytes));
    if digest != media.reference.id.as_str() {
        return Err(MediaError::Corrupt(
            "media bytes do not match their SHA-256 identity".into(),
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

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_open_file_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MediaError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn set_open_file_permissions(_file: &File) -> Result<()> {
    Ok(())
}
