use crate::{Error, Result};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub(super) struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(super) fn prepare(root: &Path) -> Result<(PathBuf, Lease)> {
    if !root.is_absolute() {
        return Err(Error::Input);
    }
    match fs::symlink_metadata(root) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(Error::Input);
        }
        Ok(_) => {
            // Refuse an Agent Store or any unrelated directory before creating a lease.
            for entry in fs::read_dir(root).map_err(|_| Error::Io)? {
                let entry = entry.map_err(|_| Error::Io)?;
                if ![
                    ".writer.lock",
                    "observed.sqlite3",
                    "observed.sqlite3-journal",
                ]
                .iter()
                .any(|name| entry.file_name() == *name)
                {
                    return Err(Error::Input);
                }
                if !entry.file_type().map_err(|_| Error::Io)?.is_file() {
                    return Err(Error::Input);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(|_| Error::Io)?;
        }
        Err(_) => return Err(Error::Io),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if fs::metadata(root).map_err(|_| Error::Io)?.uid() != rustix::process::geteuid().as_raw() {
            return Err(Error::Input);
        }
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Io)?;
    }
    let root = fs::canonicalize(root).map_err(|_| Error::Io)?;
    let file = open_private(&root.join(".writer.lock"))?;
    file.try_lock().map_err(|error| {
        let error: std::io::Error = error.into();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Error::Locked
        } else {
            Error::Io
        }
    })?;
    let lease = Lease(file);
    let path = root.join("observed.sqlite3");
    drop(open_private(&path)?);
    Ok((path, lease))
}
fn open_private(path: &Path) -> Result<File> {
    if fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(Error::Input);
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(
            i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).expect("native open flag"),
        );
    }
    let file = options.open(path).map_err(|_| Error::Io)?;
    let opened = file.metadata().map_err(|_| Error::Io)?;
    let named = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
    if !opened.is_file() || !named.is_file() || named.file_type().is_symlink() {
        return Err(Error::Input);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if opened.dev() != named.dev()
            || opened.ino() != named.ino()
            || opened.nlink() != 1
            || opened.uid() != rustix::process::geteuid().as_raw()
            || opened.mode() & 0o077 != 0
        {
            return Err(Error::Input);
        }
    }
    Ok(file)
}
