//! External conversation observation identities shared by native and wasm clients.
#![allow(clippy::missing_errors_doc)]
use serde::{Deserialize, Serialize};

/// Categorical failures never containing external payloads or launch credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// Invalid caller input.
    #[error("invalid external journal input")]
    Input,
    /// Saved schema or record violates the owning contract.
    #[error("external journal is incompatible or corrupt")]
    Corrupt,
    /// Another owner holds the journal lease.
    #[error("external journal is already owned")]
    Locked,
    /// A retained-byte, record or disk quota was reached.
    #[error("external journal quota reached")]
    Quota,
    /// Read workers or the bounded durable-mutation queue are occupied.
    #[error("external journal is busy")]
    Busy,
    /// No matching conversation or exact source exists.
    #[error("external journal source unavailable")]
    NotFound,
    /// The calling peer belongs to an obsolete connection generation.
    #[error("external connection generation is stale")]
    Stale,
    /// Storage could not complete; callers must not claim a confirmed append.
    #[error("external journal I/O failed")]
    Io,
}
/// Journal operation result.
pub type Result<T> = std::result::Result<T, Error>;

/// Opaque product-local external conversation identity, unrelated to native Session IDs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ConversationId(String);
impl ConversationId {
    /// Validates at most 128 ASCII identifier bytes.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(Error::Input);
        }
        Ok(Self(value))
    }
    /// Borrows the opaque identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for ConversationId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Negotiated stable operations, with missing flags always false.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
// These are independent advertised protocol flags, not mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    /// Remote complete-history replay.
    pub load: bool,
    /// Remote resume without replay.
    pub resume: bool,
    /// Remote explicit Session close.
    pub close: bool,
    /// Remote image prompt support.
    pub image: bool,
    /// Remote audio prompt support.
    pub audio: bool,
    /// Remote embedded-resource prompt support.
    pub embedded_context: bool,
}

/// Current local observation; Unknown makes no remote completion claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Preparing a connection whose remote state is not confirmed.
    Starting,
    /// Idle and connected.
    Ready,
    /// A prompt was sent and has not settled.
    Running,
    /// A full replay is in progress.
    Loading,
    /// Remote prompt returned normal completion.
    Completed,
    /// Remote prompt returned cancelled.
    Cancelled,
    /// The local prompt was cancelled before any wire admission.
    Discarded,
    /// An explicit failure was observed.
    Failed,
    /// Transport loss or restart left remote state unknown.
    Unknown,
    /// The owned peer was explicitly closed and reaped.
    Closed,
}

/// Exact stable remote prompt conclusion, without an objective-success claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    /// Peer ended its turn normally.
    EndTurn,
    /// Peer exhausted its token limit.
    MaxTokens,
    /// Peer exhausted its request limit.
    MaxTurnRequests,
    /// Peer refused the prompt.
    Refusal,
    /// Peer confirmed cancellation.
    Cancelled,
}

/// Bounded persisted metadata. It contains no launch configuration or secrets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Local external identity.
    pub id: ConversationId,
    /// Operator-configured endpoint identity only.
    pub endpoint: String,
    /// Canonical absolute workspace directory.
    pub cwd: String,
    /// Last confirmed remote Session identity, if any.
    pub remote: Option<String>,
    /// Connection generation fencing updates and permissions.
    #[serde(with = "decimal")]
    pub generation: u64,
    /// Complete visible projection epoch.
    #[serde(with = "decimal")]
    pub epoch: u64,
    /// Locally observed activity/settlement.
    pub status: Status,
    /// Last confirmed remote prompt stop reason, cleared on a new operation.
    pub completion: Option<Completion>,
    /// Last confirmed remote capabilities.
    pub capabilities: Capabilities,
}
// Workspace labels may come from a Host on a different OS than the UI client.
fn absolute_label(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("\\\\")
        || (value.len() >= 3
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
}
impl Snapshot {
    /// Validates the persisted metadata boundary without opening any resource.
    pub fn validate(&self) -> Result<()> {
        ConversationId::new(&self.endpoint)?;
        if self.endpoint.len() > 64
            || self.cwd.len() > 4096
            || self.cwd.contains('\0')
            || !absolute_label(&self.cwd)
            || self
                .remote
                .as_ref()
                .is_some_and(|remote| remote.is_empty() || remote.len() > 256)
            || self.generation > i64::MAX as u64
            || self.epoch == 0
            || self.epoch > i64::MAX as u64
        {
            return Err(Error::Input);
        }
        Ok(())
    }
}

/// Exact observation class, independently selectable by history consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// User-submitted prompt, without any claim of remote durable acceptance.
    User,
    /// Received ACP session update.
    Update,
    /// Received permission request and exact peer options.
    Permission,
}
impl RecordKind {
    /// Returns the stable observation class label.
    pub const fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Update => "update",
            Self::Permission => "permission",
        }
    }
    /// Parses the exact stable observation class label.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "update" => Ok(Self::Update),
            "permission" => Ok(Self::Permission),
            _ => Err(Error::Corrupt),
        }
    }
}
/// One exact source descriptor, optionally including its bounded inline payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Monotonic local sequence; never interpreted as a native Fact sequence.
    #[serde(with = "decimal")]
    pub sequence: u64,
    /// Projection epoch in which the event was observed.
    #[serde(with = "decimal")]
    pub epoch: u64,
    /// Exact content class.
    pub kind: RecordKind,
    /// Encoded JSON payload size for byte-window acquisition.
    pub bytes: usize,
    /// Inline JSON only when it fits the remaining 256 KiB page budget.
    pub value: Option<serde_json::Value>,
}
/// Bounded ascending external-history page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    /// At most 64 record descriptors and 256 KiB inline encoded payload.
    pub records: Vec<Record>,
    /// Whether later records remain in this exact epoch.
    pub has_more: bool,
}

mod decimal {
    use serde::{Deserialize as _, Deserializer, Serializer};
    #[allow(clippy::trivially_copy_pass_by_ref)] // Required by serde's field serializer.
    pub(super) fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let text = String::deserialize(deserializer)?;
        let number: u64 = text.parse().map_err(serde::de::Error::custom)?;
        if text != number.to_string() {
            return Err(serde::de::Error::custom("invalid journal sequence"));
        }
        Ok(number)
    }
}
