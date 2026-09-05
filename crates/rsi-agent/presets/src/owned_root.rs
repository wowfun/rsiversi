//! Unix descriptor-owned preset roots with one trusted platform-alias step.

use crate::{PresetError, Result};
use std::ffi::OsString;
use std::fs::File;
use std::path::{Component, Path, PathBuf};

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

    let components = normalized_absolute_components(path)?;
    let effective_path = resolve_first_component(&components, create, path)?;
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

fn resolve_first_component(
    components: &[OsString],
    create: bool,
    logical_path: &Path,
) -> Result<PathBuf> {
    let Some((first, suffix)) = components.split_first() else {
        return Ok(PathBuf::from("/"));
    };
    let logical_first = Path::new("/").join(first);
    let resolved_first = match std::fs::canonicalize(&logical_first) {
        Ok(path) => path,
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => logical_first,
        Err(error) => {
            return Err(PresetError::Io {
                operation: "resolve root-level alias",
                path: logical_path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };
    normalized_absolute_components(&resolved_first)?;
    Ok(suffix
        .iter()
        .fold(resolved_first, |path, name| path.join(name)))
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
