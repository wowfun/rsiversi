use super::{
    ApplicationProfileId, HostProfileId, MAXIMUM_PROFILE_DOCUMENT_BYTES, ProfileCatalog,
    ProfileCatalogError, STANDARD_HOST_PROFILE, builtin_application, reject_builtin_target,
    reject_legacy_application,
};
use rsi_files_native_fs::open_absolute_directory_no_follow;
use rsi_host::{Host, HostProfileEditPreview};
use rustix::fs::{Mode, OFlags, openat};
use sha2::{Digest as _, Sha256};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// Failure before source publication. No variant reports a Runtime apply result.
#[derive(Debug, thiserror::Error)]
pub enum ProfileEditError {
    /// Source selection was not a writable user catalog document.
    #[error(transparent)]
    Catalog(#[from] ProfileCatalogError),
    /// The proposed program failed pure compilation or factory resolution.
    #[error(transparent)]
    Preview(#[from] rsi_host::HostError),
    /// The source, parent identity or captured dependencies changed since preview.
    #[error("Profile sources changed; preview the edit again")]
    Conflict,
    /// Another cooperating writer currently owns this Profile directory.
    #[error("Profile directory is locked by another writer")]
    Busy,
    /// The root or prospective bytes exceeded the catalog's document bound.
    #[error("Profile source exceeds the document byte limit")]
    TooLarge,
    /// A source was a symlink, special file or lacked a stable native identity.
    #[error("Profile edit requires a regular source and a stable directory")]
    InvalidSource,
    /// A native operation failed before the root was published.
    #[error("Profile source operation failed: {0}")]
    Io(#[from] std::io::Error),
}

/// One source publication, independent of Profile apply, restart or rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileEditReceipt {
    /// Selected source path when the edit was previewed.
    pub path: PathBuf,
    /// Complete prospective source-program digest accepted by pure preview.
    pub source_digest: String,
    /// Whether directory sync completed after successful atomic replacement.
    pub directory_synced: bool,
}

/// One bounded, non-cloneable edit bound to a source and a frozen Host.
/// Dropping it performs no write; committing consumes its authority exactly once.
pub struct ProfileEdit<'host> {
    host: &'host Host,
    path: PathBuf,
    directory: File,
    original: Vec<u8>,
    proposed: Vec<u8>,
    effective: HostProfileEditPreview,
    review_digest: String,
}

impl std::fmt::Debug for ProfileEdit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileEdit")
            .field("path", &self.path)
            .field("original_bytes", &self.original.len())
            .field("proposed_bytes", &self.proposed.len())
            .field("effective", &self.effective)
            .finish_non_exhaustive()
    }
}

impl ProfileCatalog {
    /// Previews one existing user Host source against an explicit frozen Host.
    pub fn preview_host_edit<'host>(
        &self,
        host: &'host Host,
        id: &HostProfileId,
        contents: &[u8],
    ) -> Result<ProfileEdit<'host>, ProfileEditError> {
        reject_builtin_target(
            "Host Profile",
            id.as_str(),
            id.as_str() == STANDARD_HOST_PROFILE,
        )?;
        ProfileEdit::preview(host, self.host_path(id), contents)
    }

    /// Previews one existing user Application source, including an invalid old source.
    pub fn preview_application_edit<'host>(
        &self,
        host: &'host Host,
        id: &ApplicationProfileId,
        contents: &[u8],
    ) -> Result<ProfileEdit<'host>, ProfileEditError> {
        reject_builtin_target(
            "Application Profile",
            id.as_str(),
            builtin_application(id).is_some(),
        )?;
        if id.as_str() == "session" {
            return Err(ProfileCatalogError::RetiredApplication.into());
        }
        let path = self.application_path(id);
        reject_legacy_application(&path)?;
        ProfileEdit::preview(host, path, contents)
    }
}

impl<'host> ProfileEdit<'host> {
    fn preview(
        host: &'host Host,
        path: PathBuf,
        contents: &[u8],
    ) -> Result<Self, ProfileEditError> {
        if contents.len() > MAXIMUM_PROFILE_DOCUMENT_BYTES {
            return Err(ProfileEditError::TooLarge);
        }
        let parent = path.parent().ok_or(ProfileEditError::InvalidSource)?;
        let directory = open_absolute_directory_no_follow(parent)?.into_std_file();
        let original = read_root(&directory, &path)?.0;
        let effective = host.preview_file_edit(&path, contents)?;
        let mut review = Sha256::new();
        super::hash_component(&mut review, b"domain", b"rsi.profile.edit.v1");
        super::hash_path(&mut review, b"path", &path);
        super::hash_component(&mut review, b"original", &sha256(&original));
        super::hash_component(
            &mut review,
            b"proposed",
            effective.proposed.source_digest().as_bytes(),
        );
        super::hash_component(&mut review, b"host", host.composition_digest()?.as_bytes());
        let edit = Self {
            host,
            path,
            directory,
            original,
            proposed: contents.to_vec(),
            effective,
            review_digest: hex::encode(review.finalize()),
        };
        edit.check_root()?;
        // The compiled root must be the exact selected source, not a redirected path.
        let root = edit
            .effective
            .sources
            .iter()
            .find(|source| source.path == edit.path)
            .ok_or(ProfileEditError::Conflict)?;
        if root.sha256 != sha256(&edit.proposed) {
            return Err(ProfileEditError::Conflict);
        }
        Ok(edit)
    }

