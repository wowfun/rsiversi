//! Owned, bounded application UI contributions over the existing Meta lifetime graph.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod binding;
mod identity;
pub use binding::{
    PresentationBinding, PresentationBindingOwner, UiBusinessApi, UiBusinessApiContract,
};
pub use identity::fresh_identity;
mod plugin;
mod registry;
pub use plugin::{UiFactory, UiTargetFactory};
pub use registry::{ContributionLease, Ui, UiContract, UiTarget, UiTargetContract};
pub use registry::{PresentationLease, PresentationStatus, SnapshotPin};
pub use rsi_ui_protocol::*;

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
/// Maximum live presentation workers, including draining workers.
pub const MAXIMUM_PRESENTATIONS: usize = 16;
/// Maximum current, candidate and escaped immutable snapshots.
pub const MAXIMUM_SNAPSHOTS: usize = 32;
/// Shared encoded snapshot storage across all presentations and readers.
pub const MAXIMUM_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

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
impl From<rsi_ui_protocol::ProtocolError> for UiError {
    fn from(error: rsi_ui_protocol::ProtocolError) -> Self {
        Self::Invalid(error.0)
    }
}

/// Bound invocation authority and cooperative retirement signals.
#[derive(Clone, Debug)]
pub struct ActionTarget {
    pub(crate) context: Context,
    pub(crate) contribution_stop: CancellationToken,
    pub(crate) target_stop: CancellationToken,
    pub(crate) presentation_stop: CancellationToken,
    pub(crate) presentation: Option<PresentationIdentity>,
}
impl ActionTarget {
    /// Exact presentation when invoked through a displayed model lease.
    pub fn presentation(&self) -> Option<&PresentationIdentity> {
        self.presentation.as_ref()
    }
    /// Exact target Context; its Local mappings select capabilities.
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// Whether either exact owner has started retiring.
    pub fn is_cancelled(&self) -> bool {
        self.contribution_stop.is_cancelled() || self.target_stop.is_cancelled()
    }
    /// Waits for the presenting detail to close. Read handlers can stop their local
    /// I/O; mutation handlers must preserve already dispatched work ownership.
    pub async fn view_closed(&self) {
        self.presentation_stop.cancelled().await;
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
    /// Produces an arbitrary named model after an owned invocation.
    fn invoke_model(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiModel>> {
        let result = self.invoke(target, input);
        Box::pin(async move { UiModel::standard(result.await?).map_err(Into::into) })
    }
}
/// Pure surface presentation over the exact target's Local capabilities.
pub trait SurfaceRenderer: std::fmt::Debug + Send + Sync + 'static {
    /// Optionally binds one actual child scope before model materialization.
    /// Startup must cooperate with `stop` and return only after owned rollback;
    /// the presentation keeps its admission until this future completes.
    fn bind(
        &self,
        _target: Context,
        _presentation: PresentationIdentity,
        _stop: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<PresentationBinding>>> {
        Box::pin(async { Ok(None) })
    }
    /// Produces a bounded view without initiating unowned background work.
    fn render(&self, _target: &Context) -> Result<UiView> {
        Err(UiError::Invalid(
            "source requires asynchronous model presentation".into(),
        ))
    }
    /// Materializes a bounded snapshot after the presentation owner reserves capacity.
    fn model(&self, target: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move { UiModel::standard(self.render(&target)?).map_err(Into::into) })
    }
    /// Materializes for the exact presentation. Portable adapters receive this
    /// opaque identity, never the Context or its Local mapping keys.
    fn model_in(
        &self,
        target: Context,
        _presentation: PresentationIdentity,
    ) -> BoxFuture<'_, Result<UiModel>> {
        self.model(target)
    }
    /// Reads one bounded source window from a name exposed by the current model.
    fn source(
        &self,
        _target: ActionTarget,
        _name: String,
        _offset: u64,
        _maximum: usize,
    ) -> BoxFuture<'static, Result<Vec<u8>>> {
        Box::pin(async { Err(UiError::Invalid("source is unavailable".into())) })
    }
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
