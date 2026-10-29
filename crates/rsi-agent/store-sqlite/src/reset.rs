use super::*;

/// Filesystem outcome of an explicit reset; also retained if later opening fails.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SqliteStoreResetReceipt {
    /// Original configured Store root.
    pub root: PathBuf,
    /// Complete old root inside a private sibling container, if one existed.
    pub backup: Option<PathBuf>,
}

/// Reset failure together with the location of any already preserved Store.
#[derive(Debug)]
pub struct SqliteStoreResetError {
    /// Underlying failure, without interpreting the old database.
    pub source: StoreError,
    /// Reset progress; an existing backup is never rolled back or deleted.
    pub receipt: SqliteStoreResetReceipt,
}

impl std::fmt::Display for SqliteStoreResetError {
    #[allow(
        clippy::unnecessary_debug_formatting,
        reason = "Escape filesystem paths in terminal diagnostics."
    )]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; reset root {:?}; backup {:?}",
            self.source, self.receipt.root, self.receipt.backup
        )
    }
}

impl std::error::Error for SqliteStoreResetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl SqliteStore {
    /// Archives an existing root under its writer lease and opens a fresh Store.
    /// The old database is never opened; a missing root initializes normally.
    pub fn reset_and_open(
        root: impl AsRef<Path>,
    ) -> std::result::Result<(Self, SqliteStoreResetReceipt), SqliteStoreResetError> {
        reset_and_open_with(root.as_ref(), |root| Self::open(root))
    }
}

fn reset_and_open_with(
    root: &Path,
    open: impl FnOnce(&Path) -> Result<SqliteStore>,
) -> std::result::Result<(SqliteStore, SqliteStoreResetReceipt), SqliteStoreResetError> {
    let mut receipt = SqliteStoreResetReceipt {
        root: root.to_owned(),
        backup: None,
    };
    let result = (|| {
        let _ = filesystem::normalized_root_path(root)?;
        if cfg!(windows) {
            return Err(StoreError::Unsupported(
                "Agent Store archival reset requires Unix rename-under-lease semantics".into(),
            ));
        }
        let existing = match filesystem::existing_root(root) {
            Ok(root) => {
                if is_store_reset_target(&root)? {
                    Some(root)
                } else {
                    None
                }
            }
            Err(StoreError::NotFound(_)) => None,
            Err(error) => return Err(error),
        };
        // Retain the moved lease until the new Store has acquired its own lease.
        let _old_lease = if let Some(existing) = existing {
            let parent = existing
                .parent()
                .ok_or_else(|| StoreError::Invalid("cannot reset a filesystem root".into()))?;
            let name = existing
                .file_name()
                .ok_or_else(|| StoreError::Invalid("Store root has no directory name".into()))?;
            let lease = acquire_writer_lock(&existing)?;
            if !is_store_reset_target(&existing)? {
                return Err(StoreError::Invalid(
                    "Store root changed before reset".into(),
                ));
            }
            let mut prefix = name.to_os_string();
            prefix.push(".backup-");
            let container = tempfile::Builder::new()
                .prefix(&prefix)
                .tempdir_in(parent)
                .map_err(io_error)?
                .keep();
            let backup = container.join("store");
            if let Err(error) = fs::rename(&existing, &backup) {
                let _ = fs::remove_dir(&container);
                return Err(io_error(error));
            }
            receipt.backup = Some(backup);
            sync_directory(&container)?;
            sync_directory(parent)?;
            Some(lease)
        } else {
            None
        };
        open(root)
    })();
    match result {
        Ok(store) => Ok((store, receipt)),
        Err(source) => Err(SqliteStoreResetError { source, receipt }),
    }
}

// Layout recognition is intentionally independent of SQLite decoding or schema.
// Check before creating a lock so a mistaken root remains completely untouched.
fn is_store_reset_target(root: &Path) -> Result<bool> {
    if fs::read_dir(root)
        .map_err(io_error)?
        .next()
        .transpose()
        .map_err(io_error)?
        .is_none()
    {
        return Ok(false);
    }
    for (name, directory) in [("sessions.sqlite3", false), ("cas", true)] {
        match fs::symlink_metadata(root.join(name)) {
            Ok(metadata)
                if if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                } =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(StoreError::Invalid(
        "refusing to reset a nonempty directory without Agent Store layout markers".into(),
    ))
}

#[cfg(test)]
mod tests;
