use super::{ContentHash, HashSubject, LoaderError};
#[cfg(all(feature = "test-failpoints", unix))]
use crate::test_failpoints;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::Builder as TempFileBuilder;

enum BoundedReadError {
    Io(io::Error),
    Unsafe,
    TooLarge,
}

#[cfg(unix)]
fn open_regular_follow(path: &Path) -> Result<fs::File, BoundedReadError> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(BoundedReadError::Io)
}

#[cfg(not(unix))]
fn open_regular_follow(path: &Path) -> Result<fs::File, BoundedReadError> {
    fs::File::open(path).map_err(BoundedReadError::Io)
}

#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> Result<fs::File, BoundedReadError> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                BoundedReadError::Unsafe
            } else {
                BoundedReadError::Io(error)
            }
        })
}

#[cfg(not(unix))]
fn open_regular_no_follow(path: &Path) -> Result<fs::File, BoundedReadError> {
    let metadata = fs::symlink_metadata(path).map_err(BoundedReadError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(BoundedReadError::Unsafe);
    }
    fs::File::open(path).map_err(BoundedReadError::Io)
}

fn read_bounded_regular_file(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Vec<u8>, BoundedReadError> {
    read_bounded_open_file(open_regular_no_follow(path)?, maximum_bytes)
}

fn read_bounded_open_file(
    mut file: fs::File,
    maximum_bytes: usize,
) -> Result<Vec<u8>, BoundedReadError> {
    let metadata = file.metadata().map_err(BoundedReadError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(BoundedReadError::Unsafe);
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err(BoundedReadError::TooLarge);
    }

    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(maximum_bytes)
            .min(maximum_bytes),
    );
    Read::by_ref(&mut file)
        .take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(BoundedReadError::Io)?;
    if bytes.len() > maximum_bytes {
        return Err(BoundedReadError::TooLarge);
    }
    Ok(bytes)
}

/// Reads a regular file through a no-follow handle and rejects it before
/// buffering when its declared or observed length exceeds `maximum_bytes`.
///
/// This is exported for the workspace's core host. `rsi-meta-loader` is a
/// private crate and this helper is not a stable SDK contract.
#[doc(hidden)]
pub fn read_bounded_file(
    path: &Path,
    operation: &'static str,
    maximum_bytes: usize,
) -> Result<Vec<u8>, LoaderError> {
    match read_bounded_regular_file(path, maximum_bytes) {
        Ok(bytes) => Ok(bytes),
        Err(BoundedReadError::Io(source)) => Err(LoaderError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }),
        Err(BoundedReadError::Unsafe) => Err(LoaderError::UnsafeInputFile {
            operation,
            path: path.to_path_buf(),
        }),
        Err(BoundedReadError::TooLarge) => Err(LoaderError::InputTooLarge {
            operation,
            path: path.to_path_buf(),
            maximum_bytes,
        }),
    }
}

/// Reads a regular file through one opened handle, following a final symlink,
/// and rejects it before buffering when it exceeds `maximum_bytes`.
///
/// `O_NONBLOCK` keeps a path replacement with a FIFO from parking the caller;
/// fstat on that same handle then enforces the regular-file requirement. This
/// is exported only for workspace composition documents, where symlinks remain
/// an intentional convenience rather than a package trust-boundary contract.
#[doc(hidden)]
pub fn read_bounded_file_following_symlinks(
    path: &Path,
    operation: &'static str,
    maximum_bytes: usize,
) -> Result<Vec<u8>, LoaderError> {
    match open_regular_follow(path).and_then(|file| read_bounded_open_file(file, maximum_bytes)) {
        Ok(bytes) => Ok(bytes),
        Err(BoundedReadError::Io(source)) => Err(LoaderError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }),
        Err(BoundedReadError::Unsafe) => Err(LoaderError::UnsafeInputFile {
            operation,
            path: path.to_path_buf(),
        }),
        Err(BoundedReadError::TooLarge) => Err(LoaderError::InputTooLarge {
            operation,
            path: path.to_path_buf(),
            maximum_bytes,
        }),
    }
}

pub(crate) fn read_file(
    path: &Path,
    operation: &'static str,
    maximum_bytes: usize,
) -> Result<Vec<u8>, LoaderError> {
    read_bounded_file(path, operation, maximum_bytes)
}

pub(super) fn resolve_confined_file(
    package_root: &Path,
    relative: &Path,
    field: &'static str,
    operation: &'static str,
) -> Result<PathBuf, LoaderError> {
    let root = fs::canonicalize(package_root).map_err(|source| LoaderError::Io {
        operation,
        path: package_root.to_path_buf(),
        source,
    })?;
    let declared_parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let physical_parent =
        fs::canonicalize(root.join(declared_parent)).map_err(|source| LoaderError::Io {
            operation,
            path: root.join(declared_parent),
            source,
        })?;
    if !physical_parent.starts_with(&root) {
        return Err(LoaderError::UnsafeManifestPath {
            field,
            path: relative.to_path_buf(),
        });
    }
    let file_name = relative
        .file_name()
        .ok_or_else(|| LoaderError::UnsafeManifestPath {
            field,
            path: relative.to_path_buf(),
        })?;
    Ok(physical_parent.join(file_name))
}

