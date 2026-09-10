use super::{NativeAddonError, NativeAddonReceipt, NativeAddonStore, Result, source};
use crate::writer_lock::WriterLock;
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet, fs::File, io::Read as _, os::unix::fs::MetadataExt as _, path::Path,
};

const MAXIMUM_INPUT_ENTRIES: usize = 1024;
const MAXIMUM_INPUT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAXIMUM_INPUT_BYTES: u64 = 64 * 1024 * 1024;

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
    /// Reopens the current manifest while retaining the originally selected directory authority.
    /// An inaccessible or replaced root is a conflict, distinct from an invalid manifest edit.
    pub fn reopen(&self) -> Result<Self> {
        self.check_root().map_err(|_| NativeAddonError::Conflict)?;
        let next = Self::open(&self.source.path)?;
        let original = self
            .source
            .directory
            .try_clone()?
            .into_std_file()
            .metadata()?;
        let current = next
            .source
            .directory
            .try_clone()?
            .into_std_file()
            .metadata()?;
        if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
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
        let mut pending = self.build().watch.clone();
        pending.sort();
        let mut visited = BTreeSet::new();
        let mut bytes = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        while let Some(path) = pending.pop() {
            source::relative(&path)?;
            if !visited.insert(path.clone()) {
                continue;
            }
            if visited.len() > MAXIMUM_INPUT_ENTRIES {
                return Err(NativeAddonError::Capacity("build input entries"));
            }
            let spelling = path.as_os_str().as_encoded_bytes();
            hash.update((spelling.len() as u64).to_le_bytes());
            hash.update(spelling);
            let metadata = match self.source.directory.symlink_metadata(&path) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    hash.update(b"missing");
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if metadata.is_dir() {
                hash.update(b"directory");
                let directory = rsi_files_native_fs::open_relative_directory_no_follow(
                    &self.source.directory,
                    &path,
                )?;
                let mut children = Vec::new();
                for entry in directory.entries()? {
                    if visited.len() + pending.len() + children.len() >= MAXIMUM_INPUT_ENTRIES {
                        return Err(NativeAddonError::Capacity("build input entries"));
                    }
                    children.push(path.join(entry?.file_name()));
                }
                children.sort();
                pending.extend(children);
            } else if metadata.is_file() {
                hash.update(b"file");
                let file = rsi_files_native_fs::open_relative_file_no_follow(
                    &self.source.directory,
                    &path,
                )?;
                let metadata = file.metadata()?;
                hash.update(metadata.mode().to_le_bytes());
                let maximum = MAXIMUM_INPUT_FILE_BYTES.min(MAXIMUM_INPUT_BYTES - bytes);
                if metadata.len() > maximum {
                    return Err(NativeAddonError::Capacity("build input bytes"));
                }
                let mut reader = file.take(maximum + 1);
                let mut content = Sha256::new();
                let mut count = 0_u64;
                loop {
                    let length = reader.read(&mut buffer)?;
                    if length == 0 {
                        break;
                    }
                    count += length as u64;
                    if count > maximum {
                        return Err(NativeAddonError::Capacity("build input bytes"));
                    }
                    content.update(&buffer[..length]);
                }
                bytes += count;
                hash.update(count.to_le_bytes());
                hash.update(content.finalize());
            } else {
                return Err(NativeAddonError::Invalid("regular build inputs required"));
            }
        }
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
        let original_manifest = self
            .source
            .manifest_directory
            .try_clone()?
            .into_std_file()
            .metadata()?;
        if (current_manifest.dev(), current_manifest.ino())
            != (original_manifest.dev(), original_manifest.ino())
        {
            return Err(NativeAddonError::Conflict);
        }
        let current = rsi_files_native_fs::open_absolute_directory_no_follow(self.cwd())?
            .into_std_file()
            .metadata()?;
        let original = self
            .source
            .directory
            .try_clone()?
            .into_std_file()
            .metadata()?;
        if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
            return Err(NativeAddonError::Conflict);
        }
        Ok(())
    }
}
