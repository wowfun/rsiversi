//! Unix directory-handle filesystem operations without symlink traversal.
#![cfg(unix)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use cap_std::fs::Dir;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

/// Resolve only the caller-trusted first component below `/`, preserving the suffix.
/// This grants no directory authority; use no-follow acquisition on the result.
pub fn resolve_absolute_root_alias(path: &Path, allow_missing: bool) -> std::io::Result<PathBuf> {
    validate_absolute_root(path)?;
    let mut components = path.components();
    let _root = components.next();
    let Some(first) = components.next() else {
        return Ok(PathBuf::from("/"));
    };
    let logical_first = Path::new("/").join(first);
    let mut resolved = match std::fs::canonicalize(&logical_first) {
        Ok(path) => path,
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
            logical_first
        }
        Err(error) => return Err(error),
    };
    validate_absolute_root(&resolved)?;
    resolved.extend(components);
    Ok(resolved)
}

fn validate_absolute_root(path: &Path) -> std::io::Result<()> {
    use std::path::Component;
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "filesystem root is not absolute, normalized and NUL-free",
        ));
    }
    Ok(())
}

/// Acquire an absolute directory by opening each path component without following links.
pub fn open_absolute_directory_no_follow(path: &Path) -> std::io::Result<Dir> {
    acquire_absolute_directory(path, false)
}

/// Acquire a root without links, creating missing private directory components.
/// Existing permissions are preserved; created permissions are 0700 subject to umask.
/// Invalid components fail before mutation. An I/O failure may leave created parents.
pub fn create_absolute_directory_no_follow(path: &Path) -> std::io::Result<Dir> {
    acquire_absolute_directory(path, true)
}

fn acquire_absolute_directory(path: &Path, create: bool) -> std::io::Result<Dir> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    use std::path::Component;

    validate_absolute_root(path)?;
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
                let next = open_relative_directory_no_follow(&directory, Path::new(component));
                directory = match next {
                    Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                        match mkdirat(&directory, component, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => return Err(error.into()),
                        }
                        open_relative_directory_no_follow(&directory, Path::new(component))?
                    }
                    result => result?,
                };
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
