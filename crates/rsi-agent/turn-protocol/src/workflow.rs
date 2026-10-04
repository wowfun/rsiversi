//! Non-authorizing bounded Workflow snapshots for trusted Session adapters.
use rsi_agent_session_protocol::{
    ProgramBlob, ProgramChildReceipt, ProgramOutcome, ProgramRunId, SessionId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Encoded ceiling admitted before cloning one canonical overview.
pub const MAXIMUM_PROGRAM_OVERVIEW_BYTES: usize = 128 * 1024;
/// Encoded ceiling admitted before cloning one Local detail snapshot.
pub const MAXIMUM_PROGRAM_DETAILS_BYTES: usize = 2 * 1024 * 1024;

/// Canonical lifecycle summary; live ownership is reported separately as orphaned.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent durable lifecycle and live ownership observations; Kernel owns the state machine"
)]
pub struct ProgramOverview {
    pub session_id: SessionId,
    pub run_id: ProgramRunId,
    pub accepted_control_seq: u64,
    pub control_seq: u64,
    pub started: bool,
    pub detached: bool,
    pub cancelling: bool,
    pub orphaned: bool,
    pub children: usize,
    pub settled_children: usize,
    pub phase: Option<String>,
    pub progress: Option<String>,
    pub outcome: Option<ProgramOutcome>,
    pub script_ref: ProgramBlob,
    pub result_ref: Option<ProgramBlob>,
    #[serde(skip)]
    pub retention: Option<Arc<rsi_api_protocol::ByteReservation>>,
}
impl ProgramOverview {
    /// Validates external snapshot correlation and finite field bounds.
    pub fn validate(&self) -> crate::Result<()> {
        if self.accepted_control_seq == 0
            || self.control_seq < self.accepted_control_seq
            || self.children > rsi_agent_session_protocol::MAXIMUM_PROGRAM_CHILDREN as usize
            || self.settled_children > self.children
            || self.phase.as_ref().is_some_and(|v| {
                v.is_empty()
                    || v.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_PHASE_BYTES
                    || v.contains(['\0', '\u{7f}'])
            })
            || self.progress.as_ref().is_some_and(|v| {
                v.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_PROGRESS_BYTES
                    || v.contains('\0')
            })
            || self.orphaned && self.outcome.is_some()
            || self.detached && !self.started
            || matches!(self.outcome, Some(ProgramOutcome::Completed))
                && (!self.started || self.cancelling)
        {
            return Err(crate::TurnError::Invalid(
                "invalid workflow snapshot".into(),
            ));
        }
        if let Some(outcome) = &self.outcome {
            outcome
                .validate()
                .map_err(|e| crate::TurnError::Invalid(e.to_string()))?;
        }
        self.script_ref
            .validate(rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES as u64)
            .map_err(|e| crate::TurnError::Invalid(e.to_string()))?;
        if let Some(blob) = &self.result_ref {
            blob.validate(rsi_agent_session_protocol::MAXIMUM_PROGRAM_RESULT_BYTES as u64)
                .map_err(|e| crate::TurnError::Invalid(e.to_string()))?;
        }
        Ok(())
    }
}
/// One admitted initial child; later human activations are not run receipts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramChildView {
    pub ordinal: u32,
    pub session_id: SessionId,
    pub receipt: Option<ProgramChildReceipt>,
}
/// Maximum child receipts cloned into one Local workbench page.
pub const PROGRAM_CHILD_PAGE_SIZE: usize = 16;
/// One canonical detail read; CAS bodies are selected before replay.
#[derive(Clone, Debug, Default)]
#[allow(missing_docs)]
pub struct ProgramRead {
    pub expected_control_seq: Option<u64>,
    pub children_offset: usize,
    pub result: bool,
    pub script: bool,
}
/// One admitted child page and optional bodies bound to its canonical revision.
#[derive(Clone, Debug, Serialize)]
#[allow(missing_docs)]
pub struct ProgramDetails {
    pub overview: ProgramOverview,
    pub children: Vec<ProgramChildView>,
    pub children_offset: usize,
    pub next_children_offset: Option<usize>,
    #[serde(skip)]
    pub result: Option<rsi_api_protocol::RetainedBytes>,
    #[serde(skip)]
    pub script: Option<rsi_api_protocol::RetainedBytes>,
}
/// Run discovery is acceptance-ordered at a fixed Session watermark.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramHistoryPage {
    pub runs: Vec<ProgramOverview>,
    pub has_more: bool,
}
/// Cancellation acknowledgement never asserts process cleanup prematurely.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum ProgramCancelReceipt {
    Accepted {
        run_id: ProgramRunId,
        control_seq: u64,
    },
    AlreadyTerminal {
        run_id: ProgramRunId,
        control_seq: u64,
        outcome: ProgramOutcome,
    },
    OrphanedRequiresRestart {
        run_id: ProgramRunId,
        control_seq: u64,
        cancellation_requested: bool,
    },
}

impl ProgramCancelReceipt {
    /// Exact run whose cancellation was observed or requested.
    pub const fn run_id(&self) -> &ProgramRunId {
        match self {
            Self::Accepted { run_id, .. }
            | Self::AlreadyTerminal { run_id, .. }
            | Self::OrphanedRequiresRestart { run_id, .. } => run_id,
        }
    }
}
