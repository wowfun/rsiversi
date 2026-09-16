//! MCP transports, verified catalogs and ordinary frozen Agent contributions.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
pub use rsi_mcp_protocol::*;
mod discovery;
mod error;
mod owner;
mod service;
mod transport;
pub use owner::{McpFactory, McpOwner, McpOwnerContract, SETTINGS_NAMESPACE};
mod contribution;
pub use contribution::McpToolsFactory;
pub use service::{FrozenServer, McpContract, McpService};
