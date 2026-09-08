//! Native API connection plugin with shared Rust framing and explicit credentials.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod config;
mod connection;
mod plugin;
mod response;
mod transport;
pub use config::HttpClientConfig;
pub use connection::HttpClient;
pub use plugin::HttpClientFactory;
