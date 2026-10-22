//! Non-executing product package admission, resolution and generation leases.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod filesystem;
mod manifest;
mod repository;
mod store;
pub use manifest::{Dependency, Export, ExportKind, FileRecord, Manifest, Provenance};
pub use repository::{
    Repository, RepositoryTarget, SignedRepository, TrustReceipt, VerifiedRepository,
};
pub use rsi_api_protocol::ProtocolDescriptor as Interface;
pub use store::{InstallSource, PackageLease, PackageStore, Resolution, Selection};

/// Maximum bounded encoded manifest, repository or resolution document.
pub const MAX_DOCUMENT: usize = 1024 * 1024;
/// A package may contain at most this many files.
pub const MAX_FILES: usize = 4096;
/// Maximum file length admitted before copying.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Maximum installed payload bytes.
pub const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Admission and publication failures never imply activation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid external document or path.
    #[error("invalid package: {0}")]
    Invalid(String),
    /// Another package writer holds the store.
    #[error("package store is busy")]
    Busy,
    /// Signature, freshness or rollback check failed.
    #[error("package trust check failed: {0}")]
    Trust(String),
    /// File or manifest bytes differ from the selected digest.
    #[error("package content digest differs")]
    Digest,
    /// Bounded native filesystem failure.
    #[error("package filesystem: {0}")]
    Io(#[from] std::io::Error),
}
/// Result of package admission or publication.
pub type Result<T> = std::result::Result<T, Error>;
#[cfg(unix)]
impl From<rustix::io::Errno> for Error {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}
fn invalid(value: impl Into<String>) -> Error {
    Error::Invalid(value.into())
}
/// Validates package, export and repository names at an external boundary.
pub fn validate_identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        || value.starts_with('.')
        || value.ends_with('.')
    {
        return Err(invalid("invalid identifier"));
    }
    Ok(())
}
use validate_identifier as identifier;

fn hash(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid SHA-256"));
    }
    Ok(())
}
fn digest(value: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(value))
}
