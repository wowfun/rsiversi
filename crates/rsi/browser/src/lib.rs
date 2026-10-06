//! Restricted deployment browser execution and deterministic acceptance.
#![forbid(unsafe_code)]

mod protocol;
mod runtime;
pub use protocol::*;
pub use runtime::{BrowserSession, ExplorationBrowser, NativeRuntime, RuntimeConfig};

/// Process-local, nonserializable preview observation port. Its owner supplies
/// the exact frozen policy and retains retirement; text confers no authority.
#[async_trait::async_trait]
pub trait PreviewBrowser: std::fmt::Debug + Send + Sync + 'static {
    async fn navigate(&self, url: &str) -> Result<String, String>;
    async fn observe(&self) -> Result<String, String>;
    async fn close(&self) -> Result<(), String>;
}
#[async_trait::async_trait]
impl PreviewBrowser for ExplorationBrowser {
    async fn navigate(&self, url: &str) -> Result<String, String> {
        ExplorationBrowser::navigate(self, url).await
    }
    async fn observe(&self) -> Result<String, String> {
        ExplorationBrowser::observe(self).await
    }
    async fn close(&self) -> Result<(), String> {
        ExplorationBrowser::close(self).await
    }
}
