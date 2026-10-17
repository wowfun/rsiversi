//! Verified producer identity shared by native applications and their Worker.
#![deny(unsafe_code)]
#![warn(missing_docs)]

/// Paired source-manifest digest, absent for an ordinary standalone Cargo build.
pub const fn family() -> Option<&'static str> {
    option_env!("RSI_COMPILED_BUILD_FAMILY")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn managed_layout_rejects_foreign_numeric_identifiers_before_lease_access() {
        let temp = tempfile::tempdir().unwrap();
        for id in ["5", "999999999999999999999999"] {
            let bundle = temp.path().join("generations").join(id).join("bundle");
            std::fs::create_dir_all(&bundle).unwrap();
            std::fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
            assert_eq!(
                GenerationLease::open(&bundle).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }
    #[test]
    fn opened_lease_cannot_pin_a_generation_already_collected() {
        let temp = tempfile::tempdir().unwrap();
        let generation = temp.path().join("generations/00000000000000000001");
        let bundle = generation.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
        std::fs::create_dir(temp.path().join("leases")).unwrap();
        let lock_path = temp.path().join("leases/00000000000000000001");
        let gc = std::fs::File::create(&lock_path).unwrap();
        let opened = std::fs::File::open(&lock_path).unwrap();
        gc.lock().unwrap();
        let trash = temp.path().join("trash");
        std::fs::rename(&generation, &trash).unwrap();
        std::fs::remove_dir_all(&trash).unwrap();
        drop(gc);
        std::fs::remove_file(lock_path).unwrap();
        assert_eq!(
            GenerationLease::acquire(&bundle, opened)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
    #[test]
    fn managed_lease_never_waits_for_gc_and_missing_managed_lock_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("generations/00000000000000000001/bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        assert!(GenerationLease::open(&bundle).unwrap().is_none());
        std::fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
        assert!(GenerationLease::open(&bundle).is_err());
        std::fs::create_dir(temp.path().join("leases")).unwrap();
        let lock = std::fs::File::create(temp.path().join("leases/00000000000000000001")).unwrap();
        lock.lock().unwrap();
        assert_eq!(
            GenerationLease::open(&bundle).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(lock);
        let lease = GenerationLease::open(&bundle).unwrap().unwrap();
        let gc = std::fs::File::options()
            .read(true)
            .write(true)
            .open(temp.path().join("leases/00000000000000000001"))
            .unwrap();
        assert!(matches!(
            gc.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(lease);
        gc.try_lock().unwrap();
    }
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
        let lease = Self::open(parent)?;
        if option_env!("RSI_COMPILED_MANAGED") == Some("true") && lease.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "managed publication lease missing; rebuild the application",
            ));
        }
        Ok(lease)
    }
    /// Pins a managed bundle directory for a supervising watcher.
    ///
    /// # Errors
    /// Returns publication access or lock failures. A missing managed marker
    /// denotes caller-owned output and returns `None`; a missing lease is an error.
    pub fn open(bundle: &std::path::Path) -> std::io::Result<Option<Self>> {
        match std::fs::metadata(bundle.join(".rsi-generation.lock")) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid managed publication layout",
            )
        };
        let generation = bundle.parent().ok_or_else(invalid)?;
        let id = generation
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(invalid)?;
        // Application publication grammar: apps/devtools/README.md.
        if id.len() != 20 || !id.bytes().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }
        let generations = generation.parent().ok_or_else(invalid)?;
        if generations.file_name().and_then(|v| v.to_str()) != Some("generations") {
            return Err(invalid());
        }
        let cache = generations.parent().ok_or_else(invalid)?;
        let file = std::fs::File::open(cache.join("leases").join(id))?;
        Self::acquire(bundle, file).map(Some)
    }
    fn acquire(bundle: &std::path::Path, file: std::fs::File) -> std::io::Result<Self> {
        file.try_lock_shared().map_err(std::io::Error::from)?;
        // GC can remove the generation and unlink this opened lease before we lock it.
        // Once locked, a surviving marker establishes the pin against future collection.
        std::fs::metadata(bundle.join(".rsi-generation.lock"))?;
        Ok(Self { _file: file })
    }
}
