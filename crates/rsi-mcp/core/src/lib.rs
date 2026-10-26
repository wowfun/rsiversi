//! MCP transports, verified catalogs and ordinary frozen Agent contributions.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
pub use rsi_mcp_protocol::*;
#[cfg(test)]
extern crate self as rsi_mcp;

mod discovery;
mod error;
mod owner;
mod private;
mod service;
mod transport;

/// Exercises bounded JSON-RPC and split SSE decoding without opening a transport.
#[cfg(feature = "test-support")]
pub fn exercise_frame_parser(bytes: &[u8], chunk_bytes: usize) -> ParserExercise {
    transport::exercise_frame_parser(bytes, chunk_bytes)
}

/// Observable results of the isolated parser exercise.
#[cfg(feature = "test-support")]
#[derive(Debug)]
pub struct ParserExercise {
    /// Admission of the input as one JSON-RPC frame.
    pub json: std::result::Result<serde_json::Value, McpError>,
    /// Complete SSE messages emitted before exhaustion or failure.
    pub messages: Vec<serde_json::Value>,
    /// Terminal SSE decoding error, if any.
    pub event_error: Option<McpError>,
}
pub use owner::{McpFactory, McpOwner, McpOwnerContract, SETTINGS_NAMESPACE};
pub use private::PrivateMcp;
mod content;
mod contribution;
pub use contribution::McpToolsFactory;
pub use service::{FrozenServer, McpContract, McpService};
