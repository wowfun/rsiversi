//! Public HTTP/S retrieval, Exa search and ordinary frozen Agent contributions.
mod contribution;
mod decode;
mod network;
mod owner;
mod service;
pub use contribution::RetrievalToolsFactory;
pub use owner::{RetrievalContract, RetrievalFactory};
pub use rsi_retrieval_protocol::*;
pub use service::RetrievalService;

/// Resolves every destination address under the owning public-web policy.
/// Consumers must connect to a returned address without resolving the name again.
/// # Errors
/// Rejects disallowed destinations, failed DNS resolution, or any non-public address in the answer.
pub async fn resolve_public_destination(
    host: &str,
    port: u16,
) -> Result<Vec<std::net::SocketAddr>, rsi_retrieval_protocol::RetrievalError> {
    network::destination(host, port).await
}
