pub use rsi_mcp_protocol::McpError;
pub(crate) type Result<T> = std::result::Result<T, McpError>;
