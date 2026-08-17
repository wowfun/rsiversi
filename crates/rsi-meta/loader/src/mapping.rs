use std::path::Path;

use libloading::Library;

use crate::LoaderError;

/// Maps a library while resolving every imported symbol before plugin code runs.
///
/// # Safety
///
/// The caller must uphold [`Library::new`]'s requirements for native initializers.
#[cfg(unix)]
pub(super) unsafe fn open_now(path: &Path, reported_path: &Path) -> Result<Library, LoaderError> {
    use libloading::os::unix::{Library as UnixLibrary, RTLD_LOCAL, RTLD_NOW};

    // SAFETY: The caller owns the native-initializer contract. RTLD_NOW only
    // strengthens failure timing by resolving imports during this call.
    unsafe { UnixLibrary::open(Some(path), RTLD_LOCAL | RTLD_NOW) }
        .map(Into::into)
        .map_err(|source| LoaderError::DynamicLoad {
            path: reported_path.to_owned(),
            source: std::sync::Arc::new(source),
        })
}

/// Maps a library using the platform loader.
///
/// # Safety
///
/// The caller must uphold [`Library::new`]'s requirements for native initializers.
#[cfg(not(unix))]
pub(super) unsafe fn open_now(path: &Path, reported_path: &Path) -> Result<Library, LoaderError> {
    // SAFETY: Forwarded unchanged from this function's caller contract.
    unsafe { Library::new(path) }.map_err(|source| LoaderError::DynamicLoad {
        path: reported_path.to_owned(),
        source: std::sync::Arc::new(source),
    })
}
