//! Native same-UID API connection without product server or domain dependencies.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(any(unix, windows))]
mod connection;
#[cfg(any(unix, windows))]
mod io;
#[cfg(any(unix, windows))]
mod transport;
#[cfg(any(unix, windows))]
pub use connection::{UdsClient, UdsClientConfig, UdsClientFactory};

#[cfg(all(any(unix, windows), feature = "test-support"))]
mod measurement;
#[cfg(all(any(unix, windows), feature = "test-support"))]
pub use measurement::ExchangeTiming;
