use super::*;

pub(super) fn prepare_root(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(StoreError::Invalid(
            "SQLite Agent Store root must be an absolute path".into(),
        ));
    }
    reject_symlink_if_present(path, "Store root")?;
    create_private_directories(path)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::Invalid(
            "SQLite Agent Store root must be a real directory".into(),
        ));
    }
    set_directory_permissions(path)?;
    fs::canonicalize(path).map_err(io_error)
}

pub(super) fn existing_root(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(StoreError::Invalid(
            "SQLite Agent Store root must be an absolute path".into(),
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StoreError::NotFound(path.display().to_string())
        } else {
            io_error(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(StoreError::Invalid(
            "SQLite Agent Store root must be a real directory".into(),
        ));
    }
    fs::canonicalize(path).map_err(io_error)
}

pub(super) fn prepare_owned_directory(path: &Path, label: &str) -> Result<()> {
    reject_symlink_if_present(path, label)?;
    create_private_directories(path)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::Corrupt(format!(
            "{label} is not a real directory"
        )));
    }
    set_directory_permissions(path)?;
    Ok(())
}

pub(super) fn prepare_cas_staging_directory(path: &Path) -> Result<()> {
    prepare_owned_directory(path, "CAS staging directory")?;
    for (index, entry) in fs::read_dir(path).map_err(io_error)?.enumerate() {
        if index >= MAXIMUM_ORPHANED_CAS_STAGING_FILES {
            return Err(StoreError::Corrupt(format!(
                "CAS staging directory exceeds {MAXIMUM_ORPHANED_CAS_STAGING_FILES} orphaned files"
            )));
        }
        let entry = entry.map_err(io_error)?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(StoreError::Corrupt(
                "CAS staging entry is not a real regular file".into(),
            ));
        }
        fs::remove_file(entry.path()).map_err(io_error)?;
    }
    sync_directory(path)
}

#[cfg(unix)]
pub(super) fn create_private_directories(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).map_err(io_error)
}

#[cfg(not(unix))]
pub(super) fn create_private_directories(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io_error)
}

#[cfg(unix)]
pub(super) fn set_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

#[cfg(not(unix))]
pub(super) fn set_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn reject_symlink_if_present(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::Invalid(format!(
            "{label} must not be a symbolic link"
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

pub(super) fn reject_uncheckpointed_wal(root: &Path) -> Result<()> {
    let wal_path = root.join("sessions.sqlite3-wal");
    let metadata = match fs::symlink_metadata(&wal_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(StoreError::Corrupt(
            "SQLite WAL is not a regular file".into(),
        ));
    }
    if metadata.len() != 0 {
        return Err(StoreError::Invalid(
            "verification requires a cleanly closed Store or SQLite backup; found a nonempty WAL"
                .into(),
        ));
    }
    Ok(())
}

pub(super) fn acquire_writer_lock(root: &Path) -> Result<File> {
    open_writer_lock(root, true)
}

pub(super) fn acquire_existing_writer_lock(root: &Path) -> Result<File> {
    open_writer_lock(root, false)
}

pub(super) fn open_writer_lock(root: &Path, create: bool) -> Result<File> {
    let path = root.join(".writer.lock");
    reject_symlink_if_present(&path, "Store writer lock")?;
    let file = OpenOptions::new()
        .create(create)
        .truncate(false)
        .read(true)
        .write(create)
        .open(&path)
        .map_err(|error| {
            if !create && error.kind() == std::io::ErrorKind::NotFound {
                StoreError::NotFound(path.display().to_string())
            } else {
                io_error(error)
            }
        })?;
    validate_open_file(&path, &file, "Store writer lock")?;
    if let Err(error) = file.try_lock() {
        let error: std::io::Error = error.into();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(StoreError::WriterLocked);
        }
        return Err(io_error(error));
    }
    validate_open_file(&path, &file, "Store writer lock")?;
    Ok(file)
}

pub(super) fn validate_open_file(path: &Path, file: &File, label: &str) -> Result<()> {
    let path_metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let file_metadata = file.metadata().map_err(io_error)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !file_metadata.file_type().is_file()
    {
        return Err(StoreError::Corrupt(format!(
            "{label} is not a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(StoreError::Corrupt(format!(
                "{label} changed while opening"
            )));
        }
    }
    Ok(())
}

pub(super) fn configure_writer(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql_error)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;",
        )
        .map_err(sql_error)
}

pub(super) fn configure_reader(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sql_error)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA query_only = ON;",
        )
        .map_err(sql_error)
}

pub(super) fn open_verification_database(path: &Path) -> Result<Connection> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let path_text = path
        .to_str()
        .ok_or_else(|| StoreError::Invalid("SQLite database path is not Unicode".into()))?;
    #[cfg(not(unix))]
    let bytes = path_text.as_bytes();
    let uri = immutable_sqlite_uri(bytes);
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql_error)
}

pub(super) fn immutable_sqlite_uri(bytes: &[u8]) -> String {
    let mut uri = String::with_capacity(bytes.len().saturating_mul(3).saturating_add(17));
    uri.push_str("file:");
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b':' | b'-' | b'.' | b'_' | b'~')
        {
            uri.push(char::from(*byte));
        } else {
            write!(&mut uri, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    uri.push_str("?immutable=1");
    uri
}
