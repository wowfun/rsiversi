use super::{NativeAddonError, NativeAddonReceipt, NativeAddonStore, Result, source};
use crate::writer_lock::WriterLock;
use sha2::{Digest as _, Sha256};
use std::{fs::File, os::unix::fs::MetadataExt as _, path::Path};

/// A captured non-executing build declaration with pinned source-directory access.
pub struct NativeAddonBuild {
    source: source::ManifestSource,
}
impl std::fmt::Debug for NativeAddonBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeAddonBuild")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}
impl NativeAddonBuild {
    /// Captures a validated manifest; the output artifact need not exist yet.
    pub fn open(path: &Path) -> Result<Self> {
        let source = source::open_manifest(path)?;
        let build = source
            .manifest
            .build
            .as_ref()
            .ok_or(NativeAddonError::Invalid("build declaration required"))?;
        if build.command[0].contains('=') {
            return Err(NativeAddonError::Invalid(
                "build executable cannot be an environment assignment",
            ));
        }
        if build
            .watch
            .iter()
            .any(|path| source.manifest.artifact.starts_with(path))
        {
            return Err(NativeAddonError::Invalid(
                "watch includes the build artifact",
            ));
        }
        Ok(Self { source })
    }
    /// Reopens only the captured declaration under the original directory authority.
    /// Changing the declaration requires a new explicit build/watch invocation.
    pub fn reopen(&self) -> Result<Self> {
        self.check_root().map_err(|_| NativeAddonError::Conflict)?;
        let next = Self::open(&self.source.path)?;
        let original = self.source.directory.dir_metadata()?;
        let current = next.source.directory.dir_metadata()?;
        if (original.dev(), original.ino()) != (current.dev(), current.ino())
            || self.source.bytes != next.source.bytes
        {
            return Err(NativeAddonError::Conflict);
        }
        Ok(next)
    }
    /// Captured local addon identity.
    pub fn id(&self) -> &str {
        &self.source.manifest.id
    }
    /// Whether this declaration supplies explicit inputs for a watch loop.
    pub fn has_watch_inputs(&self) -> bool {
        !self.build().watch.is_empty()
    }
    /// Hashes the current bounded explicit inputs, rejecting a changed manifest or root.
    pub fn fingerprint(&self) -> Result<String> {
        self.check_root()?;
        let current = rsi_files_native_fs::open_relative_file_no_follow(
            &self.source.manifest_directory,
            self.source
                .path
                .file_name()
                .ok_or(NativeAddonError::Conflict)?
                .as_ref(),
        )?;
        if source::bounded_read(current, super::MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES)?
            != self.source.bytes
        {
            return Err(NativeAddonError::Conflict);
        }
        let mut hash = Sha256::new();
        hash.update(b"rsi.native-build-inputs/v1\0");
        hash.update(Sha256::digest(&self.source.bytes));
        let inputs = crate::source_inputs::fingerprint(&self.source.directory, &self.build().watch)
            .map_err(|error| {
                use crate::source_inputs::InputError;
                match error {
                    InputError::Capacity(resource) => NativeAddonError::Capacity(resource),
                    InputError::Invalid(message) => NativeAddonError::Invalid(message),
                    InputError::Changed => NativeAddonError::Conflict,
                    InputError::Io(error)
                        if rsi_files_native_fs::is_link_rejection(&error)
                            || error.raw_os_error()
                                == Some(rustix::io::Errno::NXIO.raw_os_error()) =>
                    {
                        NativeAddonError::Invalid("regular build inputs required")
                    }
                    InputError::Io(error) => error.into(),
                }
            })?;
        hash.update(inputs.as_bytes());
        self.check_root()?;
        Ok(hex::encode(hash.finalize()))
    }
    pub(crate) fn lock(&self) -> Result<WriterLock> {
        let directory =
            rsi_files_native_fs::open_absolute_directory_no_follow(self.cwd())?.into_std_file();
        let lock = WriterLock::acquire(directory).map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => NativeAddonError::Busy,
            std::fs::TryLockError::Error(error) => NativeAddonError::Io(error),
        })?;
        self.check_root()?;
        Ok(lock)
    }
    pub(crate) fn command(&self) -> &[String] {
        &self.build().command
    }
    pub(crate) fn timeout_seconds(&self) -> u64 {
        self.build().timeout_seconds
    }
    pub(crate) fn cwd(&self) -> &Path {
        &self.source.root
    }
    pub(crate) fn install(
        &self,
        store: &NativeAddonStore,
        fingerprint: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<NativeAddonReceipt> {
        let check = || {
            if cancelled() || self.fingerprint()? != fingerprint {
                return Err(NativeAddonError::Conflict);
            }
            Ok(())
        };
        check()?;
        let artifact: File = rsi_files_native_fs::open_relative_file_no_follow(
            &self.source.directory,
            &self.source.manifest.artifact,
        )?;
        store.install_checked(self.source.manifest.clone(), artifact, check)
    }
    fn build(&self) -> &source::Build {
        self.source
            .manifest
            .build
            .as_ref()
            .expect("validated build declaration")
    }
    fn check_root(&self) -> Result<()> {
        let manifest_parent = self
            .source
            .path
            .parent()
            .expect("validated manifest parent");
        let current_manifest =
            rsi_files_native_fs::open_absolute_directory_no_follow(manifest_parent)?
                .into_std_file()
                .metadata()?;
        let original_manifest = self.source.manifest_directory.dir_metadata()?;
        if (current_manifest.dev(), current_manifest.ino())
            != (original_manifest.dev(), original_manifest.ino())
        {
            return Err(NativeAddonError::Conflict);
        }
        let current = rsi_files_native_fs::open_absolute_directory_no_follow(self.cwd())?
            .into_std_file()
            .metadata()?;
        let original = self.source.directory.dir_metadata()?;
        if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
            return Err(NativeAddonError::Conflict);
        }
        Ok(())
    }
}
