//! Native same-UID API connection without product server or domain dependencies.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(unix)]
mod connection;
#[cfg(unix)]
mod io;
#[cfg(unix)]
mod transport;
#[cfg(unix)]
pub use connection::{UdsClient, UdsClientConfig, UdsClientFactory};
