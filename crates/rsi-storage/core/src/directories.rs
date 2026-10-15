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
        sync_ancestors(path, |ancestor| fs::File::open(ancestor)?.sync_all())
            .map_err(|error| StorageError::Io(error.to_string()))?;
        Ok(())
    }
}
#[cfg(unix)]
fn sync_ancestors(
    path: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for ancestor in path.ancestors() {
        sync(if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        })?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn every_parent_entry_is_synced_and_failure_prevents_success() {
        let mut observed = Vec::new();
        sync_ancestors(Path::new("/existing/new/inner"), |path| {
            observed.push(path.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            observed,
            ["/existing/new/inner", "/existing/new", "/existing", "/"]
                .map(std::path::PathBuf::from)
        );
        let error = sync_ancestors(Path::new("/existing/new/inner"), |path| {
            if path == Path::new("/existing") {
                Err(std::io::Error::other("ancestor sync failed"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "ancestor sync failed");
        observed.clear();
        sync_ancestors(Path::new("new/inner"), |path| {
            observed.push(path.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            observed,
            ["new/inner", "new", "."].map(std::path::PathBuf::from)
        );
    }
}
