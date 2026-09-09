use super::check;
use cap_std::fs::Dir;
use rsi_files_native_fs::{
    open_absolute_directory_no_follow, open_relative_directory_no_follow,
    open_relative_file_no_follow,
};
use rsi_files_protocol::{
    DirectoryEntry, DirectoryPage, FileKind, FilePage, FilesBinding, FilesError,
    MAXIMUM_DIRECTORY_ENTRIES, MAXIMUM_DIRECTORY_NAME_BYTES, MAXIMUM_DIRECTORY_PAGE_BYTES,
    RelativePath, Result,
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{FileExt as _, MetadataExt as _},
    },
    path::Path,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Eq, PartialEq)]
struct Version {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl From<std::fs::Metadata> for Version {
    fn from(value: std::fs::Metadata) -> Self {
        Self {
            device: value.dev(),
            inode: value.ino(),
            mode: value.mode(),
            length: value.len(),
            modified: (value.mtime(), value.mtime_nsec()),
            changed: (value.ctime(), value.ctime_nsec()),
        }
    }
}
fn directory_version(directory: &Dir) -> Result<Version> {
    Ok(directory
        .try_clone()
        .map_err(io)?
        .into_std_file()
        .metadata()
        .map_err(io)?
        .into())
}
fn native(path: &RelativePath) -> &Path {
    Path::new(OsStr::from_bytes(path.as_bytes()))
}
// map_err transfers ownership of the native error at this boundary.
#[allow(clippy::needless_pass_by_value)]
fn io(error: std::io::Error) -> FilesError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FilesError::Unavailable,
        _ => FilesError::Io,
    }
}
#[derive(Debug)]
struct Entry {
    name: OsString,
    kind: Option<FileKind>,
}
#[derive(Debug)]
enum Object {
    File(File),
    Directory(Dir, Vec<Entry>),
}
#[derive(Debug)]
pub(super) struct Resource {
    root: Dir,
    path: RelativePath,
    version: Version,
    object: Object,
}
pub(super) fn open(
    binding: &FilesBinding,
    path: RelativePath,
    kind: FileKind,
    cancellation: &CancellationToken,
) -> Result<Resource> {
    check(cancellation)?;
    let root = open_absolute_directory_no_follow(binding.workspace()).map_err(io)?;
    let (object, version) = match kind {
        FileKind::File => {
            let file = open_relative_file_no_follow(&root, native(&path)).map_err(io)?;
            let metadata = file.metadata().map_err(io)?;
            if !metadata.is_file() {
                return Err(FilesError::Invalid);
            }
            (Object::File(file), Version::from(metadata))
        }
        FileKind::Directory => {
            let directory = open_relative_directory_no_follow(&root, native(&path)).map_err(io)?;
            let version = directory_version(&directory)?;
            let mut entries = Vec::new();
            let mut bytes = 0_usize;
            for entry in directory.entries().map_err(io)? {
                check(cancellation)?;
                let entry = entry.map_err(io)?;
                let name = entry.file_name();
                bytes = bytes
                    .checked_add(name.as_bytes().len())
                    .ok_or(FilesError::Capacity)?;
                if entries.len() == MAXIMUM_DIRECTORY_ENTRIES
                    || bytes > MAXIMUM_DIRECTORY_NAME_BYTES
                {
                    return Err(FilesError::Capacity);
                }
                // Validate before retaining a name that cannot be selected by this protocol.
                path.join(name.as_bytes())?;
                let kind = entry.file_type().map_err(io)?;
                let kind = if kind.is_file() {
                    Some(FileKind::File)
                } else if kind.is_dir() {
                    Some(FileKind::Directory)
                } else {
                    None
                };
                entries.push(Entry { name, kind });
            }
            entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            if version != directory_version(&directory)? {
                return Err(FilesError::Changed);
            }
            (Object::Directory(directory, entries), version)
        }
    };
    let resource = Resource {
        root,
        path,
        version,
        object,
    };
    resource.verify(cancellation)?;
    Ok(resource)
}
impl Resource {
    pub(super) fn length(&self) -> u64 {
        match &self.object {
            Object::File(_) => self.version.length,
            Object::Directory(_, entries) => entries.len() as u64,
        }
    }
    fn verify(&self, cancellation: &CancellationToken) -> Result<()> {
        check(cancellation)?;
        let (held, named) = match &self.object {
            Object::File(file) => {
                let held = file.metadata().map(Version::from).map_err(io)?;
                let named = open_relative_file_no_follow(&self.root, native(&self.path))
                    .and_then(|file| file.metadata())
                    .map(Version::from)
                    .map_err(|_| FilesError::Changed)?;
                (held, named)
            }
            Object::Directory(directory, _) => {
                let held = directory_version(directory)?;
                let named = open_relative_directory_no_follow(&self.root, native(&self.path))
                    .map_err(|_| FilesError::Changed)
                    .and_then(|directory| directory_version(&directory))?;
                (held, named)
            }
        };
        if self.version != held || self.version != named {
            return Err(FilesError::Changed);
        }
        check(cancellation)
    }
    pub(super) fn read(
        &self,
        offset: u64,
        maximum: usize,
        cancellation: &CancellationToken,
    ) -> Result<FilePage> {
        let Object::File(file) = &self.object else {
            return Err(FilesError::Invalid);
        };
        if offset > self.version.length {
            return Err(FilesError::Invalid);
        }
        self.verify(cancellation)?;
        let count = usize::try_from((self.version.length - offset).min(maximum as u64))
            .map_err(|_| FilesError::Invalid)?;
        let mut bytes = vec![0_u8; count];
        let mut done = 0;
        while done < count {
            check(cancellation)?;
            match file.read_at(&mut bytes[done..], offset + done as u64) {
                Ok(0) => return Err(FilesError::Changed),
                Ok(read) => done += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io(error)),
            }
        }
        self.verify(cancellation)?;
        Ok(FilePage {
            offset,
            total: self.version.length,
            bytes_hex: hex::encode(bytes),
        })
    }
    pub(super) fn list(
        &self,
        offset: usize,
        maximum: usize,
        cancellation: &CancellationToken,
    ) -> Result<DirectoryPage> {
        let Object::Directory(_, entries) = &self.object else {
            return Err(FilesError::Invalid);
        };
        if offset > entries.len() {
            return Err(FilesError::Invalid);
        }
        self.verify(cancellation)?;
        let mut page = Vec::new();
        let mut bytes = 256_usize;
        for entry in entries.iter().skip(offset).take(maximum) {
            check(cancellation)?;
            let path = self.path.join(entry.name.as_bytes())?;
            let name = entry.name.to_string_lossy();
            // Hex paths and worst-case JSON escaping of the lossy UTF-8 name.
            let cost = path.as_bytes().len() * 2 + name.len() * 6 + 128;
            if bytes + cost > MAXIMUM_DIRECTORY_PAGE_BYTES {
                if page.is_empty() {
                    return Err(FilesError::Capacity);
                }
                break;
            }
            bytes += cost;
            page.push(DirectoryEntry {
                name: name.into_owned(),
                path,
                kind: entry.kind,
            });
        }
        self.verify(cancellation)?;
        Ok(DirectoryPage {
            offset,
            total: entries.len(),
            entries: page,
        })
    }
}
