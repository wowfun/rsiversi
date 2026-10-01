//! Closed process RPC schema; the transport independently bounds encoded messages.
use crate::execution::{Preparation, Prepared, Program, StartOptions};
use serde::{Deserialize, Serialize};

/// One bounded ordinary helper request.
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// First-request target policy initialization; cannot reconfigure a live server.
    Initialize {
        /// Bounded target program selectors and explicit extra values.
        configuration: crate::initialization::Initialization,
    },
    /// Resolve a target directory.
    Canonicalize {
        /// Absolute target directory candidate.
        path: String,
    },
    /// Resolve an exact configured program selector.
    Resolve {
        /// Finite target catalog key.
        selector: String,
    },
    /// Resolve and retain a bounded explicit target configuration on this connection.
    ResolveConfigured {
        /// Previously authorized extra values, never an ambient environment snapshot.
        policy: crate::initialization::ProgramPolicy,
    },
    /// Validate a workspace-only read scope through the target Sandbox owner.
    WorkspaceRead {
        /// Exact caller-selected effect mode.
        mode: rsi_sandbox::SandboxMode,
        /// Normalized target working directory.
        cwd: String,
        /// Normalized target workspace.
        workspace: String,
    },
    /// Reserve an exact native enforcement plan and stream identities.
    Prepare {
        /// Exact effect plan inputs.
        preparation: Preparation,
    },
    /// Consume the issuing connection's one-use plan.
    Start {
        /// Issuing connection plan handle.
        handle: u64,
        /// Native capture/input/terminal bounds.
        options: StartOptions,
    },
    /// Write once, returning the native accepted prefix, never transport credit.
    Write {
        /// Exact running process handle.
        handle: u64,
        /// At most 64 KiB of input.
        bytes: Vec<u8>,
    },
    /// Close stdin after previously acknowledged writes.
    CloseInput {
        /// Exact running duplex process.
        handle: u64,
    },
    /// Wait at most five seconds for outcome and settlement, then return their snapshot.
    Status {
        /// Exact running or settled process.
        handle: u64,
    },
    /// Release an already settled process and its streams.
    Release {
        /// Exact settled process.
        handle: u64,
    },
    /// Open a bounded read token in the target namespace.
    FilesOpen {
        /// Explicit normalized target workspace.
        workspace: String,
        /// Exact relative filename bytes.
        path: rsi_files_protocol::RelativePath,
        /// Required object type.
        kind: rsi_files_protocol::FileKind,
    },
    /// Read a bounded exact byte page.
    FilesRead {
        /// Connection-owned open handle.
        handle: u64,
        /// Whole-file byte offset.
        offset: u64,
        /// Requested bytes, within the Files owner limit.
        maximum: usize,
    },
    /// Read bounded immediate directory children.
    FilesList {
        /// Connection-owned open handle.
        handle: u64,
        /// Captured entry offset.
        offset: usize,
        /// Requested entries, within the Files owner limit.
        maximum: usize,
    },
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").finish_non_exhaustive()
    }
}
/// Verified native process exit facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Normal exit status, when available.
    pub exit_code: Option<i32>,
    /// Native signal, when available.
    pub signal: Option<i32>,
}
impl Outcome {
    /// Validates the mutually exclusive Linux wait status before publication.
    pub fn validate(&self) -> crate::frame::Result<()> {
        if matches!(
            (self.exit_code, self.signal),
            (Some(0..=255), None) | (None, Some(1..=64))
        ) {
            Ok(())
        } else {
            Err(crate::frame::FrameError::Invalid)
        }
    }
}
impl From<rsi_process::ProcessOutcome> for Outcome {
    fn from(value: rsi_process::ProcessOutcome) -> Self {
        Self {
            exit_code: value.exit_code,
            signal: value.signal,
        }
    }
}
impl From<Outcome> for rsi_process::ProcessOutcome {
    fn from(value: Outcome) -> Self {
        Self {
            exit_code: value.exit_code,
            signal: value.signal,
        }
    }
}
/// Closed remote errors omit command text, environment and filesystem diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// Malformed input or foreign handle.
    Invalid,
    /// Fixed resource reservation unavailable.
    Capacity,
    /// Required target capability unavailable.
    Unsupported,
    /// Connection owner has stopped accepting work.
    Closed,
    /// Native creation rejected before a process was published.
    Spawn,
    /// Native bounded read failed.
    Io,
    /// Admitted effects lack a verified outcome.
    OutcomeUnknown,
    /// The native process group did not settle within its bound.
    SettlementTimeout,
}
impl From<rsi_process::ProcessError> for Failure {
    fn from(value: rsi_process::ProcessError) -> Self {
        use rsi_process::ProcessError as E;
        match value {
            E::InvalidInput(_) => Self::Invalid,
            E::Capacity => Self::Capacity,
            E::ShuttingDown => Self::Closed,
            E::Unsupported => Self::Unsupported,
            E::Spawn(_) => Self::Spawn,
            E::OutcomeUnknown => Self::OutcomeUnknown,
            E::SettlementTimeout | E::ShutdownTimeout => Self::SettlementTimeout,
            E::Api(_) | E::Io(_) => Self::Io,
        }
    }
}
impl From<Failure> for rsi_process::ProcessError {
    fn from(value: Failure) -> Self {
        match value {
            Failure::Invalid => Self::InvalidInput("invalid target process request".into()),
            Failure::Capacity => Self::Capacity,
            Failure::Unsupported => Self::Unsupported,
            Failure::Closed => Self::ShuttingDown,
            Failure::Spawn => Self::Spawn("target rejected process creation".into()),
            Failure::Io => Self::Io("target process I/O failed".into()),
            Failure::OutcomeUnknown => Self::OutcomeUnknown,
            Failure::SettlementTimeout => Self::SettlementTimeout,
        }
    }
}
/// Exact complete response; consumers must also match it to their request shape.
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// Initialization completed after artifact lease and native prerequisite checks.
    Ready {
        /// SHA-256 of the independently leased helper image.
        artifact: String,
        /// Requested selectors unavailable in the target namespace.
        unavailable: Vec<String>,
    },
    /// Exact opened target object; neither identity grants authority.
    FilesOpened {
        /// Connection-local handle for continuation and reserved release.
        handle: u64,
        /// Native captured object metadata.
        opened: rsi_files_protocol::OpenedFile,
    },
    /// Native byte page with exact hex contents.
    FilePage {
        /// Page checked against the admitted open and requested range.
        page: rsi_files_protocol::FilePage,
    },
    /// Native captured immediate children.
    DirectoryPage {
        /// Page checked against the admitted directory and requested range.
        page: rsi_files_protocol::DirectoryPage,
    },
    /// Closed Files failure taxonomy preserved independently of transport failure.
    FilesFailed {
        /// Verified native request rejection or version/read failure.
        failure: rsi_files_protocol::FilesError,
    },
    /// Canonical target directory, without local path resolution.
    Path {
        /// Normalized POSIX target directory.
        path: String,
    },
    /// Target-selected executable and environment.
    Program {
        /// Exact target policy selection.
        program: Program,
    },
    /// Prepared enforcement and routing identities.
    Prepared {
        /// One-use plan and streams.
        prepared: Prepared,
    },
    /// Native process creation was acknowledged.
    Started {
        /// Target direct child identity.
        pid: u32,
    },
    /// Native input accepted this exact prefix.
    Written {
        /// Exact nonzero accepted prefix length.
        bytes: usize,
    },
    /// None means still running; no wait slot is retained.
    Status {
        /// Verified native settlement, independently of stream consumption.
        outcome: Option<std::result::Result<Outcome, Failure>>,
        /// Native reaping and pipe-task joins, without certifying lossless EOF.
        settlement: Option<std::result::Result<(), Failure>>,
    },
    /// Exact operation has completed.
    Done,
    /// Verified rejection or native failure, not a missing transport reply.
    Failed {
        /// Closed redacted failure class.
        failure: Failure,
    },
}
/// Encodes one captured-tail update with its whole-stream byte offset.
/// The EOF marker belongs to the enclosing credited frame.
pub fn encode_tail(offset: u64, bytes: &[u8]) -> crate::frame::Result<Vec<u8>> {
    if bytes.len() > crate::frame::MAXIMUM_FRAGMENT_BYTES - 8
        || offset.checked_add(bytes.len() as u64).is_none()
    {
        return Err(crate::frame::FrameError::Capacity);
    }
    let mut packet = Vec::with_capacity(8 + bytes.len());
    packet.extend_from_slice(&offset.to_be_bytes());
    packet.extend_from_slice(bytes);
    Ok(packet)
}
/// Validates one tail packet before updating a local retained capture.
pub fn decode_tail(packet: &[u8]) -> crate::frame::Result<(u64, &[u8])> {
    if !(8..=crate::frame::MAXIMUM_FRAGMENT_BYTES).contains(&packet.len()) {
        return Err(crate::frame::FrameError::Invalid);
    }
    let offset = u64::from_be_bytes(
        packet[..8]
            .try_into()
            .map_err(|_| crate::frame::FrameError::Invalid)?,
    );
    if offset.checked_add((packet.len() - 8) as u64).is_none() {
        return Err(crate::frame::FrameError::Invalid);
    }
    Ok((offset, &packet[8..]))
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod reply_debug_tests {
    use super::*;
    #[test]
    fn debug_omits_reply_strings() {
        let reply = Reply::Ready {
            artifact: "private-artifact".into(),
            unavailable: vec!["private-path".into()],
        };
        assert_eq!(format!("{reply:?}"), "Reply { .. }");
    }
}
