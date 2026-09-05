use crate::SessionHostDiagnostics;
use crate::owner::{
    HostEpoch, SESSION_HOST_PROTOCOL_EPOCH, SessionHostError, SessionHostPaths,
    session_host_product_build, validate_launch_key,
};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    AgentControlRecord, AgentPresetId, MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
    MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS, MAXIMUM_SESSION_FACT_BYTES, MessageId, SessionFact,
    SessionHeader, SessionId, TurnId, WorkspaceTrust,
};
use rsi_agent_store_protocol::{
    MAXIMUM_STORE_FACT_PAGE_BYTES, StoreBackwardFactPage, StoreRecentSession,
    StoreRecentSessionCursor, StoreRecentSessionPage,
};
use rsi_agent_turn_protocol::{
    CancelResult, CancelTarget, MessageReceipt, MessageState, ObservationCursor,
    SessionObservation, SessionObservationStream, TurnError,
};
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest, MAXIMUM_APPROVAL_FIELD_BYTES};
use rsi_sandbox::SandboxMode;
use rsi_session::{
    CreateSession, MAXIMUM_SESSION_INPUT_IMAGE_BYTES, RecentSessionCursor, RecentSessionPage,
    SessionApplication, SessionApplicationError, SessionHandle, SessionHistoryPage, SessionInput,
    SessionSummary, SubmitDirectImage, SubmitInput, TurnReceipt, canonical_workspace_directory,
    validate_session_input,
};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const MAXIMUM_FRAME_BYTES: usize = MAXIMUM_SESSION_FACT_BYTES + 64 * 1024;
const MAXIMUM_HANDSHAKE_FRAME_BYTES: usize = 16 * 1024;
const MAXIMUM_IN_FLIGHT_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_UPLOAD_CHUNK_BYTES: usize = 48 * 1024;
const MAXIMUM_UPLOAD_CHUNK_BASE64_BYTES: usize = 64 * 1024;
const MAXIMUM_UPLOAD_FRAME_BYTES: usize = MAXIMUM_UPLOAD_CHUNK_BASE64_BYTES + 16 * 1024;
const MAXIMUM_CONNECTIONS: usize = 128;
const MAXIMUM_UNPUBLISHED_DRAFTS: usize = 1024;
const MAXIMUM_SEQUENCE_ITEMS: usize = 1024;
const UNPUBLISHED_DRAFT_IDLE_TIMEOUT: Duration = Duration::from_hours(1);
const UNPUBLISHED_DRAFT_SWEEP_INTERVAL: Duration = Duration::from_mins(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
const UPLOAD_READ_TIMEOUT: Duration = Duration::from_mins(1);
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);
const RESPONSE_READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time graceful server shutdown waits for admitted connections.
pub const SESSION_HOST_DRAIN_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientFrame {
    Hello {
        protocol_epoch: u32,
        product_build: String,
        launch_key: String,
        host_epoch: HostEpoch,
    },
    Request {
        request_id: String,
        operation: WireOperation,
    },
    UploadChunk {
        request_id: String,
        upload_id: u16,
        index: u32,
        data: String,
    },
    UploadEnd {
        request_id: String,
    },
}

#[derive(Debug)]
struct UnpublishedDraft {
    handle: Arc<dyn SessionHandle>,
    expires_at: tokio::time::Instant,
    _admission: OwnedSemaphorePermit,
}

type UnpublishedDrafts = tokio::sync::Mutex<BTreeMap<SessionId, UnpublishedDraft>>;

#[derive(Clone, Debug)]
struct FrameReadBudget {
    bytes: Arc<Semaphore>,
}

impl FrameReadBudget {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(maximum_bytes)),
        }
    }

    async fn acquire(&self, bytes: usize) -> Result<OwnedSemaphorePermit, SessionHostError> {
        let bytes = u32::try_from(bytes)
            .map_err(|_| SessionHostError::Invalid("frame length exceeds admission".into()))?;
        Arc::clone(&self.bytes)
            .acquire_many_owned(bytes)
            .await
            .map_err(|_| SessionHostError::Io("Session Host frame admission closed".into()))
    }
}

