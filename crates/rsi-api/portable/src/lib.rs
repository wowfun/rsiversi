//! Explicit Portable transport for a generation-bound API client capability.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod client;
mod error;
mod export;
mod plugin;
mod wire;
pub use client::PortableApiClient;
pub use plugin::{
    ApiExportContract, PortableApiClientFactory, PortableApiExportFactory, export_api,
};
