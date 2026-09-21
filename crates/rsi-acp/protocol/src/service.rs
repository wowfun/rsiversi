//! Host-owned external conversation capability without launch configuration.
#![allow(clippy::missing_errors_doc)]
use crate::observation::{ConversationId, Page, Snapshot};
use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
/// Explicit remote setup selection; missing capability never implies support.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Setup {
    /// Create a new remote Session exactly once for this local identity.
    New,
    /// Resume the confirmed remote identity without replaying history.
    Resume,
    /// Replace the visible local projection with the complete remote replay.
    Load,
}

/// One exact peer option, without inferred local authorization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
pub struct PermissionOption {
    /// Exact peer-provided option identity.
    pub id: String,
    /// Bounded peer-provided label.
    pub name: String,
    /// Exact standard allow/reject-once/always kind.
    pub kind: String,
}
/// Bounded live permission presentation; the journal source holds full tool details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
pub struct Permission {
    /// Local opaque request identity within this exact connection.
    pub id: String,
    /// Canonical decimal connection generation.
    pub generation: String,
    /// Bounded title; renderers must apply their normal terminal/HTML escaping.
    pub title: String,
    /// Exact advertised choices, including all four standard kinds.
    pub options: Vec<PermissionOption>,
    /// Exact local journal sequence for reading the complete permission request.
    pub source_sequence: String,
}

/// Redacted service failures, independent of transport and remote error prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    /// Invalid bounded caller input.
    #[error("invalid external conversation input")]
    Input,
    /// No exact saved conversation or configured endpoint exists.
    #[error("external conversation or endpoint unavailable")]
    NotFound,
    /// An operation, worker or resident slot is already occupied.
    #[error("external conversation capacity is busy")]
    Busy,
    /// Saved identity or exact live interaction is obsolete.
    #[error("external conversation identity is stale")]
    Stale,
    /// Remote capability was not advertised.
    #[error("external operation is unsupported")]
    Unsupported,
    /// Peer explicitly rejected the operation.
    #[error("external peer rejected the operation")]
    Remote,
    /// Launch confinement, credential or Process acquisition failed.
    #[error("external endpoint could not be launched")]
    Launch,
    /// Local history could not be safely stored or read.
    #[error("external observation storage failed")]
    Journal,
    /// Delivery or remote settlement is unknown; never retry a prompt implicitly.
    #[error("external operation outcome is unknown")]
    Unknown,
}
/// External capability operation result.
pub type Result<T> = std::result::Result<T, Error>;

/// Redacted configured selection; it contains no launch paths or credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    /// Exact operator-configured endpoint ID.
    pub id: String,
    /// Whether the operator explicitly enabled launch.
    pub enabled: bool,
}
/// Current observation with ephemeral interactions separate from history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    /// Bounded persisted observation.
    pub snapshot: Snapshot,
    /// Whether a Host-owned peer currently accepts control.
    pub connected: bool,
    /// At most 32 exact live permission requests; restart never recreates them.
    pub permissions: Vec<Permission>,
}

/// Exact live attention target without duplicating all peer options.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionSummary {
    /// Exact connection-local permission request identity.
    pub id: String,
    /// Canonical decimal connection generation.
    pub generation: String,
    /// Bounded visible peer title.
    pub title: String,
}
/// Bounded resident activity and attention, without transcript or full tool payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resident {
    /// Current persisted observation.
    pub snapshot: Snapshot,
    /// Canonical decimal highest visible observation sequence, within snapshot epoch.
    pub sequence: String,
    /// Whether the exact peer accepts control.
    pub connected: bool,
    /// At most 32 exact outstanding interaction targets.
    pub permissions: Vec<PermissionSummary>,
}

/// One authority for direct interaction and delegation to configured endpoints.
#[async_trait]
pub trait ExternalConversations: std::fmt::Debug + Send + Sync + 'static {
    /// Enumerates at most 64 configured endpoint identities without launch data.
    async fn endpoints(&self) -> Result<Vec<Endpoint>>;
    /// Reads at most eight resident views without creating peers or granting authority.
    async fn residents(&self) -> Result<Vec<Resident>>;
    /// Reads at most 64 saved observations after an exact opaque local identity.
    async fn list(&self, after: Option<ConversationId>) -> Result<Vec<Snapshot>>;
    /// Reads saved observation and any current exact-connection interactions.
    async fn view(&self, id: &ConversationId) -> Result<View>;
    /// Reserves the caller's identity and creates one remote Session. Repeated identity reads.
    async fn start(&self, id: ConversationId, endpoint: &str) -> Result<Snapshot>;
    /// Explicitly resumes or loads an existing remote identity. New is rejected.
    async fn reconnect(&self, id: &ConversationId, setup: Setup) -> Result<Snapshot>;
    /// Admits one text prompt without retry; response proves only local admission.
    async fn submit(&self, id: &ConversationId, text: &str) -> Result<Snapshot>;
    /// Cancels and waits for actual peer settlement within the owning deadline.
    async fn cancel(&self, id: &ConversationId) -> Result<Snapshot>;
    /// Closes one peer and awaits Process cleanup, retaining saved observations.
    async fn close(&self, id: &ConversationId) -> Result<Snapshot>;
    /// Answers the exact connection, request and peer option identity.
    async fn answer(
        &self,
        id: &ConversationId,
        generation: u64,
        permission: &str,
        option: &str,
    ) -> Result<()>;
    /// Reads a bounded page from one complete observed epoch.
    async fn page(&self, id: &ConversationId, epoch: u64, after: u64) -> Result<Page>;
    /// Reads at most 64 KiB of an exact encoded record, without changing its source.
    async fn window(
        &self,
        id: &ConversationId,
        epoch: u64,
        sequence: u64,
        start: usize,
    ) -> Result<Vec<u8>>;
}
/// Local external-conversation service; it grants no endpoint configuration access.
#[derive(Debug)]
pub struct ExternalConversationsContract;
impl LocalContract for ExternalConversationsContract {
    const KEY: &'static str = "rsi.acp.conversations";
    type Service = dyn ExternalConversations;
}
