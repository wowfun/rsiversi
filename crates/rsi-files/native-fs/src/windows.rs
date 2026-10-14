//! Windows directory capabilities; policy and durable paths stay with callers.
use crate::{DirectoryCreationError, EntryKind};
use std::{
    ffi::OsString,
    fs::File,
    io,
    path::{Path, PathBuf},
};

mod names;
#[allow(unsafe_code)]
mod native;

/// An owned no-follow directory capability, without path-based directory methods.
#[derive(Debug)]
pub struct Directory(File);

/// Native namespace pin; protects root identity and a permanent nonempty guard.
#[derive(Debug)]
pub struct NamespaceLease(std::sync::Arc<NamespacePin>);
#[derive(Debug)]
struct NamespacePin {
    directory: File,
    _anchor: File,
}
static NAMESPACE_PINS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<[u8; 24], std::sync::Weak<NamespacePin>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
impl NamespaceLease {
    /// Publish an admitted private child through this retained writable namespace.
    pub fn publish_child(
        &self,
        source: &Path,
        destination: &Path,
        kind: EntryKind,
        replace: bool,
    ) -> io::Result<()> {
        let source = names::relative(source)?;
        let destination = names::relative(destination)?;
        if source.len() != 1 || destination.len() != 1 {
            return Err(names::invalid(
                "publication requires two single component names",
            ));
        }
        let directory = match kind {
            EntryKind::File => false,
            EntryKind::Directory => true,
            _ => {
                return Err(names::invalid(
                    "publication requires a regular file or directory",
                ));
            }
        };
        native::rename_child(
            &self.0.directory,
            &source[0],
            &destination[0],
            directory,
            replace,
        )
    }
    /// Hard-link a private staging file into this namespace without replacement.
    pub fn link_child(
        &self,
        source_parent: &Directory,
        source: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        let source = single_name(source)?;
        let destination = single_name(destination)?;
        native::link_child(&self.0.directory, &source_parent.0, &source, &destination)
    }
    /// Remove a single regular child through the retained namespace.
    pub fn remove_child(&self, name: &Path) -> io::Result<()> {
        native::remove_child(&self.0.directory, &single_name(name)?)
    }
    /// Flush this exact pinned writable directory without reopening a pathname.
    pub fn sync_all(&self) -> io::Result<()> {
        self.0.directory.sync_all()
    }
}

fn single_name(path: &Path) -> io::Result<Vec<u16>> {
    let mut names = names::relative(path)?;
    if names.len() != 1 {
        return Err(names::invalid("operation requires one component"));
    }
    Ok(names.remove(0))
}

/// Version observed on the retained handle; times are native 100 ns units.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileVersion {
    /// Volume serial and 128-bit `FileID`, without reopening a path.
    pub identity: [u8; 24],
    /// Native file attributes.
    pub attributes: u32,
    /// End-of-file length.
    pub length: u64,
    /// Native count of hard links observed on the retained handle.
    pub link_count: u32,
    /// Last-write time.
    pub modified: i64,
    /// Metadata change time.
    pub changed: i64,
}

/// Capture identity and version directly from an already owned native file.
pub fn file_version(file: &File) -> io::Result<FileVersion> {
    native::version(file)
}

