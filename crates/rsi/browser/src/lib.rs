//! Restricted deployment browser execution and deterministic acceptance.
#![forbid(unsafe_code)]

mod protocol;
mod runtime;
mod session_policy;
pub use session_policy::SessionPolicy;
mod plugin;
mod session;
mod tools;
mod ui;
pub use plugin::{
    RuntimePool, RuntimePoolContract, RuntimePoolFactory, SessionBrowserContract,
    SessionBrowserFactory,
};
pub use protocol::*;
pub use runtime::{BrowserSession, ExplorationBrowser, NativeRuntime, OpenError, RuntimeConfig};
pub use session::{
    BrowserBinding, SessionAuthority, SessionBrowser, SessionOperation, SessionResult,
};
pub use tools::SessionBrowserToolsFactory;
pub use ui::SessionBrowserUiFactory;

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
