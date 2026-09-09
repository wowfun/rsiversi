//! Authenticated standard-product Files endpoints and shared native/Worker clients.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_api_protocol::ApiError;
use rsi_files_protocol::{
    DirectoryPage, FileKind, FilePage, FileToken, FilesError, OpenedFile, RelativePath,
};
use rsi_session_protocol::SessionTarget;
use std::fmt;

mod client;
mod local;
mod plugin;
mod server;
mod wire;
pub use client::SessionFilesClient;
pub use plugin::{SessionFilesApiFactory, SessionFilesClientFactory};
pub use server::SessionFilesApi;
pub use wire::FilesOperation;

/// Read-domain failures preserve independent transport/authentication errors.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SessionFilesError {
    /// Files or Session binding failure.
    #[error(transparent)]
    Files(#[from] FilesError),
    /// Transport, authentication or API capacity failure.
    #[error(transparent)]
    Api(#[from] ApiError),
}
/// Session-bound file operation result.
pub type Result<T> = std::result::Result<T, SessionFilesError>;

/// Human workspace browsing over one authenticated API connection generation.
#[async_trait]
pub trait SessionFiles: fmt::Debug + Send + Sync + 'static {
    /// Open a new read snapshot for the currently bound Session workspace.
    async fn open(
        &self,
        target: SessionTarget,
        path: RelativePath,
        kind: FileKind,
    ) -> Result<OpenedFile>;
    /// Read exact bytes from a previously opened regular file.
    async fn read(
        &self,
        target: SessionTarget,
        file: OpenedFile,
        offset: u64,
        maximum: usize,
    ) -> Result<FilePage>;
    /// Read a bounded directory page from the captured snapshot.
    async fn list(
        &self,
        target: SessionTarget,
        file: OpenedFile,
        offset: usize,
        maximum: usize,
    ) -> Result<DirectoryPage>;
    /// Release a token early; absent tokens are already released.
    async fn release(&self, target: SessionTarget, token: FileToken) -> Result<()>;
}
/// Nominal Local capability consumed by application UI plugins.
#[derive(Debug)]
pub struct SessionFilesContract;
impl rsi_meta::LocalContract for SessionFilesContract {
    const KEY: &'static str = "rsi.session.files";
    type Service = dyn SessionFiles;
}
