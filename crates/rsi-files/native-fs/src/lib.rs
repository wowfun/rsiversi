//! Unix directory-handle filesystem operations without symlink traversal.
#![cfg(unix)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use cap_std::fs::Dir;
use std::{fs::File, path::Path};

/// Acquire an absolute directory by opening each path component without following links.
pub fn open_absolute_directory_no_follow(path: &Path) -> std::io::Result<Dir> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::path::Component;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "filesystem root is not absolute",
        ));
    }
    let root = openat(
        rustix::fs::CWD,
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut directory = Dir::from_std_file(root.into());
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => {
                directory = open_relative_directory_no_follow(&directory, Path::new(component))?;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "filesystem root is not normalized",
                ));
            }
        }
    }
    Ok(directory)
}

/// Descend from an owned directory; an empty path clones the handle.
pub fn open_relative_directory_no_follow(directory: &Dir, path: &Path) -> std::io::Result<Dir> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::os::fd::AsFd as _;
    use std::path::Component;

    let mut current = directory.try_clone()?;
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "relative directory is not normalized",
            ));
        };
        let next = openat(
            current.as_fd(),
            Path::new(component),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        current = Dir::from_std_file(next.into());
    }
    Ok(current)
}

/// Open a relative path read-only and nonblocking; the caller checks its file type.
pub fn open_relative_file_no_follow(directory: &Dir, path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::os::fd::AsFd as _;

    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file path has no name")
    })?;
    let parent = open_relative_directory_no_follow(directory, parent)?;
    let file = openat(
        parent.as_fd(),
        Path::new(name),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    Ok(file.into())
}

/// Whether a component open rejected a symlink or a non-directory parent.
pub fn is_link_rejection(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| code == libc::ELOOP || code == libc::ENOTDIR)
}
