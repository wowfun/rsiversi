use rsi_api_protocol::{ApiError, Result};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};
fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::Invalid(e.to_string())
}
#[derive(Debug)]
pub(super) struct Root {
    pub path: PathBuf,
    _lease: Lease,
}
impl Root {
    pub async fn open(path: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            let path =
                rsi_files_native_fs::resolve_absolute_root_alias(&path, true).map_err(invalid)?;
            if !path.is_absolute() {
                return Err(invalid("review scratch root must be absolute"));
            }
            #[cfg(unix)]
            let _root =
                rsi_files_native_fs::create_absolute_directory_no_follow(&path).map_err(invalid)?;
            #[cfg(not(unix))]
            std::fs::create_dir_all(&path).map_err(invalid)?;
            private_directory(&path)?;
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600).custom_flags(
                    i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).expect("native flag"),
                );
            }
            let lease = options.open(path.join(".writer.lock")).map_err(invalid)?;
            let metadata = lease.metadata().map_err(invalid)?;
            if !metadata.is_file() {
                return Err(invalid("review lease must be a regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if metadata.uid() != rustix::process::geteuid().as_raw()
                    || metadata.nlink() != 1
                    || metadata.mode() & 0o077 != 0
                {
                    return Err(invalid("review lease is not private"));
                }
            }
            if let Err(error) = lease.try_lock() {
                let error: std::io::Error = error.into();
                return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                    ApiError::Capacity
                } else {
                    invalid(error)
                });
            }
            let lease = Lease(lease);
            let mut old = Vec::new();
            for entry in std::fs::read_dir(&path).map_err(invalid)?.take(65) {
                let entry = entry.map_err(invalid)?;
                if entry.file_name() == ".writer.lock" {
                    continue;
                }
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| invalid("unrelated review scratch entry"))?;
                if !name.starts_with("interval-")
                    || name.len() > 128
                    || !entry.file_type().map_err(invalid)?.is_dir()
                {
                    return Err(invalid("unrelated review scratch entry"));
                }
                private_directory(&entry.path())?;
                old.push(entry.path());
            }
            if old.len() > 32 {
                return Err(invalid("review scratch recovery exceeds directory bound"));
            }
            for path in old {
                std::fs::remove_dir_all(path).map_err(invalid)?;
            }
            Ok(Self {
                path,
                _lease: lease,
            })
        })
        .await
        .map_err(invalid)?
    }
}
fn private_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(invalid)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("review scratch is not a private directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(invalid("review scratch directory permissions mismatch"));
        }
    }
    Ok(())
}

pub(super) fn interval_directory(root: &Path) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("interval-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(root)
}

#[derive(Debug)]
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(all(test, unix))]
mod lease_tests {
    use super::*;

    #[tokio::test]
    async fn duplicate_does_not_extend_root_lease_or_unlock_next_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let first = Root::open(directory.path().to_owned()).await.unwrap();
        let Root { _lease: lease, .. } = &first;
        let duplicate = lease.0.try_clone().unwrap();
        assert!(matches!(
            Root::open(directory.path().to_owned()).await,
            Err(ApiError::Capacity)
        ));
        drop(first);
        let second = Root::open(directory.path().to_owned()).await.unwrap();
        drop(duplicate);
        assert!(matches!(
            Root::open(directory.path().to_owned()).await,
            Err(ApiError::Capacity)
        ));
        drop(second);
        // Recovery fails after acquisition, and must release its lease too.
        std::fs::write(directory.path().join("unrelated"), b"keep").unwrap();
        assert!(matches!(
            Root::open(directory.path().to_owned()).await,
            Err(ApiError::Invalid(_))
        ));
        let file = OpenOptions::new()
            .write(true)
            .open(directory.path().join(".writer.lock"))
            .unwrap();
        file.try_lock().unwrap();
        file.unlock().unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("unrelated")).unwrap(),
            b"keep"
        );
    }
}
