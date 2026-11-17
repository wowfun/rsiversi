use super::{
    BoundedWriter, BufWriter, File, MAXIMUM_TEMPORARY_NAME_ATTEMPTS, NEXT_TEMPORARY_SEQUENCE,
    OpenOptions, Ordering, Path, PathBuf, Read, Serialize, StorageError, Write, ensure_directory,
    fs,
};
pub(super) fn read_file_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Vec<u8>> {
    let file = open_unchanged_regular_file(path, "JSON storage document")?;
    if file.metadata()?.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("JSON storage document exceeds {maximum_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("JSON storage document exceeds {maximum_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

fn open_unchanged_regular_file(path: &Path, label: &str) -> std::io::Result<File> {
    let initial = fs::symlink_metadata(path)?;
    if !initial.file_type().is_file() {
        return Err(not_regular_file(label));
    }
    let file = open_file_no_follow(path)?;
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !opened.file_type().is_file() || !current.file_type().is_file() {
        return Err(not_regular_file(label));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if initial.dev() != opened.dev()
            || initial.ino() != opened.ino()
            || current.dev() != opened.dev()
            || current.ino() != opened.ino()
        {
            return Err(changed_file(label));
        }
    }
    #[cfg(windows)]
    {
        let opened_identity = same_file::Handle::from_file(file.try_clone()?)?;
        let current_identity = same_file::Handle::from_file(open_file_no_follow(path)?)?;
        if opened_identity != current_identity {
            return Err(changed_file(label));
        }
    }
    #[cfg(not(any(unix, windows)))]
    if initial.len() != opened.len()
        || current.len() != opened.len()
        || initial.modified().ok() != opened.modified().ok()
        || current.modified().ok() != opened.modified().ok()
    {
        return Err(changed_file(label));
    }
    Ok(file)
}

fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn not_regular_file(label: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{label} must be a regular non-symlink file"),
    )
}

fn changed_file(label: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{label} changed while opening"),
    )
}

pub(super) fn atomic_write(
    path: &Path,
    maximum_bytes: usize,
    document: &impl Serialize,
) -> Result<(), StorageError> {
    atomic_write_with_sync(path, maximum_bytes, document, |directory| {
        if let Some(directory) = directory {
            directory.sync_all()?;
        }
        Ok(())
    })
}

// The final-directory-sync seam exercises a failure after the real file replacement.
pub(super) fn atomic_write_with_sync(
    path: &Path,
    maximum_bytes: usize,
    document: &impl Serialize,
    sync: impl FnOnce(Option<File>) -> std::io::Result<()>,
) -> Result<(), StorageError> {
    let parent = path.parent().ok_or_else(|| {
        StorageError::InvalidInput("JSON storage path has no parent directory".into())
    })?;
    ensure_directory(parent)?;
    #[cfg(unix)]
    let directory = Some(File::open(parent).map_err(|error| StorageError::Io(error.to_string()))?);
    #[cfg(not(unix))]
    let directory = None;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StorageError::InvalidInput("JSON storage file name is invalid".into()))?;
    let (temporary, file) = create_temporary_file(parent, file_name)?;
    let result = (|| {
        set_file_permissions(&file)?;
        let mut writer = BoundedWriter::new(BufWriter::new(&file), maximum_bytes);
        encode_document(document, &mut writer)?;
        writer
            .flush()
            .and_then(|()| file.sync_all())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        fs::rename(&temporary, path).map_err(|error| StorageError::Io(error.to_string()))?;
        sync(directory).map_err(|error| StorageError::OutcomeUnknown(error.to_string()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn encode_document(
    document: &impl Serialize,
    writer: &mut BoundedWriter<impl Write>,
) -> Result<(), StorageError> {
    serde_json::to_writer(&mut *writer, document).map_err(|error| {
        if writer.limit_exceeded() {
            StorageError::InvalidInput(error.to_string())
        } else {
            StorageError::Io(error.to_string())
        }
    })
}

#[cfg(unix)]
pub(super) fn sync_parent_directory(parent: &Path) -> Result<(), StorageError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
pub(super) fn sync_parent_directory(_parent: &Path) -> Result<(), StorageError> {
    require_native_platform()
}

pub(super) fn create_temporary_file(
    parent: &Path,
    file_name: &str,
) -> Result<(PathBuf, File), StorageError> {
    for _ in 0..MAXIMUM_TEMPORARY_NAME_ATTEMPTS {
        let sequence = NEXT_TEMPORARY_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| StorageError::Io("JSON storage temporary sequence exhausted".into()))?;
        let path = parent.join(format!(
            ".{file_name}.{}.{sequence}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StorageError::Io(error.to_string())),
        }
    }
    Err(StorageError::Io(
        "JSON storage could not allocate a private temporary file".into(),
    ))
}

#[cfg(unix)]
fn set_file_permissions(file: &File) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| StorageError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn set_file_permissions(_file: &File) -> Result<(), StorageError> {
    require_native_platform()
}

pub(super) fn require_native_platform() -> Result<(), StorageError> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(StorageError::Unsupported(
            "JSON Storage native filesystem protection is unsupported on this platform".into(),
        ))
    }
}
