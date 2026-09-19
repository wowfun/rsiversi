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
    reset_and_open_progress(root, open, |_| {})
}

fn reset_and_open_progress(
    root: &Path,
    open: impl FnOnce(&Path) -> Result<SqliteStore>,
    progress: impl FnOnce(&SqliteStoreResetReceipt),
) -> std::result::Result<(SqliteStore, SqliteStoreResetReceipt), SqliteStoreResetError> {
    let mut receipt = SqliteStoreResetReceipt {
        root: root.to_owned(),
        backup: None,
    };
    let result = (|| {
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
            progress(&receipt);
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

/// Explicit one-shot startup authority. No serialized configuration can create it.
#[derive(Clone, Debug)]
pub struct SqliteStoreResetRequest(Arc<Mutex<ResetState>>, Arc<ResetReceipt>);

#[derive(Debug, Default)]
struct ResetReceipt {
    // Publication is one-shot even after the launcher consumes the receipt.
    state: Mutex<(bool, Option<SqliteStoreResetReceipt>)>,
    ready: tokio::sync::Notify,
}
impl ResetReceipt {
    fn publish(&self, receipt: SqliteStoreResetReceipt) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.0 {
            *state = (true, Some(receipt));
            self.ready.notify_waiters();
        }
    }
}

#[derive(Debug, Default)]
struct ResetState {
    consumed: bool,
    root: Option<PathBuf>,
    failure: Option<StoreError>,
}

impl Default for SqliteStoreResetRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl SqliteStoreResetRequest {
    /// Authorizes one reset during initial activation.
    pub fn new() -> Self {
        Self(
            Arc::new(Mutex::new(ResetState::default())),
            Arc::new(ResetReceipt::default()),
        )
    }

    /// Whether the initial reset has yet to begin.
    /// A consumed failure is not pending; subsequent opens still return that failure.
    pub fn is_pending(&self) -> bool {
        !self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .consumed
    }

    /// Takes filesystem progress for the launcher's diagnostic, including on failure.
    pub fn take_receipt(&self) -> Option<SqliteStoreResetReceipt> {
        self.1
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .take()
    }

    /// Waits for backup progress without waiting for replacement Store initialization.
    /// Without an old Store, publication instead follows the initialization outcome.
    /// The single launcher consumer retrieves it with `take_receipt`.
    pub async fn receipt_ready(&self) {
        loop {
            let ready = self.1.ready.notified();
            if self
                .1
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0
            {
                return;
            }
            ready.await;
        }
    }

    pub(super) fn bind(&self, root: &Path) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.consumed {
            if state.root.as_deref().is_some_and(|bound| bound != root) {
                return Err(StoreError::Invalid(
                    "reset requires exactly one SQLite Agent Store root".into(),
                ));
            }
            state.root = Some(root.to_owned());
        }
        Ok(())
    }

    pub(super) fn open(&self, root: &Path) -> Result<SqliteStore> {
        self.open_with(root, |root| {
            reset_and_open_progress(
                root,
                |root| SqliteStore::open(root),
                |receipt| self.1.publish(receipt.clone()),
            )
        })
    }

    fn open_with(
        &self,
        root: &Path,
        reset: impl FnOnce(
            &Path,
        ) -> std::result::Result<
            (SqliteStore, SqliteStoreResetReceipt),
            SqliteStoreResetError,
        >,
    ) -> Result<SqliteStore> {
        self.bind(root)?;
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.consumed {
            drop(state);
            return SqliteStore::open(root);
        }
        state.consumed = true;
        // Only success clears this latch. Unwinding must not authorize an
        // ordinary open through the recovered poisoned mutex.
        state.failure = Some(StoreError::Io("Agent Store reset did not complete".into()));
        // Publish failures before the move too; moved-root receipts are already visible.
        match reset(root) {
            Ok((store, receipt)) => {
                self.1.publish(receipt);
                state.failure = None;
                Ok(store)
            }
            Err(error) => {
                self.1.publish(error.receipt);
                state.failure = Some(error.source.clone());
                Err(error.source)
            }
        }
    }
}

#[cfg(test)]
mod tests;
