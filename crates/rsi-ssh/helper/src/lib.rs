//! Target-side helper lifetime enforcement.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(target_os = "linux")]
mod cache;
#[cfg(target_os = "linux")]
mod entry;
#[cfg(target_os = "linux")]
mod lifecycle;
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod stdio;
#[cfg(target_os = "linux")]
pub use cache::{ArtifactCache, ArtifactLease, CacheError};
#[cfg(target_os = "linux")]
pub use entry::Invocation;
#[cfg(target_os = "linux")]
pub use lifecycle::{SystemdWatchdog, TransientUnit};
#[cfg(target_os = "linux")]
pub use server::ExecutionServer;

/// Closed lifecycle failures without including target environment contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HelperError {
    /// An external launch coordinate or manager reply is malformed.
    #[error("invalid SSH helper lifecycle input")]
    Invalid,
    /// The exact required user-manager/cgroup/watchdog contract is absent.
    #[error("SSH helper lifecycle prerequisites are unavailable")]
    Unavailable,
    /// Bounded native inspection or notification failed.
    #[error("SSH helper lifecycle I/O failed")]
    Io,
}
/// Helper lifecycle result.
pub type Result<T> = std::result::Result<T, HelperError>;

#[cfg(target_os = "linux")]
fn valid_service_namespace(service: &str) -> bool {
    service.len() == 32
        && service
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
