use rsi_files_native_fs::{Directory, DirectoryCreationError};
use std::{fs::File, io, path::Path};

pub(super) fn private_directory(directory: &Directory) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = directory.dir_metadata()?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "spill directory must be private to its owner",
            ));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        directory.verify_private()
    }
}

pub(super) fn scope_directory(
    root: &Directory,
    scope: &str,
    create: bool,
) -> io::Result<Directory> {
    let directory =
        match rsi_files_native_fs::open_relative_directory_no_follow(root, Path::new(scope)) {
            Ok(directory) => directory,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                rsi_files_native_fs::create_directory_no_follow(root, scope.as_ref()).map_err(
                    |error| match error {
                        DirectoryCreationError::NotCreated(e)
                        | DirectoryCreationError::Created(e) => e,
                    },
                )?
            }
            Err(error) => return Err(error),
        };
    private_directory(&directory)?;
    Ok(directory)
}

pub(super) fn open_file(directory: &Directory, name: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = rsi_files_native_fs::open_relative_file_no_follow(directory, name)?;
    #[cfg(windows)]
    let file = rsi_files_native_fs::open_relative_private_file_no_follow(directory, name)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("spill object is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(io::Error::other(
                "spill object is not a private single-link file",
            ));
        }
    }
    #[cfg(windows)]
    if rsi_files_native_fs::file_version(&file)?.link_count != 1 {
        return Err(io::Error::other("spill object is not a single-link file"));
    }
    Ok(file)
}

pub(super) fn create_file(directory: &Directory, name: &Path) -> io::Result<File> {
    rsi_files_native_fs::create_relative_private_file_no_follow(directory, name)
}
pub(super) fn publish(directory: &Directory, source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        directory.link_child(directory, source, destination)
    }
    #[cfg(windows)]
    {
        directory.publish_child(
            source,
            destination,
            rsi_files_native_fs::EntryKind::File,
            false,
        )
    }
}
pub(super) fn remove(directory: &Directory, name: &Path) -> io::Result<()> {
    match directory.remove_child(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(unix)]
pub(super) fn free_bytes(directory: &Directory, _: &Path) -> io::Result<u64> {
    let space = rustix::fs::fstatvfs(directory)?;
    Ok(space.f_bavail.saturating_mul(space.f_frsize))
}
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn free_bytes(_: &Directory, path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt as _;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    // SAFETY: path is a live NUL-terminated UTF-16 buffer, and available is a
    // writable u64 for the duration of this synchronous call. Optional outputs
    // are null. The caller retains the directory's pinned namespace.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &raw mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(available)
    }
}
