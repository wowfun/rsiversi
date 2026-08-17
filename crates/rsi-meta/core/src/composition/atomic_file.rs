use std::fs::{self, OpenOptions as FileOpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::PreparedComposition;
use crate::host::CompositionFiles;
use crate::model::CompositionLock;
use crate::{HostError, Result};

pub(crate) fn write_lock_create_new(path: &Path, lock: &CompositionLock) -> Result<()> {
    let source = toml::to_string_pretty(lock)
        .map_err(|error| HostError::InvalidManifest(error.to_string()))?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| HostError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let temporary = path.with_extension(format!("lock-tmp-{}", uuid::Uuid::now_v7()));
    let mut file = FileOpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| HostError::Io {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file
        .write_all(source.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(HostError::Io {
            path: temporary,
            source,
        });
    }
    drop(file);
    if let Err(source) = fs::hard_link(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return if source.kind() == std::io::ErrorKind::AlreadyExists {
            Err(HostError::LockAlreadyExists {
                path: path.to_owned(),
            })
        } else {
            Err(HostError::Io {
                path: path.to_owned(),
                source,
            })
        };
    }
    fs::remove_file(&temporary).map_err(|source| HostError::Io {
        path: temporary,
        source,
    })?;
    sync_parent(path)
}

pub(crate) fn install_pair(
    prepared: &PreparedComposition,
    installed: &CompositionFiles,
    command_id: &str,
) -> Result<()> {
    #[cfg(not(feature = "test-failpoints"))]
    let _ = command_id;
    // The lock is installed last and is the recovery commit marker.
    write_bytes_atomic(&installed.manifest_path, &prepared.manifest_bytes)?;
    #[cfg(feature = "test-failpoints")]
    crate::test_failpoints::gate(
        command_id,
        crate::test_failpoints::CrashPoint::ManifestReplacedBeforeLock,
    );
    write_bytes_atomic(&installed.lock_path, &prepared.lock_bytes)
}

pub(crate) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent != Path::new(".") {
        fs::create_dir_all(parent).map_err(|source| HostError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    reap_stale_atomic_files(parent, path);
    let mut temporary = AtomicFile::create(parent, path)?;
    temporary.write_all(bytes)?;
    temporary.commit(path)?;
    sync_parent(path)
}

struct AtomicFile {
    path: PathBuf,
    file: Option<std::fs::File>,
    committed: bool,
}

impl AtomicFile {
    fn create(parent: &Path, target: &Path) -> Result<Self> {
        let target_name = target
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("document");
        let path = parent.join(format!(
            ".rsi-meta-write-{target_name}-{}",
            uuid::Uuid::now_v7()
        ));
        let file = FileOpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|source| HostError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            path,
            file: Some(file),
            committed: false,
        })
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let file = self
            .file
            .as_mut()
            .expect("uncommitted atomic file has an open descriptor");
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| HostError::Io {
                path: self.path.clone(),
                source,
            })
    }

    fn commit(&mut self, target: &Path) -> Result<()> {
        drop(self.file.take());
        fs::rename(&self.path, target).map_err(|source| HostError::Io {
            path: target.to_owned(),
            source,
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        FileOpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| HostError::Io {
                path: parent.to_owned(),
                source,
            })?;
    }
    Ok(())
}

fn reap_stale_atomic_files(parent: &Path, target: &Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_hours(24);
    let Some(target_name) = target.file_name().and_then(std::ffi::OsStr::to_str) else {
        return;
    };
    let prefix = format!(".rsi-meta-write-{target_name}-");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if stale {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_uncommitted_atomic_file_removes_its_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("composition.toml");
        let temporary_path = {
            let mut atomic = AtomicFile::create(directory.path(), &target).unwrap();
            atomic.write_all(b"not committed").unwrap();
            atomic.path.clone()
        };
        assert!(!temporary_path.exists());
        assert!(!target.exists());
    }
}
