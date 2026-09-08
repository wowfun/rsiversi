//! Same-origin browser API connection with shared Rust admission and decoding.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use rsi_api_protocol::EndpointId;
use serde::{Deserialize, Serialize};

/// Browser connection settings; origin and credentials are not Profile data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserClientConfig {
    /// Expected persisted deployment identity.
    pub endpoint_id: EndpointId,
    /// Development opt-in, accepted only for a loopback HTTP Worker origin.
    #[serde(default)]
    pub allow_loopback_http: bool,
}

#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
compile_error!("browser API requires a single-threaded Worker without WASM atomics");

#[cfg(target_arch = "wasm32")]
mod bridge;
#[cfg(target_arch = "wasm32")]
mod connection;
#[cfg(target_arch = "wasm32")]
mod plugin;
#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(target_arch = "wasm32")]
pub use bridge::BrowserResourceSnapshot;
#[cfg(target_arch = "wasm32")]
pub use connection::BrowserClient;
#[cfg(target_arch = "wasm32")]
pub use plugin::BrowserClientFactory;
