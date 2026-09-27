//! Verified producer identity shared by native applications and their Worker.
#![deny(unsafe_code)]
#![warn(missing_docs)]

/// Paired source-manifest digest, absent for an ordinary standalone Cargo build.
pub const fn family() -> Option<&'static str> {
    option_env!("RSI_COMPILED_BUILD_FAMILY")
}

/// A managed publication remains alive while any native process retains its lock.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct GenerationLease {
    _file: std::fs::File,
}

#[cfg(not(target_arch = "wasm32"))]
impl GenerationLease {
    /// Pins an adjacent managed generation; external distributions need no GC lease.
    ///
    /// # Errors
    /// Returns executable resolution, publication access or lock failures.
    pub fn current() -> std::io::Result<Option<Self>> {
        let executable = std::env::current_exe()?.canonicalize()?;
        let Some(parent) = executable.parent() else {
            return Ok(None);
        };
        Self::open(parent)
    }
    /// Pins a managed bundle directory for a supervising watcher.
    ///
    /// # Errors
    /// Returns publication access or lock failures. A missing lease file denotes
    /// caller-owned output and returns `None`.
    pub fn open(bundle: &std::path::Path) -> std::io::Result<Option<Self>> {
        let path = bundle.join(".rsi-generation.lock");
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        file.lock_shared()?;
        Ok(Some(Self { _file: file }))
    }
}
