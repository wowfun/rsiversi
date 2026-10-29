//! Unix directory-handle mechanics.
use crate::{DirectoryCreationError, EntryKind};

use std::{
    fs::File,
    path::{Path, PathBuf},
};

/// An owned no-follow directory capability, without path-based directory methods.
#[derive(Debug)]
pub struct Directory(File);

impl Directory {
    /// Removes a symbolic link itself through retained parent authority.
    pub fn remove_link(&self, name: &Path) -> std::io::Result<()> {
        if self.child_kind(name)? != EntryKind::Link {
            return Err(std::io::Error::other("entry is not a link"));
        }
        self.remove_child(name)
    }
    /// Classify exactly one child without following links or enumerating siblings.
    pub fn child_kind(&self, name: &Path) -> std::io::Result<EntryKind> {
        single_name(name)?;
        let stat = rustix::fs::statat(self, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
            rustix::fs::FileType::RegularFile => EntryKind::File,
            rustix::fs::FileType::Directory => EntryKind::Directory,
            rustix::fs::FileType::Symlink => EntryKind::Link,
            _ => EntryKind::Other,
        })
    }
    /// Clone the same native directory authority without reopening its path.
    pub fn try_clone(&self) -> std::io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    /// Transfer the owned Unix handle to a consumer's handle-based operations.
    pub fn into_std_file(self) -> File {
        self.0
    }

    /// Inspect the retained directory, without looking up its absolute name.
    pub fn dir_metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.0.metadata()
    }

    /// Adopt an owned directory handle without reopening its name.
    /// The caller owns the original acquisition policy; this verifies object type.
    pub fn from_opened_file(file: File) -> std::io::Result<Self> {
        if !file.metadata()?.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "opened object is not a directory",
            ));
        }
        Ok(Self(file))
    }

    /// Enumerate the retained directory; callers bound and cancel iteration.
    pub fn entries(&self) -> std::io::Result<DirectoryEntries> {
        cap_std::fs::Dir::from_std_file(self.0.try_clone()?)
            .entries()
            .map(DirectoryEntries)
    }
}

impl std::os::fd::AsFd for Directory {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// A retained directory's iterator; it never resolves the directory's absolute name.
#[derive(Debug)]
pub struct DirectoryEntries(cap_std::fs::ReadDir);

impl Iterator for DirectoryEntries {
    type Item = std::io::Result<DirectoryEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|entry| entry.map(DirectoryEntry))
    }
}

/// One entry observed through the retained enumeration authority.
#[derive(Debug)]
pub struct DirectoryEntry(cap_std::fs::DirEntry);

impl DirectoryEntry {
    /// The native single-component name; it does not authorize an open by itself.
    pub fn file_name(&self) -> std::ffi::OsString {
        self.0.file_name()
    }

    /// Classify the entry without following a link; an observation may fail under mutation.
    pub fn file_type(&self) -> std::io::Result<EntryKind> {
        let kind = self.0.file_type()?;
        Ok(if kind.is_file() {
            EntryKind::File
        } else if kind.is_dir() {
            EntryKind::Directory
        } else if kind.is_symlink() {
            EntryKind::Link
        } else {
            EntryKind::Other
        })
    }
}

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
pub fn open_absolute_directory_no_follow(path: &Path) -> std::io::Result<Directory> {
    acquire_absolute_directory(path, false, false)
}

/// Acquire a root without links, creating missing private directory components.
/// Existing permissions are preserved; created permissions are 0700 subject to umask.
/// Invalid components fail before mutation. An I/O failure may leave created parents.
pub fn create_absolute_directory_no_follow(path: &Path) -> std::io::Result<Directory> {
    acquire_absolute_directory(path, true, false)
}

/// Create missing private directories and sync each changed namespace through its handle.
/// Existing permissions are preserved; a failed sync may leave a created prefix.
pub fn create_absolute_directory_and_sync_no_follow(path: &Path) -> std::io::Result<Directory> {
    acquire_absolute_directory(path, true, true)
}

fn acquire_absolute_directory(path: &Path, create: bool, sync: bool) -> std::io::Result<Directory> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    use std::path::Component;

    validate_absolute_root(path)?;
    let root = openat(
        rustix::fs::CWD,
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut directory = Directory(root.into());
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => {
                let next = open_relative_directory_no_follow(&directory, Path::new(component));
                directory = match next {
                    Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                        let created = match mkdirat(
                            &directory,
                            component,
                            Mode::RUSR | Mode::WUSR | Mode::XUSR,
                        ) {
                            Ok(()) => true,
                            Err(rustix::io::Errno::EXIST) => false,
                            Err(error) => return Err(error.into()),
                        };
                        let child =
                            open_relative_directory_no_follow(&directory, Path::new(component))?;
                        if sync && created {
                            child.0.sync_all()?;
                            directory.0.sync_all()?;
                        }
                        child
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
pub fn open_relative_directory_no_follow(
    directory: &Directory,
    path: &Path,
) -> std::io::Result<Directory> {
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
        current = Directory(next.into());
    }
    Ok(current)
}

/// Open a relative path read-only and nonblocking; the caller checks its file type.
pub fn open_relative_file_no_follow(directory: &Directory, path: &Path) -> std::io::Result<File> {
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

/// Creates exactly one directory under a retained parent and opens it without links.
/// An existing target is an error; no intermediate directory is created.
pub fn create_directory_no_follow(
    parent: &Directory,
    name: &std::ffi::OsStr,
) -> Result<Directory, DirectoryCreationError> {
    use std::path::Component;
    let path = Path::new(name);
    let mut components = path.components();
    if name.is_empty()
        || name.as_encoded_bytes().contains(&0)
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || path.as_os_str() != name
        || name.as_encoded_bytes().contains(&b'/')
    {
        return Err(DirectoryCreationError::NotCreated(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "single directory name required",
        )));
    }
    rustix::fs::mkdirat(
        parent,
        path,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
    )
    .map_err(|error| DirectoryCreationError::NotCreated(error.into()))?;
    open_relative_directory_no_follow(parent, path).map_err(DirectoryCreationError::Created)
}

fn single_name(path: &Path) -> std::io::Result<()> {
    if path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "operation requires one component",
        ));
    }
    Ok(())
}

/// Create one private regular staging file without following an existing name.
pub fn create_relative_private_file_no_follow(
    parent: &Directory,
    name: &Path,
) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    single_name(name)?;
    Ok(openat(
        parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )?
    .into())
}

impl Directory {
    /// Publish a staging child through two retained parents, never replacing a destination.
    pub fn link_child(
        &self,
        source_parent: &Self,
        source: &Path,
        destination: &Path,
    ) -> std::io::Result<()> {
        single_name(source)?;
        single_name(destination)?;
        rustix::fs::linkat(
            source_parent,
            source,
            self,
            destination,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(Into::into)
    }
    /// Remove one entry without traversing any component.
    pub fn remove_child(&self, name: &Path) -> std::io::Result<()> {
        single_name(name)?;
        rustix::fs::unlinkat(self, name, rustix::fs::AtFlags::empty()).map_err(Into::into)
    }
    /// Removes one empty directory without following links or reopening the parent.
    pub fn remove_empty_directory(&self, name: &Path) -> std::io::Result<()> {
        single_name(name)?;
        rustix::fs::unlinkat(self, name, rustix::fs::AtFlags::REMOVEDIR).map_err(Into::into)
    }
    /// Flush this retained directory without reopening its path.
    pub fn sync_all(&self) -> std::io::Result<()> {
        self.0.sync_all()
    }
}
