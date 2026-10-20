//! Shared native ownership primitives; policy remains with each caller.
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::Job;
