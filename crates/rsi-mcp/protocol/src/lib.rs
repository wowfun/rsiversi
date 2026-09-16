//! Bounded MCP configuration and complete typed saved manifests.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod config;
mod headers;
mod manifest;
pub use headers::{
    HttpParameter, MAXIMUM_HTTP_PARAMETER_BYTES, encode_header_value, http_parameters,
};
mod status;
pub use config::*;
pub use manifest::*;
use sha2::{Digest, Sha256};
pub use status::*;
/// MCP-owned Credentials address space.
pub const CREDENTIAL_OWNER: &str = "rsi.mcp";
/// Maximum enabled or disabled configured server identities.
pub const MAXIMUM_SERVERS: usize = 8;
/// Maximum complete RPC request or response frame before JSON materialization.
pub const MAXIMUM_FRAME_BYTES: usize = 1024 * 1024;
/// Complete discovery/manifest tool ceiling, also constrained by the shared registrar.
pub const MAXIMUM_TOOLS: usize = rsi_tools_protocol::MAXIMUM_REGISTERED_TOOLS;
/// Complete resource catalog ceiling.
pub const MAXIMUM_RESOURCES: usize = 256;
/// Integration-owned result; diagnostics contain no external transport text.
pub type Result<T> = std::result::Result<T, String>;
/// Hashes exact JSON number text and insertion order under repository policy.
///
/// # Panics
/// Panics if the supplied serializer rejects its value. MCP callers supply validated JSON data.
pub fn digest(value: &impl serde::Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("validated MCP value"))
    )
}
fn name(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}
