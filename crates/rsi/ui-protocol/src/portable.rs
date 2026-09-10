//! Closed Portable UI source protocol. Replies are one bounded Message followed
//! by clean channel completion; source bodies are raw bytes, never JSON arrays.
use crate::{ActionInput, ExportScope, PresentationIdentity, TargetKind};
use serde::{Deserialize, Serialize};

/// Exact business contract carried by a caller-selected Portable service key.
pub const CONTRACT: &str = "rsi.ui.portable";
/// Version of the request grammar, independent of native ABI and model schemas.
pub const VERSION: u32 = 1;
/// Maximum one-message metadata, request or model reply including its envelope.
pub const MAXIMUM_PACKET_BYTES: usize = 128 * 1024;

/// Immutable source metadata, captured before contributing any Local UI entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    /// Logical bundle name, unique within the application registry.
    pub name: String,
    /// Explicit surfaces exposed by this source.
    pub surfaces: Vec<Surface>,
    /// Explicit action names and their target class.
    pub actions: Vec<Action>,
}
/// One model source declaration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Surface {
    /// Bundle-local model source name.
    pub name: String,
    /// Plain accessible menu label.
    pub title: String,
    /// Permitted target class.
    pub target: TargetKind,
}
/// One action declaration. Runtime checks still require displayed membership.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Bundle-local action name.
    pub name: String,
    /// Permitted target class.
    pub target: TargetKind,
}
/// Caller-selected operation, with no Context, trait object or raw origin.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Reads the complete declaration without granting action authority.
    Describe {},
    /// Returns one renderer-neutral `UiModel` JSON reply.
    Snapshot {
        /// Exact opaque model presentation chosen by the Local owner.
        presentation: PresentationIdentity,
        /// Semantic target metadata; `Some` accompanies exactly one explicit API grant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ExportScope>,
    },
    /// Returns a `UiModel` after the admitted action settles.
    Invoke {
        /// Exact bound presentation.
        presentation: PresentationIdentity,
        /// Semantic metadata bound by the target owner, independently of this identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ExportScope>,
        /// Already admitted displayed action name.
        action: String,
        /// Bounded domain-validated payload.
        input: ActionInput,
    },
    /// Returns a single raw binary source window.
    Source {
        /// Exact bound presentation.
        presentation: PresentationIdentity,
        /// Semantic metadata accompanying the explicit target-scoped API grant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ExportScope>,
        /// Source exposed by the current snapshot.
        name: String,
        /// Requested byte offset.
        offset: u64,
        /// Nonzero upper bound, at most 64 KiB.
        maximum: usize,
    },
}