impl Directory {
    /// Verify the retained directory's exact `TokenUser` private policy without mutation.
    pub fn verify_private(&self) -> io::Result<()> {
        native::verify_private_directory(&self.0)
    }
    /// Pin this same directory's namespace for path-based native consumers.
    pub fn pin_namespace(&self) -> io::Result<NamespaceLease> {
        native::verify_private_directory(&self.0)?;
        let identity = self.version()?.identity;
        let mut pins = NAMESPACE_PINS
            .lock()
            .map_err(|_| io::Error::other("namespace pin registry is poisoned"))?;
        pins.retain(|_, pin| pin.strong_count() != 0);
        if let Some(file) = pins.get(&identity).and_then(std::sync::Weak::upgrade) {
            return Ok(NamespaceLease(file));
        }
        if pins.len() == 4096 {
            return Err(io::Error::other("namespace pin capacity exhausted"));
        }
        drop(pins);
        let name: Vec<u16> = ".rsi-namespace-pin".encode_utf16().collect();
        let anchor = match native::private_file(&self.0, &name, true) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                native::private_file(&self.0, &name, false)?
            }
            Err(e) => return Err(e),
        };
        if anchor.metadata()?.len() != 0 {
            return Err(io::Error::other(
                "namespace guard must be an empty private regular file",
            ));
        }
        let file = std::sync::Arc::new(NamespacePin {
            directory: native::pin_namespace(&self.0)?,
            _anchor: anchor,
        });
        let mut pins = NAMESPACE_PINS
            .lock()
            .map_err(|_| io::Error::other("namespace pin registry is poisoned"))?;
        pins.retain(|_, pin| pin.strong_count() != 0);
        if let Some(existing) = pins.get(&identity).and_then(std::sync::Weak::upgrade) {
            drop(pins);
            return Ok(NamespaceLease(existing));
        }
        if pins.len() == 4096 {
            drop(pins);
            return Err(io::Error::other("namespace pin capacity exhausted"));
        }
        pins.insert(identity, std::sync::Arc::downgrade(&file));
        Ok(NamespaceLease(file))
    }
    /// Clone the same native authority without reopening its path.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    /// Inspect the retained directory, without looking up its absolute name.
    pub fn dir_metadata(&self) -> io::Result<std::fs::Metadata> {
        self.0.metadata()
    }

    /// Observe the retained directory's native identity and metadata version.
    pub fn version(&self) -> io::Result<FileVersion> {
        native::version(&self.0)
    }

    /// Flush the exact directory through a writable relative reopen, never a path.
    pub fn sync_all(&self) -> io::Result<()> {
        let identity = self.version()?.identity;
        let pinned = NAMESPACE_PINS
            .lock()
            .map_err(|_| io::Error::other("namespace pin registry is poisoned"))?
            .get(&identity)
            .and_then(std::sync::Weak::upgrade);
        if let Some(file) = pinned {
            native::verify_private_directory(&self.0)?;
            return file.directory.sync_all();
        }
        let writable = native::writable_relative(&self.0, &[], true)?;
        if native::identity(&self.0)? != native::identity(&writable)? {
            return Err(io::Error::other(
                "directory changed while reopening for synchronization",
            ));
        }
        writable.sync_all()
    }

    /// Enumerate the retained directory with an independent native cursor.
    pub fn entries(&self) -> io::Result<DirectoryEntries> {
        let file = native::relative(&self.0, &[], true)?;
        if native::identity(&self.0)? != native::identity(&file)? {
            return Err(io::Error::other("enumeration directory identity changed"));
        }
        Ok(DirectoryEntries {
            file,
            buffer: native::EnumerationBuffer::new(),
            ended: false,
        })
    }
}

/// A retained directory's independent iterator; callers bound and cancel iteration.
#[derive(Debug)]
pub struct DirectoryEntries {
    file: File,
    buffer: native::EnumerationBuffer,
    ended: bool,
}

impl Iterator for DirectoryEntries {
    type Item = io::Result<DirectoryEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match native::next_entry(&self.file, &mut self.buffer) {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => {
                self.ended = true;
                None
            }
            Err(error) => {
                self.ended = true;
                Some(Err(error))
            }
        }
    }
}

/// One name and kind observed through the retained enumeration authority.
#[derive(Debug)]
pub struct DirectoryEntry {
    name: OsString,
    kind: EntryKind,
}

impl DirectoryEntry {
    /// Native single-component name; it does not authorize an open by itself.
    pub fn file_name(&self) -> OsString {
        self.name.clone()
    }

    /// Return the kind captured during enumeration without further I/O or failure.
    /// The Result shape matches platforms that can fail while observing its type.
    pub fn file_type(&self) -> io::Result<EntryKind> {
        Ok(self.kind)
    }
}

/// Validate the local Windows root spelling without granting directory authority.
/// No aliases or existence checks apply here, including when `allow_missing` is false.
pub fn resolve_absolute_root_alias(path: &Path, _allow_missing: bool) -> io::Result<PathBuf> {
    names::absolute(path)?;
    Ok(path.to_owned())
}

/// Validate the native absolute path grammar without I/O or directory authority.
pub fn validate_absolute_path(path: &Path) -> io::Result<()> {
    names::absolute(path).map(|_| ())
}

/// Acquire a directory without following any reparse component.
pub fn open_absolute_directory_no_follow(path: &Path) -> io::Result<Directory> {
    let (drive, components) = names::absolute(path)?;
    let mut directory = Directory(native::root(drive)?);
    for name in components {
        directory = Directory(native::relative(&directory.0, &name, true)?);
    }
    Ok(directory)
}

/// Descend from an owned directory; an empty path clones the native handle.
pub fn open_relative_directory_no_follow(parent: &Directory, path: &Path) -> io::Result<Directory> {
    let components = names::relative(path)?;
    let mut directory = parent.try_clone()?;
    for name in components {
        directory = Directory(native::relative(&directory.0, &name, true)?);
    }
    Ok(directory)
}

/// Open an exact relative regular-file name read-only, without reparse traversal.
pub fn open_relative_file_no_follow(parent: &Directory, path: &Path) -> io::Result<File> {
    let mut components = names::relative(path)?;
    let name = components
        .pop()
        .ok_or_else(|| names::invalid("file path has no name"))?;
    let mut directory = parent.try_clone()?;
    for component in components {
        directory = Directory(native::relative(&directory.0, &component, true)?);
    }
    native::relative(&directory.0, &name, false)
}

fn file_parent(parent: &Directory, path: &Path) -> io::Result<(Directory, Vec<u16>)> {
    let mut components = names::relative(path)?;
    let name = components
        .pop()
        .ok_or_else(|| names::invalid("file path has no name"))?;
    let mut directory = parent.try_clone()?;
    for component in components {
        directory = Directory(native::relative(&directory.0, &component, true)?);
    }
    Ok((directory, name))
}

