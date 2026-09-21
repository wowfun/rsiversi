//! Metadata-only, generation-owned activity without cold Session hydration.
use crate::{SessionId, TurnId};
use serde::{Deserialize, Serialize};

/// Current ownership of native activity, not external-effect settlement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    /// Current Kernel owns nonterminal work.
    Running,
    /// No open Turn exists at the durable cut.
    Idle,
    /// Durable open work has no current local owner or an owner read failed.
    Unknown,
}
/// Exact live human interaction, with no copied question or tool payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivityRequest {
    /// Approval owner validates this exact Session/Turn/request tuple.
    Approval {
        /// Owning Turn.
        turn: TurnId,
        /// Exact broker request identity.
        request: String,
    },
    /// Question owner validates this exact Session/Turn/request tuple.
    Question {
        /// Owning Turn.
        turn: TurnId,
        /// Exact broker request identity.
        request: String,
    },
}
/// One bounded native attention candidate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionActivity {
    /// Exact native Session identity.
    pub session: SessionId,
    /// Canonical decimal durable Fact watermark; never a floating-point number.
    pub fact_seq: String,
    /// Current-owner classification.
    pub status: ActivityStatus,
    /// At most 32 current exact interaction targets.
    pub requests: Vec<ActivityRequest>,
    /// More live interactions were omitted.
    pub truncated: bool,
}
/// At most 128 distinct candidates, without loading historical transcripts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionActivityPage {
    /// Bounded current-generation candidates.
    pub entries: Vec<SessionActivity>,
    /// Additional residents or interactions were omitted.
    pub truncated: bool,
}
