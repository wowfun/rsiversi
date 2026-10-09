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

/// Generation-owned public-web destination resolver. Construction performs no I/O.
#[derive(Debug, Default)]
pub struct PublicDestinationResolver {
    dns: tokio::sync::OnceCell<std::sync::Arc<hickory_resolver::TokioResolver>>,
}
impl PublicDestinationResolver {
    /// Creates an uninitialized resolver with the owning policy's bounded cache.
    pub fn new() -> Self {
        Self::default()
    }
    /// Checks every answer and returns addresses that must be pinned to the connection.
    /// # Errors
    /// Rejects disallowed destinations, failed resolution, or non-public answers.
    pub async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, RetrievalError> {
        self.resolve_with(host, port, network::system_resolver)
            .await
    }
    async fn resolve_with(
        &self,
        host: &str,
        port: u16,
        initialize: impl FnOnce() -> Result<
            std::sync::Arc<hickory_resolver::TokioResolver>,
            RetrievalError,
        >,
    ) -> Result<Vec<std::net::SocketAddr>, RetrievalError> {
        let url = network::destination_url(host, port)?;
        let dns = if matches!(url.host(), Some(url::Host::Ipv4(_))) {
            Err(RetrievalError::Resolution)
        } else {
            self.dns
                .get_or_try_init(|| async { initialize() })
                .await
                .cloned()
        };
        network::resolve(&url, &dns).await
    }
}