/// Open a relative file read/write without following any reparse component.
pub fn open_relative_file_read_write_no_follow(
    parent: &Directory,
    path: &Path,
) -> io::Result<File> {
    let (parent, name) = file_parent(parent, path)?;
    native::writable_relative(&parent.0, &name, false)
}

/// Create one private file atomically; failure may leave a created but unverified name.
pub fn create_relative_private_file_no_follow(parent: &Directory, path: &Path) -> io::Result<File> {
    let (parent, name) = file_parent(parent, path)?;
    native::private_file(&parent.0, &name, true)
}

/// Open an existing read/write file only when its exact protected private policy matches.
pub fn open_relative_private_file_no_follow(parent: &Directory, path: &Path) -> io::Result<File> {
    let (parent, name) = file_parent(parent, path)?;
    native::private_file(&parent.0, &name, false)
}

/// Validate the private owner/DACL on an already opened file with `READ_CONTROL`.
pub fn verify_private_file(file: &File) -> io::Result<()> {
    native::verify_private(file)
}

/// Replace only a TokenUser-owned regular file's DACL after no-follow acquisition.
pub fn set_relative_private_file_permissions_no_follow(
    parent: &Directory,
    path: &Path,
) -> io::Result<File> {
    let (parent, name) = file_parent(parent, path)?;
    native::privatize_file(&parent.0, &name)
}

/// Privatize only the selected TokenUser-owned directory, through retained parents.
pub fn set_private_directory_permissions_no_follow(path: &Path) -> io::Result<Directory> {
    let (drive, mut components) = names::absolute(path)?;
    let name = components
        .pop()
        .ok_or_else(|| names::invalid("cannot privatize a drive root"))?;
    let mut parent = Directory(native::root(drive)?);
    for component in components {
        parent = Directory(native::relative(&parent.0, &component, true)?);
    }
    native::privatize_directory(&parent.0, &name).map(Directory)
}

/// Flush an admitted no-follow local directory and report native failure.
pub fn sync_absolute_directory_no_follow(path: &Path) -> io::Result<()> {
    open_absolute_directory_no_follow(path)?.sync_all()
}

/// Whether a component open encountered a reparse point or a non-directory parent.
pub fn is_link_rejection(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(native::ERROR_REPARSE_POINT_ENCOUNTERED | native::ERROR_DIRECTORY)
    )
}

/// Acquire a no-follow root, creating only missing components with private descriptors.
/// Existing permissions are preserved. Invalid spelling fails before mutation;
/// an I/O failure can leave newly created parents for the caller to inspect.
pub fn create_absolute_directory_no_follow(path: &Path) -> io::Result<Directory> {
    create_absolute_directory(path, false)
}

/// Create missing private directories and flush each changed namespace through its handle.
/// Existing higher ancestors need no write access; a failed sync may leave a created prefix.
pub fn create_absolute_directory_and_sync_no_follow(path: &Path) -> io::Result<Directory> {
    create_absolute_directory(path, true)
}

fn create_absolute_directory(path: &Path, sync: bool) -> io::Result<Directory> {
    let (drive, components) = names::absolute(path)?;
    let mut directory = Directory(native::root(drive)?);
    for name in components {
        directory = match native::relative(&directory.0, &name, true) {
            Ok(file) => Directory(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (file, created) = create_or_open_directory(&directory, &name)?;
                let child = Directory(file);
                if sync && created {
                    child.sync_all()?;
                    directory.sync_all()?;
                }
                child
            }
            Err(error) => return Err(error),
        };
    }
    Ok(directory)
}

fn create_or_open_directory(parent: &Directory, name: &[u16]) -> io::Result<(File, bool)> {
    match native::create_directory(&parent.0, name) {
        Ok(file) => Ok((file, true)),
        Err(DirectoryCreationError::NotCreated(error))
            if error.kind() == io::ErrorKind::AlreadyExists =>
        {
            // FILE_CREATE created no object. Acquire the current existing name
            // from the retained parent; this confers no private-creation receipt.
            native::relative(&parent.0, name, true).map(|file| (file, false))
        }
        Err(DirectoryCreationError::NotCreated(error) | DirectoryCreationError::Created(error)) => {
            Err(error)
        }
    }
}

/// Create one new private directory below the retained parent, without following links.
/// Existing targets are refused, and created-but-unverified errors remain distinct.
pub fn create_directory_no_follow(
    parent: &Directory,
    name: &std::ffi::OsStr,
) -> Result<Directory, DirectoryCreationError> {
    use std::os::windows::ffi::OsStrExt as _;
    // Capture one unit beyond the limit to reject overlong input without a full scan.
    let units: Vec<_> = name
        .encode_wide()
        .take(names::MAX_COMPONENT_UNITS + 1)
        .collect();
    names::validate_component(&units).map_err(DirectoryCreationError::NotCreated)?;
    native::create_directory(&parent.0, &units).map(Directory)
}

#[cfg(test)]
mod tests;
