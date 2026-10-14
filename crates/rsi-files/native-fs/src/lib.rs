//! Directory-handle filesystem operations without link traversal.
//! Unsafe code is confined to the Windows native syscall module.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// Closed entry classification; special native objects remain `Other`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link, without following its target.
    Link,
    /// A special object or unclassified native entry.
    Other,
}

impl EntryKind {
    /// Whether the entry is a regular file.
    pub fn is_file(self) -> bool {
        self == Self::File
    }

    /// Whether the entry is a directory.
    pub fn is_dir(self) -> bool {
        self == Self::Directory
    }

    /// Whether the entry is a symbolic link.
    pub fn is_symlink(self) -> bool {
        self == Self::Link
    }
}

/// Whether an error happened before or after a directory was created.
#[derive(Debug)]
pub enum DirectoryCreationError {
    /// Native creation did not succeed; no new directory was created by this call.
    NotCreated(std::io::Error),
    /// Creation succeeded, but the resulting directory could not be verified safely.
    Created(std::io::Error),
}
impl std::fmt::Display for DirectoryCreationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCreated(error) => write!(formatter, "{error}"),
            Self::Created(error) => write!(
                formatter,
                "directory created but verification failed: {error}"
            ),
        }
    }
}
impl std::error::Error for DirectoryCreationError {}
