//! Owned, bounded application UI contributions over the existing Meta lifetime graph.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod identity;
mod plugin;
mod registry;
mod view;
pub use plugin::{UiFactory, UiTargetFactory};
pub use registry::{ContributionLease, Ui, UiContract, UiTarget, UiTargetContract};
pub use view::*;

use futures_util::future::BoxFuture;
use rsi_conversation::{SourceIndex, ToolState};
use rsi_meta::Context;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Maximum active bundles in one application registry.
pub const MAXIMUM_BUNDLES: usize = 128;
/// Maximum actual application/surface targets.
pub const MAXIMUM_TARGETS: usize = 16;
/// Maximum surfaces, actions or renderers in one bundle.
pub const MAXIMUM_CONTRIBUTIONS: usize = 32;
/// Maximum admitted actions, including retiring owners' work.
pub const MAXIMUM_ACTIONS: usize = 8;
/// Maximum encoded action input.
pub const MAXIMUM_INPUT_BYTES: usize = 64 * 1024;
/// Maximum encoded presentation view.
pub const MAXIMUM_VIEW_BYTES: usize = 128 * 1024;
/// Maximum flat view elements.
pub const MAXIMUM_ELEMENTS: usize = 256;

/// UI boundary or contribution failure.
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    /// Invalid declaration, input or renderer output.
    #[error("invalid UI data: {0}")]
    Invalid(String),
    /// The exact target or contribution no longer admits work.
    #[error("UI action or surface has retired")]
    Retired,
    /// No waiting queue is allocated.
    #[error("UI contribution capacity exhausted")]
    Capacity,
    /// A handler failed; adapters display this as data.
    #[error("UI action failed: {0}")]
    Action(String),
    /// The underlying Meta owner rejected the operation.
    #[error(transparent)]
    Meta(#[from] rsi_meta::MetaError),
}
/// UI operation result.
pub type Result<T> = std::result::Result<T, UiError>;

/// Bound invocation authority and cooperative retirement signals.
#[derive(Clone, Debug)]
pub struct ActionTarget {
    pub(crate) context: Context,
    pub(crate) contribution_stop: CancellationToken,
    pub(crate) target_stop: CancellationToken,
}
impl ActionTarget {
    /// Exact target Context; its Local mappings select capabilities.
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// Whether either exact owner has started retiring.
    pub fn is_cancelled(&self) -> bool {
        self.contribution_stop.is_cancelled() || self.target_stop.is_cancelled()
    }
    /// Waits for either owner to request cooperative cleanup.
    pub async fn cancelled(&self) {
        tokio::select! {
            () = self.contribution_stop.cancelled() => {},
            () = self.target_stop.cancelled() => {},
        }
    }
}

/// Action implementation validates its business input before I/O.
pub trait UiAction: std::fmt::Debug + Send + Sync + 'static {
    /// Runs once under both owners, even if the response waiter is dropped.
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>>;
}
/// Pure surface presentation over the exact target's Local capabilities.
pub trait SurfaceRenderer: std::fmt::Debug + Send + Sync + 'static {
    /// Produces a bounded view without initiating unowned background work.
    fn render(&self, target: &Context) -> Result<UiView>;
}
/// Borrowed, bounded input to a contributed block renderer.
#[derive(Debug)]
pub struct BlockInput<'a> {
    /// Shared source identity.
    pub key: &'a str,
    /// Application's bounded visible text window.
    pub text: &'a str,
    /// Shared Tool metadata, when applicable.
    pub tool: Option<&'a ToolState>,
    /// Bounded source references, with no retained Fact or observation lease.
    pub sources: &'a SourceIndex,
}
/// Optional presentation replacement for a shared conversation block.
pub trait BlockRenderer: std::fmt::Debug + Send + Sync + 'static {
    /// Returns None when this renderer does not recognize the block.
    fn render(&self, target: &Context, block: &BlockInput<'_>) -> Result<Option<UiView>>;
}
/// Named, target-scoped logical panel and menu entry.
#[derive(Debug)]
pub struct SurfaceContribution {
    /// Bundle-local stable name.
    pub name: String,
    /// Menu label.
    pub title: String,
    /// Permitted target class.
    pub target: TargetKind,
    /// Pure view callback.
    pub renderer: Arc<dyn SurfaceRenderer>,
}
/// Exact bundle-local operation.
#[derive(Debug)]
pub struct ActionContribution {
    /// Name used by the bundle's buttons.
    pub name: String,
    /// Permitted target class.
    pub target: TargetKind,
    /// Owned asynchronous operation.
    pub handler: Arc<dyn UiAction>,
}
/// Ordered optional block renderer.
#[derive(Debug)]
pub struct BlockRendererContribution {
    /// Bundle-local stable name.
    pub name: String,
    /// Permitted target class.
    pub target: TargetKind,
    /// Pure rendering implementation.
    pub renderer: Arc<dyn BlockRenderer>,
}
/// One exact generation's independent UI contribution bundle.
#[derive(Debug, Default)]
pub struct Contributions {
    /// Stable logical bundle name, unique within an application.
    pub name: String,
    /// Ordered menu entries and panels.
    pub surfaces: Vec<SurfaceContribution>,
    /// Exact operations referenced by this bundle's views.
    pub actions: Vec<ActionContribution>,
    /// Ordered first-matching block renderers.
    pub renderers: Vec<BlockRendererContribution>,
}
