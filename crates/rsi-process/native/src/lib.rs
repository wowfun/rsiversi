//! Shared native ownership primitives; policy remains with each caller.
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::Job;
#[cfg(windows)]
#[allow(unsafe_code)]
mod conpty;
#[cfg(windows)]
pub use conpty::{Console, ConsoleChild};
#[cfg(windows)]
#[allow(unsafe_code)]
mod identity;
#[cfg(windows)]
pub use identity::{ProcessHandle, ProcessIdentity};
#[cfg(windows)]
#[allow(unsafe_code)]
mod inheritance;
#[cfg(windows)]
pub use inheritance::clear_standard_handle_inheritance;
