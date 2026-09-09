//! Unix descriptor-owned preset roots with one trusted platform-alias step.

use crate::{PresetError, Result};
use std::ffi::OsString;
use std::fs::File;
use std::path::{Component, Path};

/// An opened Unix preset root whose mutations stay relative to one directory handle.
#[derive(Debug)]
pub struct OwnedPresetRoot {
    directory: File,
}

impl OwnedPresetRoot {
    /// Consumes the root and returns its no-follow directory handle.
    pub fn into_directory(self) -> File {
        self.directory
    }
}

/// Opens an existing absolute preset root without following links below its
/// first component.
///
/// # Errors
///
/// Returns an error when the path is not normalized and absolute, the root is
/// absent, or any component below the permitted root-level alias is unsafe.
pub fn open_existing_preset_root(path: &Path) -> Result<OwnedPresetRoot> {
    open_preset_root(path, false)
}

/// Opens or creates an absolute preset root without following links below its
/// first component.
///
/// # Errors
///
/// Returns an error when the path is not normalized and absolute or a path
/// component cannot be securely opened or created.
pub fn open_or_create_preset_root(path: &Path) -> Result<OwnedPresetRoot> {
    open_preset_root(path, true)
}

fn open_preset_root(path: &Path, create: bool) -> Result<OwnedPresetRoot> {
    use rustix::fs::{Mode, OFlags};

    let effective_path =
        rsi_files_native_fs::resolve_absolute_root_alias(path, create).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidInput {
                invalid_root(path)
            } else {
                PresetError::Io {
                    operation: "resolve root-level alias",
                    path: path.to_path_buf(),
                    message: error.to_string(),
                }
            }
        })?;
    let effective_components = normalized_absolute_components(&effective_path)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = rustix::fs::open("/", flags, Mode::empty())
        .map(File::from)
        .map_err(|error| unsafe_root(path, "filesystem root is unavailable", error))?;

    for name in effective_components {
        let (opened, created) = match rustix::fs::openat(&directory, &name, flags, Mode::empty()) {
            Ok(opened) => (opened, false),
            Err(rustix::io::Errno::NOENT) if create => {
                let created = match rustix::fs::mkdirat(
                    &directory,
                    &name,
                    Mode::RUSR | Mode::WUSR | Mode::XUSR,
                ) {
                    Ok(()) => true,
                    Err(rustix::io::Errno::EXIST) => false,
                    Err(error) => {
                        return Err(io_root("create root directory", path, error));
                    }
                };
                let opened = rustix::fs::openat(&directory, &name, flags, Mode::empty()).map_err(
                    |error| unsafe_root(path, "root component is not a no-follow directory", error),
                )?;
                (opened, created)
            }
            Err(error) => {
                return Err(unsafe_root(
                    path,
                    "root component is not a no-follow directory",
                    error,
                ));
            }
        };
        directory = File::from(opened);
        if created {
            rustix::fs::fchmod(&directory, Mode::RUSR | Mode::WUSR | Mode::XUSR)
                .map_err(|error| io_root("set root directory mode", path, error))?;
        }
    }

    Ok(OwnedPresetRoot { directory })
}

fn normalized_absolute_components(path: &Path) -> Result<Vec<OsString>> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(invalid_root(path));
    }
    let mut saw_root = false;
    let mut output = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir if !saw_root => saw_root = true,
            Component::Normal(name) if saw_root => output.push(name.to_os_string()),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => {
                return Err(invalid_root(path));
            }
            Component::Normal(_) => return Err(invalid_root(path)),
        }
    }
    if !saw_root {
        return Err(invalid_root(path));
    }
    Ok(output)
}

fn invalid_root(path: &Path) -> PresetError {
    PresetError::InvalidRoot(format!(
        "root is not a normalized absolute path: {}",
        path.display()
    ))
}

fn unsafe_root(path: &Path, reason: &str, error: rustix::io::Errno) -> PresetError {
    PresetError::UnsafeEntry {
        path: path.to_path_buf(),
        reason: format!("{reason}: {error}"),
    }
}

fn io_root(operation: &'static str, path: &Path, error: rustix::io::Errno) -> PresetError {
    PresetError::Io {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