    /// Original explicit source bytes, potentially containing user secrets.
    pub fn original_source(&self) -> &[u8] {
        &self.original
    }

    /// Prospective explicit source bytes, potentially containing user secrets.
    pub fn proposed_source(&self) -> &[u8] {
        &self.proposed
    }

    /// Redacted effective tree and resolved identities; plugin semantics are unprepared.
    pub const fn effective(&self) -> &HostProfileEditPreview {
        &self.effective
    }

    /// Selected writable root; includes cannot be selected through this value.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stable comparison key for the root, proposal and frozen composition, not a credential.
    pub fn review_digest(&self) -> &str {
        &self.review_digest
    }

    /// Publishes exactly this source edit, without applying or rolling back a Runtime.
    pub fn commit_once(self) -> Result<ProfileEditReceipt, ProfileEditError> {
        self.directory.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => ProfileEditError::Busy,
            std::fs::TryLockError::Error(error) => ProfileEditError::Io(error),
        })?;
        // This independently opened directory owns the lock until self is dropped.
        let mode = self.check_root()?;
        self.check_dependencies()?;
        let mut staged = Staged::create(&self.directory)?;
        staged
            .file
            .set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
        staged.file.write_all(&self.proposed)?;
        staged.file.sync_all()?;
        self.check_root()?;
        self.check_dependencies()?;
        rustix::fs::renameat(
            &self.directory,
            &staged.name,
            &self.directory,
            self.path
                .file_name()
                .ok_or(ProfileEditError::InvalidSource)?,
        )
        .map_err(std::io::Error::from)?;
        staged.published = true;
        // No fallible operation after publication may pretend the source was unwritten.
        let directory_synced = self.directory.sync_all().is_ok();
        Ok(ProfileEditReceipt {
            path: self.path.clone(),
            source_digest: self.effective.proposed.source_digest().to_owned(),
            directory_synced,
        })
    }

    fn check_root(&self) -> Result<u32, ProfileEditError> {
        let parent = self.path.parent().ok_or(ProfileEditError::InvalidSource)?;
        let current =
            open_absolute_directory_no_follow(parent).map_err(|_| ProfileEditError::Conflict)?;
        let current = current.into_std_file().metadata()?;
        let opened = self.directory.metadata()?;
        if current.dev() != opened.dev() || current.ino() != opened.ino() {
            return Err(ProfileEditError::Conflict);
        }
        let (bytes, mode) = read_root(&self.directory, &self.path)?;
        if sha256(&bytes) != sha256(&self.original) {
            return Err(ProfileEditError::Conflict);
        }
        Ok(mode)
    }

    fn check_dependencies(&self) -> Result<(), ProfileEditError> {
        let current = self
            .host
            .preview_file_edit(&self.path, &self.proposed)
            .map_err(|_| ProfileEditError::Conflict)?;
        if current.sources != self.effective.sources || current.proposed != self.effective.proposed
        {
            return Err(ProfileEditError::Conflict);
        }
        Ok(())
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn read_root(directory: &File, path: &Path) -> Result<(Vec<u8>, u32), ProfileEditError> {
    let name = path.file_name().ok_or(ProfileEditError::InvalidSource)?;
    let file = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(std::io::Error::from)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ProfileEditError::InvalidSource);
    }
    if metadata.len() > MAXIMUM_PROFILE_DOCUMENT_BYTES as u64 {
        return Err(ProfileEditError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAXIMUM_PROFILE_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAXIMUM_PROFILE_DOCUMENT_BYTES {
        return Err(ProfileEditError::TooLarge);
    }
    Ok((bytes, metadata.mode()))
}

struct Staged<'directory> {
    directory: &'directory File,
    name: String,
    file: File,
    published: bool,
}

impl<'directory> Staged<'directory> {
    fn create(directory: &'directory File) -> Result<Self, ProfileEditError> {
        let mut random = [0; 16];
        getrandom::fill(&mut random)
            .map_err(|_| std::io::Error::other("temporary name entropy unavailable"))?;
        let name = format!(".rsi-profile-{}.tmp", hex::encode(random));
        let file = openat(
            directory,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(std::io::Error::from)?;
        Ok(Self {
            directory,
            name,
            file,
            published: false,
        })
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = rustix::fs::unlinkat(self.directory, &self.name, rustix::fs::AtFlags::empty());
        }
    }
}
