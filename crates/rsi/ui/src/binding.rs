use crate::Result;
use async_trait::async_trait;
use rsi_meta::Context;
use std::{fmt, sync::Arc};

/// Explicit domain-owned target authority, separate from presentation identities.
#[derive(Debug)]
pub struct UiBusinessApi {
    /// Validated semantic target described to the Portable source.
    pub scope: crate::ExportScope,
    /// API client already restricted by its owning domain to that target.
    pub client: Arc<dyn rsi_api_protocol::ApiClient>,
}
/// Target-supplied business grant; an ordinary global `ApiClient` is never a fallback.
#[derive(Debug)]
pub struct UiBusinessApiContract;
impl rsi_meta::LocalContract for UiBusinessApiContract {
    const KEY: &'static str = "rsi.ui.target.business-api";
    type Service = UiBusinessApi;
}

/// Actual child-scope ownership, supplied by a trusted presentation adapter.
#[async_trait]
pub trait PresentationBindingOwner: fmt::Debug + Send + Sync + 'static {
    /// Fences new work. The implementation must own final cleanup even after Drop.
    fn retire(&self);
    /// Idempotently joins the actual scope after admitted presentation actions drain.
    async fn close(&self) -> Result<()>;
}
/// A source's ordinary child Context and its independently owned cleanup.
#[derive(Debug)]
pub struct PresentationBinding {
    context: Context,
    owner: Arc<dyn PresentationBindingOwner>,
}
impl PresentationBinding {
    /// Binds the source to a child of the original target, retaining its Local facets.
    pub fn new(context: Context, owner: Arc<dyn PresentationBindingOwner>) -> Self {
        Self { context, owner }
    }
    /// The real Context used by models, action handlers and source reads.
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// Fences and joins this child scope; the owner also handles a discarded binding.
    pub async fn close(self) -> Result<()> {
        self.owner.retire();
        self.owner.close().await
    }
}
impl Drop for PresentationBinding {
    fn drop(&mut self) {
        self.owner.retire();
    }
}
