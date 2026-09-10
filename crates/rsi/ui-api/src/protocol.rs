//! Closed UI operation DTOs; semantic scope validation belongs to the target owner.
use rsi_api_protocol::{
    OperationAccess, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_ui::{ActionInput, ModelSnapshot, PresentationAction, PresentationIdentity};
use serde::{Deserialize, Serialize};

/// Full model plus bounded transport envelope.
pub const MAXIMUM_ITEM_BYTES: usize = rsi_ui::MAXIMUM_VIEW_BYTES + 4096;
pub use rsi_ui::ExportScope;
/// A stable selection; the server creates fresh generation-bound references.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// Actual domain target requested by the caller.
    pub scope: ExportScope,
    /// Declared contribution name.
    pub bundle: String,
    /// Bundle-local surface name.
    pub surface: String,
}
/// Safe catalog entry without an ephemeral target address.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    /// Declared contribution name.
    pub bundle: String,
    /// Bundle-local surface name.
    pub surface: String,
    /// Human-readable label.
    pub title: String,
}
/// Logical continuation key; never a retained target or an authorization token.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCursor {
    /// Stable declared contribution name.
    pub bundle: String,
    /// Stable surface name within that contribution.
    pub surface: String,
}
/// Bounded declaration page over a temporary authorized binding.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRequest {
    /// Semantic target scope.
    pub scope: ExportScope,
    /// Exclusive logical continuation; replacement may require a fresh catalog.
    pub after: Option<CatalogCursor>,
    /// Number of entries, in 1..=64.
    pub maximum: usize,
}
/// A declaration page; opening an observer always binds a fresh target.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPage {
    /// At most the requested number of logical surfaces.
    pub entries: Vec<CatalogEntry>,
    /// Continue after this logical key when more declarations are available.
    pub next: Option<CatalogCursor>,
}
/// One multiplexed observer for this exact application lifetime.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observe {
    /// Client-generated lifetime nonce; grants no authority.
    pub application: String,
    /// At most 16 independently bound presentations.
    pub selections: Vec<Selection>,
}
/// Complete presentation item; ticket absence means invocation is busy or closed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// Index in the observer's immutable selections.
    pub selection: usize,
    /// Full model including its exact presentation epoch and revision.
    pub snapshot: ModelSnapshot,
    /// One current replay fence; consumed even when UI admission fails.
    pub ticket: Option<String>,
}
/// One action from a displayed snapshot, sent once.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invoke {
    /// Exact observer lifetime nonce.
    pub application: String,
    /// Exact displayed action address.
    pub action: PresentationAction,
    /// Current one-time input ticket.
    pub ticket: String,
    /// Validated bounded form data.
    pub input: ActionInput,
}
/// Byte window from one displayed model's declared source.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Exact observer lifetime nonce.
    pub application: String,
    /// Exact current presentation.
    pub presentation: PresentationIdentity,
    /// Exact displayed revision.
    pub revision: u64,
    /// Source declared by that model.
    pub name: String,
    /// Byte offset in that source.
    pub offset: u64,
    /// Requested bytes, in 1..=65536.
    pub maximum: usize,
}
/// Exact public descriptors used by both client and registry.
pub fn operations() -> [OperationSpec; 4] {
    [
        operation("catalog"),
        operation("observe"),
        operation("invoke"),
        operation("source"),
    ]
}
pub(crate) fn operation(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("ui", name, 1).expect("constant UI operation"),
        access: OperationAccess::Authenticated,
        class: if name == "observe" {
            OperationClass::Subscription
        } else {
            OperationClass::Data
        },
        effect: if name == "invoke" {
            OperationEffect::Mutation
        } else {
            OperationEffect::Read
        },
        encoding: RequestEncoding::Json,
        maximum_request_bytes: rsi_ui::MAXIMUM_INPUT_BYTES + 4096,
        maximum_response_bytes: MAXIMUM_ITEM_BYTES,
    }
}