pub(super) fn open_regular_file(
    path: &Path,
    operation: &'static str,
) -> Result<fs::File, LoaderError> {
    let file = match open_regular_no_follow(path) {
        Ok(file) => file,
        Err(BoundedReadError::Io(source)) => {
            return Err(LoaderError::Io {
                operation,
                path: path.to_path_buf(),
                source,
            });
        }
        Err(BoundedReadError::Unsafe | BoundedReadError::TooLarge) => {
            return Err(LoaderError::UnsafeInputFile {
                operation,
                path: path.to_path_buf(),
            });
        }
    };
    let metadata = file.metadata().map_err(|source| LoaderError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(LoaderError::UnsafeInputFile {
            operation,
            path: path.to_path_buf(),
        });
    }
    Ok(file)
}

pub(super) fn hash_open_file(
    file: &mut fs::File,
    path: &Path,
    operation: &'static str,
) -> Result<ContentHash, LoaderError> {
    reject_oversized_artifact(file, path, operation, 0)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut observed_bytes = 0_usize;
    loop {
        let read = file.read(&mut buffer).map_err(|source| LoaderError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read);
        reject_oversized_artifact(file, path, operation, observed_bytes)?;
        digest.update(&buffer[..read]);
    }
    Ok(ContentHash(digest.finalize().into()))
}

/// Computes SHA-256 through a no-follow regular-file handle without buffering
/// the complete file. Artifacts larger than
/// [`crate::MAX_PLUGIN_ARTIFACT_BYTES`] are rejected.
///
/// This is exported for the workspace's core host. `rsi-meta-loader` is a
/// private crate and this helper is not a stable SDK contract.
#[doc(hidden)]
pub fn hash_regular_file(path: &Path, operation: &'static str) -> Result<ContentHash, LoaderError> {
    let mut file = open_regular_file(path, operation)?;
    hash_open_file(&mut file, path, operation)
}

