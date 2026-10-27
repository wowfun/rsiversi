//! Fixed-layout native installation authority, independent of product composition.
#![deny(unsafe_code)]
// Fallible I/O and admission rules are shared by the owning installation contract.
#![allow(clippy::missing_errors_doc)]

pub mod filesystem;
mod install;
pub mod legacy;
pub mod origin;
pub mod path_receipt;
mod paths;
pub mod receipts;
pub mod record;
pub mod transaction;
pub mod uninstall;
pub mod upgrade;
#[cfg(windows)]
#[allow(unsafe_code)] // Single documented Win32 environment-notification boundary.
mod windows_environment;
pub use install::{InstallOptions, install, probe};
pub use paths::{default_prefix, standard_paths};
pub use record::{Layout, Record};
pub type Result<T> = std::io::Result<T>;
pub fn error(message: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(message.to_string())
}
pub const PROTOCOL: &str = "{\"interface\":1,\"record\":1,\"transaction\":2}";
#[cfg(test)]
mod tests;
