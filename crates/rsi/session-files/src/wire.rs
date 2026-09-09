use rsi_api_protocol::{
    OperationAccess, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_files_protocol::{
    FileKind, FileToken, FilesError, MAXIMUM_DIRECTORY_ENTRIES, MAXIMUM_DIRECTORY_PAGE_ENTRIES,
    MAXIMUM_FILE_PAGE_BYTES, MAXIMUM_FILE_PATH_BYTES, OpenedFile, RelativePath,
};
use rsi_session_protocol::SessionTarget;
use serde::{Deserialize, Serialize};

pub(super) const MAXIMUM_REQUEST_BYTES: usize = 2 * MAXIMUM_FILE_PATH_BYTES + 4096;
pub(super) const MAXIMUM_RESPONSE_BYTES: usize = 256 * 1024;

/// Exact operation metadata owned by this product adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesOperation {
    /// Capture a regular file or directory.
    Open,
    /// Read exact file bytes.
    Read,
    /// Page directory entries.
    List,
    /// Release a token early.
    Release,
}
impl FilesOperation {
    pub(super) const ALL: [Self; 4] = [Self::Open, Self::Read, Self::List, Self::Release];
    /// Registered authentication, cancellation and wire-budget policy.
    #[allow(clippy::missing_panics_doc)] // All operation identifiers are compile-time constants.
    pub fn spec(self) -> OperationSpec {
        let name = match self {
            Self::Open => "open",
            Self::Read => "read",
            Self::List => "list",
            Self::Release => "release",
        };
        OperationSpec {
            id: OperationId::new("files", name, 1).expect("constant Files operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: OperationEffect::Read,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: MAXIMUM_REQUEST_BYTES,
            maximum_response_bytes: MAXIMUM_RESPONSE_BYTES,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request<T> {
    pub target: SessionTarget,
    pub input: T,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reply<I, O> {
    pub request: Request<I>,
    pub body: O,
}

pub(super) trait Input {
    fn validate(&self) -> rsi_files_protocol::Result<()>;
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Open {
    pub path: RelativePath,
    pub kind: FileKind,
}
impl Input for Open {
    fn validate(&self) -> rsi_files_protocol::Result<()> {
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Read {
    pub file: OpenedFile,
    pub offset: u64,
    pub maximum: usize,
}
impl Input for Read {
    fn validate(&self) -> rsi_files_protocol::Result<()> {
        if self.file.kind != FileKind::File
            || self.maximum == 0
            || self.maximum > MAXIMUM_FILE_PAGE_BYTES
            || self.offset > self.file.length
        {
            return Err(FilesError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct List {
    pub file: OpenedFile,
    pub offset: usize,
    pub maximum: usize,
}
impl Input for List {
    fn validate(&self) -> rsi_files_protocol::Result<()> {
        if self.file.kind != FileKind::Directory
            || self.file.length > MAXIMUM_DIRECTORY_ENTRIES as u64
            || self.maximum == 0
            || self.maximum > MAXIMUM_DIRECTORY_PAGE_ENTRIES
            || self.offset as u64 > self.file.length
        {
            return Err(FilesError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Release {
    pub token: FileToken,
}
impl Input for Release {
    fn validate(&self) -> rsi_files_protocol::Result<()> {
        Ok(())
    }
}
