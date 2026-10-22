//! Authenticated native byte transports without product or API-domain policy.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{LocalClientStream, LocalServerStream, PipeListener, PipeName, connect};
