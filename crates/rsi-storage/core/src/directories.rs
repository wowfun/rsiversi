//! Publication durability for backend directory chains.
use crate::{Result, StorageError};
#[cfg(not(windows))]
use std::fs;
use std::path::Path;

/// Creates private path components and durably publishes their ancestor entries.
/// Unix existing ancestors are synced too: an earlier interrupted attempt may have
/// created them without publishing their names durably.
/// Windows flushes newly created nodes and the retained parents whose entries changed.
pub fn create_private_directories(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(path)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder
            .create(path)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        #[cfg(unix)]
        rsi_files_native_fs::sync_directory_ancestors(path)
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(())
    }
}