fn reject_oversized_artifact(
    file: &fs::File,
    path: &Path,
    operation: &'static str,
    observed_bytes: usize,
) -> Result<(), LoaderError> {
    let metadata_bytes = file
        .metadata()
        .map_err(|source| LoaderError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?
        .len();
    let maximum_bytes = crate::MAX_PLUGIN_ARTIFACT_BYTES;
    let maximum_u64 = u64::try_from(maximum_bytes).unwrap_or(u64::MAX);
    if observed_bytes > maximum_bytes || metadata_bytes > maximum_u64 {
        return Err(LoaderError::InputTooLarge {
            operation,
            path: path.to_path_buf(),
            maximum_bytes,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Atomic publication keeps cleanup and fsync ordering together.
pub(super) fn publish_cache_entry(
    cache_path: &Path,
    source_path: &Path,
    source: &mut fs::File,
    expected_hash: ContentHash,
) -> Result<(), LoaderError> {
    let parent = cache_path
        .parent()
        .expect("cache entries always have a digest directory");
    fs::create_dir_all(parent).map_err(|source| LoaderError::Io {
        operation: "create plugin cache directory",
        path: parent.to_path_buf(),
        source,
    })?;

    let stage_lock_path = parent.join(".stage.lock");
    let stage_lock = open_stage_lock(&stage_lock_path)?;
    stage_lock.lock().map_err(|source| LoaderError::Io {
        operation: "lock plugin cache staging directory",
        path: stage_lock_path.clone(),
        source,
    })?;
    reap_stale_stage_files(parent)?;

    match fs::symlink_metadata(cache_path) {
        Ok(_) => return verify_cache_entry(cache_path, expected_hash).map(drop),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(LoaderError::Io {
                operation: "inspect plugin cache entry",
                path: cache_path.to_path_buf(),
                source,
            });
        }
    }

    let mut temporary = TempFileBuilder::new()
        .prefix(".rsi-meta-stage-")
        .tempfile_in(parent)
        .map_err(|source| LoaderError::Io {
            operation: "create temporary plugin cache entry",
            path: parent.to_path_buf(),
            source,
        })?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|source| LoaderError::Io {
            operation: "rewind plugin artifact",
            path: source_path.to_path_buf(),
            source,
        })?;
    let mut copied_hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut copied_bytes = 0_usize;
    loop {
        let read = source.read(&mut buffer).map_err(|source| LoaderError::Io {
            operation: "read plugin artifact",
            path: source_path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        copied_bytes = copied_bytes.saturating_add(read);
        reject_oversized_artifact(source, source_path, "read plugin artifact", copied_bytes)?;
        copied_hash.update(&buffer[..read]);
        temporary
            .write_all(&buffer[..read])
            .map_err(|source| LoaderError::Io {
                operation: "write temporary plugin cache entry",
                path: temporary.path().to_path_buf(),
                source,
            })?;
    }
    let copied_hash = ContentHash(copied_hash.finalize().into());
    if copied_hash != expected_hash {
        return Err(LoaderError::HashMismatch {
            subject: HashSubject::Artifact,
            path: source_path.to_path_buf(),
            expected: expected_hash,
            actual: copied_hash,
        });
    }
    temporary.flush().map_err(|source| LoaderError::Io {
        operation: "flush temporary plugin cache entry",
        path: temporary.path().to_path_buf(),
        source,
    })?;
    make_readonly(temporary.path())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| LoaderError::Io {
            operation: "sync temporary plugin cache entry",
            path: temporary.path().to_path_buf(),
            source,
        })?;

    #[cfg(all(feature = "test-failpoints", unix))]
    test_failpoints::gate_before_cache_publish(cache_path, expected_hash)?;

    match temporary.persist_noclobber(cache_path) {
        Ok(_) => {
            sync_directory(parent)?;
            verify_cache_entry(cache_path, expected_hash).map(drop)
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            make_writable_for_cleanup(error.file.path());
            verify_cache_entry(cache_path, expected_hash).map(drop)
        }
        Err(error) => {
            let source = error.error;
            make_writable_for_cleanup(error.file.path());
            Err(LoaderError::Io {
                operation: "publish plugin cache entry",
                path: cache_path.to_path_buf(),
                source,
            })
        }
    }
}

pub(super) fn verify_cache_entry(
    path: &Path,
    expected_hash: ContentHash,
) -> Result<fs::File, LoaderError> {
    let mut file = open_regular_file(path, "read plugin cache entry").map_err(|error| {
        if matches!(error, LoaderError::UnsafeInputFile { .. }) {
            LoaderError::InvalidCacheEntry(path.to_path_buf())
        } else {
            error
        }
    })?;
    let metadata = file.metadata().map_err(|source| LoaderError::Io {
        operation: "inspect plugin cache entry",
        path: path.to_path_buf(),
        source,
    })?;
    let actual = hash_open_file(&mut file, path, "read plugin cache entry")?;
    if actual != expected_hash {
        return Err(LoaderError::HashMismatch {
            subject: HashSubject::CachedArtifact,
            path: path.to_path_buf(),
            expected: expected_hash,
            actual,
        });
    }
    if !metadata.permissions().readonly() {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)
            .and_then(|()| file.sync_all())
            .map_err(|source| LoaderError::Io {
                operation: "make plugin cache entry read-only",
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(file)
}

fn open_stage_lock(path: &Path) -> Result<fs::File, LoaderError> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::OpenOptionsExt;

        fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    };
    #[cfg(not(unix))]
    let result = {
        fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
    };
    result.map_err(|source| LoaderError::Io {
        operation: "open plugin cache staging lock",
        path: path.to_path_buf(),
        source,
    })
}

fn reap_stale_stage_files(parent: &Path) -> Result<(), LoaderError> {
    let entries = fs::read_dir(parent).map_err(|source| LoaderError::Io {
        operation: "inspect plugin cache staging directory",
        path: parent.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| LoaderError::Io {
            operation: "inspect plugin cache staging entry",
            path: parent.to_path_buf(),
            source,
        })?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".rsi-meta-stage-")
        {
            fs::remove_file(entry.path()).map_err(|source| LoaderError::Io {
                operation: "remove stale plugin cache staging entry",
                path: entry.path(),
                source,
            })?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn ensure_private_cache_root(path: &Path) -> Result<(), LoaderError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).map_err(|source| LoaderError::Io {
        operation: "create private plugin cache root",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| LoaderError::Io {
        operation: "inspect private plugin cache root",
        path: path.to_path_buf(),
        source,
    })?;
    // SAFETY: `geteuid` has no preconditions and does not dereference memory.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(LoaderError::UnsafeCacheRoot(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn ensure_private_cache_root(path: &Path) -> Result<(), LoaderError> {
    fs::create_dir_all(path).map_err(|source| LoaderError::Io {
        operation: "create private plugin cache root",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| LoaderError::Io {
        operation: "inspect private plugin cache root",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(LoaderError::UnsafeCacheRoot(path.to_path_buf()));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), LoaderError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| LoaderError::Io {
            operation: "sync plugin cache directory",
            path: path.to_path_buf(),
            source,
        })
}

fn make_readonly(path: &Path) -> Result<(), LoaderError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| LoaderError::Io {
            operation: "inspect plugin cache permissions",
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).map_err(|source| LoaderError::Io {
        operation: "make plugin cache entry read-only",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn make_writable_for_cleanup(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}

#[cfg(not(windows))]
fn make_writable_for_cleanup(_path: &Path) {}