impl Default for FrameReadBudget {
    fn default() -> Self {
        Self::new(MAXIMUM_IN_FLIGHT_FRAME_BYTES)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireOperation {
    Probe,
    Create {
        cwd: String,
        session_id: Option<SessionId>,
        agent_preset_id: Option<AgentPresetId>,
        workspace_trust: WorkspaceTrust,
    },
    Attach {
        session_id: SessionId,
    },
    ListRecent {
        after: Option<WireRecentCursor>,
        limit: usize,
    },
    Header {
        session_id: SessionId,
    },
    SubmitInput {
        session_id: SessionId,
        message_id: MessageId,
        content: Vec<WireInputBlock>,
        model: Option<ModelRef>,
        sandbox: Option<SandboxMode>,
    },
    MessageStatus {
        session_id: SessionId,
        message_id: MessageId,
    },
    SubmitImage {
        session_id: SessionId,
        turn_id: TurnId,
        model: ModelRef,
        request: ImageRequest,
    },
    Cancel {
        session_id: SessionId,
        target: WireCancelTarget,
        reason: Option<String>,
    },
    History {
        session_id: SessionId,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    },
    Subscribe {
        session_id: SessionId,
        cursor: WireObservationCursor,
    },
    PendingApprovals {
        session_id: SessionId,
    },
    AnswerApproval {
        session_id: SessionId,
        approval_id: String,
        decision: ApprovalDecision,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireInputBlock {
    Text {
        text: String,
    },
    Image {
        upload_id: u16,
        bytes: u64,
        sha256: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireCancelTarget {
    Message { message_id: MessageId },
    Turn { turn_id: TurnId },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireRecentCursor {
    created_at_ms: u64,
    session_id: SessionId,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireObservationCursor {
    control_seq: u64,
    fact_seq: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ServerFrame {
    HelloOk {
        protocol_epoch: u32,
        product_build: String,
        launch_key: String,
        host_epoch: HostEpoch,
    },
    HelloRejected {
        reason: String,
    },
    Response {
        request_id: String,
        response: Option<WireResponse>,
        error: Option<WireError>,
    },
    Event {
        request_id: String,
        session_id: SessionId,
        update: WireUpdate,
    },
    Item {
        request_id: String,
        item: Box<WireItem>,
    },
    End {
        request_id: String,
        error: Option<WireError>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireResponse {
    Ready,
    Session {
        header: Box<SessionHeader>,
    },
    RecentStart {
        has_more: bool,
    },
    TurnReceipt {
        session_id: SessionId,
        turn_id: TurnId,
        accepted_seq: u64,
    },
    MessageReceipt {
        session_id: SessionId,
        message_id: MessageId,
        accepted_control_seq: u64,
        observed_fact_seq: u64,
        state: WireMessageState,
    },
    Cancel {
        accepted: bool,
        already_terminal: bool,
    },
    HistoryStart {
        before_seq: u64,
        durable_seq: u64,
        has_more: bool,
    },
    PendingApprovalsStart,
    ApprovalAnswer {
        accepted: bool,
    },
    Subscribed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum WireMessageState {
    Pending,
    Claimed {
        activation_id: rsi_agent_session_protocol::ActivationId,
        turn_id: TurnId,
        step_id: rsi_agent_session_protocol::StepId,
        entered_fact_seq: u64,
    },
    Discarded {
        reason: rsi_agent_session_protocol::MessageDiscardReason,
        control_seq: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireItem {
    Session {
        header: SessionHeader,
    },
    Fact {
        session_id: SessionId,
        fact: SessionFact,
    },
    Approval {
        request: ApprovalRequest,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireUpdate {
    Control {
        record: Box<AgentControlRecord>,
        durable_control_seq: u64,
    },
    Fact {
        fact: Box<SessionFact>,
        durable_fact_seq: u64,
    },
}

impl From<SessionObservation> for WireUpdate {
    fn from(update: SessionObservation) -> Self {
        match update {
            SessionObservation::Control {
                record,
                durable_control_seq,
            } => Self::Control {
                record: Box::new((*record).clone()),
                durable_control_seq,
            },
            SessionObservation::Fact {
                fact,
                durable_fact_seq,
            } => Self::Fact {
                fact: Box::new((*fact).clone()),
                durable_fact_seq,
            },
        }
    }
}

impl From<WireUpdate> for SessionObservation {
    fn from(update: WireUpdate) -> Self {
        match update {
            WireUpdate::Control {
                record,
                durable_control_seq,
            } => Self::Control {
                record: Arc::new(*record),
                durable_control_seq,
            },
            WireUpdate::Fact {
                fact,
                durable_fact_seq,
            } => Self::Fact {
                fact: Arc::new(*fact),
                durable_fact_seq,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireError {
    Invalid { message: String },
    NotFound { value: String },
    Conflict { session: String, turn: String },
    MessageConflict { session: String, message: String },
    MessageOutcomeUnknown { session: String, message: String },
    Capacity,
    ShuttingDown,
    Backend { message: String },
}

impl From<SessionApplicationError> for WireError {
    fn from(error: SessionApplicationError) -> Self {
        match error {
            SessionApplicationError::Invalid(message) => Self::Invalid { message },
            SessionApplicationError::NotFound(value) => Self::NotFound { value },
            SessionApplicationError::Conflict { session, turn } => Self::Conflict { session, turn },
            SessionApplicationError::MessageConflict { session, message } => {
                Self::MessageConflict { session, message }
            }
            SessionApplicationError::MessageOutcomeUnknown { session, message } => {
                Self::MessageOutcomeUnknown { session, message }
            }
            SessionApplicationError::Capacity => Self::Capacity,
            SessionApplicationError::ShuttingDown => Self::ShuttingDown,
            SessionApplicationError::Backend(message) => Self::Backend { message },
        }
    }
}

impl From<WireError> for SessionApplicationError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::Invalid { message } => Self::Invalid(message),
            WireError::NotFound { value } => Self::NotFound(value),
            WireError::Conflict { session, turn } => Self::Conflict { session, turn },
            WireError::MessageConflict { session, message } => {
                Self::MessageConflict { session, message }
            }
            WireError::MessageOutcomeUnknown { session, message } => {
                Self::MessageOutcomeUnknown { session, message }
            }
            WireError::Capacity => Self::Capacity,
            WireError::ShuttingDown => Self::ShuttingDown,
            WireError::Backend { message } => Self::Backend(message),
        }
    }
}

mod client;
mod framing;
mod server;

pub use client::UdsSessionApplication;
pub use server::UdsSessionServer;

use framing::{
    create_private_runtime_directory, host_as_session_error, host_as_wire_error,
    io_as_session_error, io_error, message_outcome_unknown, read_frame,
    read_frame_with_retained_budget, read_frame_with_timeout, read_subscription_frame,
    remove_stale_socket_after_failed_probe, validate_request_id, write_frame,
};

#[cfg(test)]
mod tests;
