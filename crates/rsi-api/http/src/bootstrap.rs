//! Optional, explicitly supplied browser launch exchange.
use rsi_api_protocol::Result;
use rsi_credentials_protocol::SecretValue;

/// Application-owned one-use launch authority, never device administration.
/// The returned device credential is consumed only by the HTTP cookie adapter.
pub trait BrowserBootstrap: std::fmt::Debug + Send + Sync + 'static {
    /// Atomically consumes a bounded ticket or rejects expired/retired authority.
    fn redeem(&self, ticket: &SecretValue) -> Result<SecretValue>;
}
