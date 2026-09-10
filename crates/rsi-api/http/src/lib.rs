//! Native authenticated HTTP transport for registered domain operations.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod access;
mod assets;
pub use assets::{AssetType, HttpAsset, HttpAssets, HttpAssetsContract};
mod bounded_io;
#[cfg(unix)]
mod local;
#[cfg(unix)]
pub use local::LocalHttpService;
mod delivery;
mod diagnostics;
pub use diagnostics::{HttpDiagnostics, HttpDiagnosticsSnapshot};
mod h2_tasks;
mod plugin;
mod policy;
mod server;
mod sse;
mod tls;
pub use plugin::{HttpFactory, HttpListener, HttpListenerContract, StaticHttpFactory};
pub use policy::{HttpConfig, TlsFiles};
pub use server::{HttpServer, HttpServices};
