//! Optional, explicitly supplied browser launch exchange.
use rsi_api_protocol::Result;
use rsi_credentials_protocol::SecretValue;

/// Resolves the deployment's durable browser principal after cookie authentication.
#[async_trait::async_trait]
pub trait BrowserOwner: std::fmt::Debug + Send + Sync + 'static {
    /// Returns the identity used for browser-owned application state and grants.
    async fn owner_id(&self) -> rsi_api_protocol::Result<rsi_api_protocol::DeviceId>;
}
/// Optional product-supplied browser principal for HTTP listeners.
#[derive(Debug)]
pub struct BrowserOwnerContract;
impl rsi_meta::LocalContract for BrowserOwnerContract {
    const KEY: &'static str = "rsi.http.browser-owner";
    type Service = dyn BrowserOwner;
}

/// Application-owned one-use launch authority, never device administration.
/// The returned device credential is consumed only by the HTTP cookie adapter.
pub trait BrowserBootstrap: std::fmt::Debug + Send + Sync + 'static {
    /// Atomically consumes a bounded ticket or rejects expired/retired authority.
    fn redeem(&self, ticket: &SecretValue) -> Result<SecretValue>;
}
