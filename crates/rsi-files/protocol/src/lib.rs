//! Bounded read-only filesystem contracts. Authority belongs to the caller.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Preferred exact file page size.
pub const PREFERRED_FILE_PAGE_BYTES: usize = 16 * 1024;
/// Hard exact file page size.
pub const MAXIMUM_FILE_PAGE_BYTES: usize = 64 * 1024;
/// Preferred directory page length.
pub const PREFERRED_DIRECTORY_PAGE_ENTRIES: usize = 128;
/// Hard directory page length.
pub const MAXIMUM_DIRECTORY_PAGE_ENTRIES: usize = 256;
/// Conservative encoded JSON byte budget for one directory page.
pub const MAXIMUM_DIRECTORY_PAGE_BYTES: usize = 64 * 1024;
/// Maximum entries retained in one directory snapshot.
pub const MAXIMUM_DIRECTORY_ENTRIES: usize = 4096;
/// Maximum aggregate exact name bytes in a directory snapshot.
pub const MAXIMUM_DIRECTORY_NAME_BYTES: usize = 1024 * 1024;
/// Maximum live token resources across provider generations in one process.
pub const MAXIMUM_FILE_TOKENS: usize = 64;
/// Maximum actual blocking jobs across provider generations in one process.
pub const MAXIMUM_FILE_JOBS: usize = 4;
/// Maximum relative path bytes.
pub const MAXIMUM_FILE_PATH_BYTES: usize = 16 * 1024;
/// Fixed token lifetime; reads do not extend it.
pub const FILE_TOKEN_LIFETIME: Duration = Duration::from_mins(5);

/// Read failure without exposing native absolute paths or credentials.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesError {
    /// Malformed request.
    #[error("invalid Files request")]
    Invalid,
    /// Binding does not match the token's admitted caller.
    #[error("Files binding changed")]
    Binding,
    /// Missing, expired or released token/object.
    #[error("Files object unavailable")]
    Unavailable,
    /// Object changed since the captured version.
    #[error("Files object changed; refresh required")]
    Changed,
    /// Provider resource budget exhausted.
    #[error("Files capacity exhausted")]
    Capacity,
    /// Caller cancellation or retired provider.
    #[error("Files read cancelled")]
    Cancelled,
    /// Platform cannot supply the promised filesystem confinement.
    #[error("Files unsupported on this platform")]
    Unsupported,
    /// Native filesystem rejected this operation.
    #[error("Files I/O failed")]
    Io,
}
/// Files operation result.
pub type Result<T> = std::result::Result<T, FilesError>;

/// Opaque identity issued by a trusted caller for its service generation.
#[derive(Clone, Default)]
pub struct FilesCaller(Arc<()>);
impl fmt::Debug for FilesCaller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilesCaller").finish_non_exhaustive()
    }
}
impl PartialEq for FilesCaller {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for FilesCaller {}

/// Non-wire binding supplied only after the caller's own authority checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesBinding {
    caller: FilesCaller,
    subject: Box<str>,
    revision: Box<str>,
    workspace: PathBuf,
}
impl FilesBinding {
    /// Bind an authorized caller and immutable subject revision to a native root.
    pub fn new(
        caller: FilesCaller,
        subject: &str,
        revision: &str,
        workspace: PathBuf,
    ) -> Result<Self> {
        if [subject, revision].iter().any(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
        }) || !workspace.is_absolute()
            || !rsi_workspace_path::is_normalized_absolute_path(&workspace)
        {
            return Err(FilesError::Invalid);
        }
        Ok(Self {
            caller,
            subject: subject.into(),
            revision: revision.into(),
            workspace,
        })
    }
    /// Owning caller generation for lifecycle cleanup.
    pub fn caller(&self) -> &FilesCaller {
        &self.caller
    }

    /// Exact native root selected by the trusted caller.
    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }
}

/// Validated relative filename bytes; serialized as lowercase hex.
#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct RelativePath(Box<[u8]>);
impl RelativePath {
    /// Validate exact path bytes. Empty selects the root directory.
    pub fn new(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAXIMUM_FILE_PATH_BYTES
            || bytes.contains(&0)
            || (!bytes.is_empty()
                && bytes
                    .split(|b| *b == b'/')
                    .any(|part| part.is_empty() || part == b"." || part == b".."))
        {
            return Err(FilesError::Invalid);
        }
        Ok(Self(bytes.into()))
    }
    /// Exact relative bytes, without lossy filename decoding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Append one native filename, validating the resulting relative path.
    pub fn join(&self, name: &[u8]) -> Result<Self> {
        if name.is_empty()
            || name.contains(&b'/')
            || self
                .0
                .len()
                .saturating_add(name.len())
                .saturating_add(usize::from(!self.0.is_empty()))
                > MAXIMUM_FILE_PATH_BYTES
        {
            return Err(FilesError::Invalid);
        }
        let mut bytes = self.0.to_vec();
        if !bytes.is_empty() {
            bytes.push(b'/');
        }
        bytes.extend_from_slice(name);
        Self::new(&bytes)
    }
}
impl Serialize for RelativePath {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(&self.0))
    }
}
impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > MAXIMUM_FILE_PATH_BYTES * 2 {
            return Err(serde::de::Error::custom(FilesError::Invalid));
        }
        let bytes = hex::decode(value).map_err(serde::de::Error::custom)?;
        Self::new(&bytes).map_err(serde::de::Error::custom)
    }
}

