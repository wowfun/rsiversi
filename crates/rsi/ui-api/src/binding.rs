use crate::ExportScope;
use async_trait::async_trait;
use rsi_api_protocol::{CallOrigin, Result};
use rsi_meta::LocalContract;
use rsi_ui::{Ui, UiTarget};
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Ordinary Meta target ownership supplied by the product's semantic binder.
#[async_trait]
pub trait UiBindingOwner: fmt::Debug + Send + Sync + 'static {
    /// Fences further target work immediately; must be idempotent.
    fn retire(&self);
    /// Joins target cleanup, after the adapter has drained presentation actions.
    async fn close(&self) -> Result<()>;
}
/// A real UI target and the owner that keeps its domain dependencies alive.
#[derive(Debug)]
pub struct UiBinding {
    /// Registry in which the target is actually registered.
    pub ui: Arc<Ui>,
    /// Exact target created by the trusted binding owner.
    pub target: Arc<UiTarget>,
    owner: Arc<dyn UiBindingOwner>,
}
impl UiBinding {
    /// Associates a real target with its ordinary asynchronous lifecycle owner.
    pub fn new(ui: Arc<Ui>, target: Arc<UiTarget>, owner: Arc<dyn UiBindingOwner>) -> Self {
        Self { ui, target, owner }
    }
    /// Fences and joins the owned target. Drop also fences, but cannot await cleanup.
    pub async fn close(self) -> Result<()> {
        self.owner.retire();
        self.owner.close().await
    }
}
impl Drop for UiBinding {
    fn drop(&mut self) {
        self.owner.retire();
    }
}
/// Explicit semantic authorization and target construction; no request-supplied Context.
#[async_trait]
pub trait UiTargetBinder: fmt::Debug + Send + Sync + 'static {
    /// Validates the scope against this trusted origin and constructs an owned Meta target.
    /// Implementations own admitted startup/cleanup even if this waiter is dropped.
    async fn bind(
        &self,
        origin: CallOrigin,
        scope: ExportScope,
        stop: CancellationToken,
    ) -> Result<UiBinding>;
}
/// Product-supplied semantic binding capability, consumed by the generic UI API.
#[derive(Debug)]
pub struct UiTargetBinderContract;
impl LocalContract for UiTargetBinderContract {
    const KEY: &'static str = "rsi.ui.target.binder";
    type Service = dyn UiTargetBinder;
}
