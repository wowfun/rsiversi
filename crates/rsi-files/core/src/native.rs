use super::check;
use rsi_files_native_fs::{
    Directory as Dir, open_absolute_directory_no_follow, open_relative_directory_no_follow,
    open_relative_file_no_follow,
};
use rsi_files_protocol::{
    DirectoryEntry, DirectoryPage, FileKind, FilePage, FilesBinding, FilesError,
    MAXIMUM_DIRECTORY_ENTRIES, MAXIMUM_DIRECTORY_NAME_BYTES, MAXIMUM_DIRECTORY_PAGE_BYTES,
    RelativePath, Result,
};
use rsi_files_protocol::{FileToken, OpenedFile};
#[cfg(unix)]
use std::os::unix::{
    ffi::{OsStrExt as _, OsStringExt as _},
    fs::{FileExt as _, MetadataExt as _},
};
#[cfg(windows)]
use std::os::windows::fs::FileExt as _;
use std::{fs::File, path::PathBuf};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
type Version = rsi_files_native_fs::FileVersion;
#[cfg(unix)]
#[derive(Debug, Eq, PartialEq)]
struct Version {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
#[cfg(unix)]
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
    #[cfg(unix)]
    {
        Ok(directory.dir_metadata().map_err(io)?.into())
    }
    #[cfg(windows)]
    {
        directory.version().map_err(io)
    }
}
fn file_version(file: &File) -> Result<Version> {
    #[cfg(unix)]
    {
        Ok(file.metadata().map_err(io)?.into())
    }
    #[cfg(windows)]
    {
        rsi_files_native_fs::file_version(file).map_err(io)
    }
}
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))] // The shared Windows boundary is fallible.
fn native(path: &RelativePath) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        Ok(PathBuf::from(std::ffi::OsString::from_vec(
            path.as_bytes().to_vec(),
        )))
    }
    #[cfg(windows)]
    {
        let text = std::str::from_utf8(path.as_bytes()).map_err(|_| FilesError::Invalid)?;
        if text.contains(['\\', ':']) {
            return Err(FilesError::Invalid);
        }
        Ok(PathBuf::from(text))
    }
}
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))] // Windows names may not be UTF-8.
fn name_bytes(name: &std::ffi::OsStr) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        Ok(name.as_bytes().to_vec())
    }
    #[cfg(windows)]
    {
        Ok(name
            .to_str()
            .ok_or(FilesError::Invalid)?
            .as_bytes()
            .to_vec())
    }
}
fn executable(version: &Version) -> bool {
    #[cfg(unix)]
    {
        version.mode & 0o111 != 0
    }
    #[cfg(windows)]
    {
        let _ = version;
        false
    }
}
// map_err transfers ownership of the native error at this boundary.
#[allow(clippy::needless_pass_by_value)]
fn io(error: std::io::Error) -> FilesError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FilesError::Unavailable,
        _ => FilesError::Io,
    }
}
#[allow(clippy::needless_pass_by_value)]
fn relative_open_error(error: std::io::Error) -> FilesError {
    if error.kind() == std::io::ErrorKind::NotFound {
        FilesError::Missing
    } else {
        io(error)
    }
}
#[derive(Debug)]
struct Entry {
    name: Vec<u8>,
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
            let file = open_relative_file_no_follow(&root, &native(&path)?)
                .map_err(relative_open_error)?;
            let metadata = file.metadata().map_err(io)?;
            if !metadata.is_file() {
                return Err(FilesError::Invalid);
            }
            let version = file_version(&file)?;
            (Object::File(file), version)
        }
        FileKind::Directory => {
            let directory = open_relative_directory_no_follow(&root, &native(&path)?)
                .map_err(relative_open_error)?;
            let version = directory_version(&directory)?;
            let mut entries = Vec::new();
            let mut bytes = 0_usize;
            for entry in directory.entries().map_err(io)? {
                check(cancellation)?;
                let entry = entry.map_err(io)?;
                let name = name_bytes(&entry.file_name())?;
                bytes = bytes.checked_add(name.len()).ok_or(FilesError::Capacity)?;
                if entries.len() == MAXIMUM_DIRECTORY_ENTRIES
                    || bytes > MAXIMUM_DIRECTORY_NAME_BYTES
                {
                    return Err(FilesError::Capacity);
                }
                // Validate before retaining a name that cannot be selected by this protocol.
                path.join(&name)?;
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
            entries.sort_by(|a, b| a.name.cmp(&b.name));
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
    pub(super) fn describe(&self, token: FileToken) -> OpenedFile {
        OpenedFile {
            path: self.path.clone(),
            token,
            kind: match self.object {
                Object::File(_) => FileKind::File,
                Object::Directory(..) => FileKind::Directory,
            },
            length: self.length(),
            executable: matches!(self.object, Object::File(_)) && executable(&self.version),
        }
    }
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
                let held = file_version(file)?;
                let named = open_relative_file_no_follow(&self.root, &native(&self.path)?)
                    .map_err(|_| FilesError::Changed)
                    .and_then(|file| file_version(&file))
                    .map_err(|_| FilesError::Changed)?;
                (held, named)
            }
            Object::Directory(directory, _) => {
                let held = directory_version(directory)?;
                let named = open_relative_directory_no_follow(&self.root, &native(&self.path)?)
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
            #[cfg(unix)]
            let read = file.read_at(&mut bytes[done..], offset + done as u64);
            #[cfg(windows)]
            let read = file.seek_read(&mut bytes[done..], offset + done as u64);
            match read {
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
            let path = self.path.join(&entry.name)?;
            let name = String::from_utf8_lossy(&entry.name);
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