/// Bounded correlation handle, not an authorization credential.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FileToken(Box<str>);
impl TryFrom<String> for FileToken {
    type Error = FilesError;
    fn try_from(value: String) -> Result<Self> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(FilesError::Invalid);
        }
        Ok(Self(value.into()))
    }
}
impl From<FileToken> for String {
    fn from(value: FileToken) -> Self {
        value.0.into()
    }
}

/// Object type admitted by a read operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Regular file.
    File,
    /// Directory with a bounded captured enumeration.
    Directory,
}
/// Opened read resource and its captured length.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenedFile {
    /// Exact path opened from the retained root.
    pub path: RelativePath,
    /// Correlation handle required on continuation.
    pub token: FileToken,
    /// Regular file or directory.
    pub kind: FileKind,
    /// File bytes or directory entries at capture.
    pub length: u64,
}
/// Exact, bounded regular-file page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePage {
    /// First byte offset.
    pub offset: u64,
    /// Captured total length.
    pub total: u64,
    /// Exact bytes, including invalid UTF-8 and terminal controls.
    pub bytes_hex: String,
}
/// One untrusted directory entry; symlinks and special files are not selectable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    /// Lossy UTF-8 label; render as untrusted text.
    pub name: String,
    /// Exact relative path for future open.
    pub path: RelativePath,
    /// None for symlinks, sockets, devices and other non-readable kinds.
    pub kind: Option<FileKind>,
}
/// One bounded page of an immutable directory enumeration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryPage {
    /// First entry offset.
    pub offset: usize,
    /// Captured total entry count.
    pub total: usize,
    /// Entries ordered by exact native name bytes.
    pub entries: Vec<DirectoryEntry>,
}

/// Shared reader. Entrypoints must establish current authority before every call.
#[async_trait]
pub trait Files: fmt::Debug + Send + Sync + 'static {
    /// Release a caller generation after its owner has stopped and drained admission.
    fn release_caller(&self, caller: &FilesCaller);
    /// Inspect admitted metadata under a currently authorized binding.
    fn describe(&self, binding: &FilesBinding, token: &FileToken) -> Result<OpenedFile>;
    /// Acquire a new bounded token and capture its object version.
    async fn open(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        kind: FileKind,
        cancellation: CancellationToken,
    ) -> Result<OpenedFile>;
    /// Read exact file bytes, rechecking both token binding and object version.
    async fn read(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: u64,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<FilePage>;
    /// Read a directory page, rejecting a changed enumeration.
    async fn list(
        &self,
        binding: FilesBinding,
        token: FileToken,
        offset: usize,
        maximum: usize,
        cancellation: CancellationToken,
    ) -> Result<DirectoryPage>;
    /// Release a token early; absent tokens are already released.
    fn release(&self, binding: &FilesBinding, token: &FileToken) -> Result<()>;
}
/// Nominal Local capability for the reader provider.
#[derive(Debug)]
pub struct FilesContract;
impl rsi_meta_contract::LocalContract for FilesContract {
    const KEY: &'static str = "rsi.files";
    type Service = dyn Files;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_paths_round_trip_and_reject_escape_before_native_work() {
        for path in [b"a/\xff".as_slice(), b"\xe4\xb8\xad\xe6\x96\x87", b""] {
            let value = RelativePath::new(path).unwrap();
            assert_eq!(
                serde_json::from_str::<RelativePath>(&serde_json::to_string(&value).unwrap())
                    .unwrap(),
                value
            );
        }
        for path in ["/a", "../a", "a/../b", "a//b", "a/", "./a", "\0"] {
            assert_eq!(RelativePath::new(path.as_bytes()), Err(FilesError::Invalid));
        }
        assert!(RelativePath::new(&vec![b'a'; MAXIMUM_FILE_PATH_BYTES + 1]).is_err());
        assert!(serde_json::from_str::<RelativePath>("\"2e2e2f78\"").is_err());
        assert!(serde_json::from_str::<FileToken>("\"x\"").is_err());
    }
    #[test]
    fn caller_identity_is_not_value_equality_and_binding_is_bounded() {
        let caller = FilesCaller::default();
        assert_eq!(caller, caller.clone());
        assert_ne!(caller, FilesCaller::default());
        assert!(FilesBinding::new(caller.clone(), "session", "header", "relative".into()).is_err());
        assert!(FilesBinding::new(caller, &"s".repeat(257), "header", "/".into()).is_err());
    }
}
