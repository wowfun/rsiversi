use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

pub(crate) fn read_profile_source(
    path: &Path,
    maximum_bytes: usize,
) -> std::io::Result<(PathBuf, Vec<u8>)> {
    let initial = path.symlink_metadata()?;
    if !initial.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Profile source must be a regular non-symlink file",
        ));
    }
    let file = open_profile_file(path)?;
    let opened = file.metadata()?;
    let current = path.symlink_metadata()?;
    if !opened.file_type().is_file() || !current.file_type().is_file() {
        return Err(changed_profile_source());
    }
    #[cfg(not(windows))]
    if !same_file_identity(&initial, &opened) || !same_file_identity(&current, &opened) {
        return Err(changed_profile_source());
    }
    #[cfg(windows)]
    let opened_identity = profile_file_identity(&file)?;
    #[cfg(windows)]
    if profile_path_identity(path)? != opened_identity {
        return Err(changed_profile_source());
    }
    let canonical = path.canonicalize()?;
    #[cfg(not(windows))]
    if !same_file_identity(&canonical.symlink_metadata()?, &opened) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Profile source identity changed while resolving its canonical path",
        ));
    }
    #[cfg(windows)]
    if profile_path_identity(&canonical)? != opened_identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Profile source identity changed while resolving its canonical path",
        ));
    }
    read_open_file_bounded(file, maximum_bytes).map(|bytes| (canonical, bytes))
}

fn open_profile_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
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

fn changed_profile_source() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "Profile source changed while opening or is not a regular file",
    )
}

pub(crate) fn read_file_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    read_profile_source(path, maximum_bytes).map(|(_, bytes)| bytes)
}

fn read_open_file_bounded(file: File, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    if file.metadata()?.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Profile source exceeds its document bound",
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Profile source exceeds its document bound",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn profile_file_identity(file: &File) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_file(file.try_clone()?)
}

#[cfg(windows)]
fn profile_path_identity(path: &Path) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_file(open_profile_file(path)?)
}

#[cfg(not(any(unix, windows)))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && left.file_type().is_file()
        && right.file_type().is_file()
}
