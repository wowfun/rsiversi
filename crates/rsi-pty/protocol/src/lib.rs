//! Live terminal scopes and bounded transport values, independent of Sessions.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};

/// Terminals in one live scope.
pub const MAXIMUM_TERMINALS: usize = 8;
/// Attachments per terminal.
pub const MAXIMUM_ATTACHMENTS: usize = 8;
/// Retained bytes per follower, excluding its separately admitted snapshot.
pub const MAXIMUM_FOLLOWER_BYTES: usize = 2 * 1024 * 1024;
/// Maximum accepted input request bytes.
pub const MAXIMUM_INPUT_BYTES: usize = 64 * 1024;
/// Maximum raw UTF-8 bytes in one output page.
pub const MAXIMUM_OUTPUT_PAGE_BYTES: usize = 16 * 1024;

/// Closed live-terminal failure taxonomy.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "message",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PtyError {
    /// Invalid bounded request or response.
    #[error("invalid terminal value: {0}")]
    Invalid(String),
    /// The platform, sandbox or live generation is unavailable.
    #[error("Terminal unavailable: {0}")]
    Unavailable(String),
    /// Finite process, screen, snapshot, follower or input admission is exhausted.
    #[error("terminal capacity is exhausted")]
    Capacity,
    /// The caller no longer holds the current controller epoch.
    #[error("terminal controller changed")]
    StaleController,
    /// A sequence names different input or skips the admitted order.
    #[error("terminal input sequence conflicts")]
    InputConflict,
    /// A native operation failed; diagnostic text is bounded.
    #[error("terminal I/O failed: {0}")]
    Io(String),
}
/// Terminal result.
pub type Result<T> = std::result::Result<T, PtyError>;
/// Checked character dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    /// Character rows in 1..=200.
    pub rows: u16,
    /// Character columns in 1..=500.
    pub columns: u16,
}
impl Size {
    /// Validates at native and wire boundaries.
    pub fn validate(self) -> Result<()> {
        self.native()
            .validate()
            .map_err(|e| PtyError::Invalid(e.to_string()))
    }
    /// Native typed dimensions.
    pub const fn native(self) -> rsi_process::PtySize {
        rsi_process::PtySize {
            rows: self.rows,
            columns: self.columns,
        }
    }
}
/// Terminal lifecycle within this process generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Phase {
    /// Interactive shell still runs.
    Running,
    /// Shell and native output have settled.
    Exited {
        /// Native normal exit code.
        exit_code: Option<i32>,
        /// Native terminating signal.
        signal: Option<i32>,
    },
    /// Native settlement failed.
    Failed,
}
/// Bounded terminal status, with no native path or environment payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terminal {
    /// Opaque live terminal identity.
    pub id: String,
    /// Current character dimensions.
    pub size: Size,
    /// Native lifecycle.
    pub phase: Phase,
    /// Current writer attachment, absent after detach.
    pub controller: Option<String>,
    /// Current controller authority epoch.
    pub controller_epoch: u64,
}
/// New attachment with a separately bounded snapshot stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    /// Current terminal status.
    pub terminal: Terminal,
    /// Opaque attachment identity, scoped to this terminal.
    pub id: String,
    /// Current stream epoch; first read starts at byte zero.
    pub stream_epoch: u64,
}
/// One input attempt, independent of shell command completion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputState {
    /// Native write still owns its admission.
    Pending,
    /// Exact bytes accepted by one native write.
    Accepted {
        /// Accepted prefix length, never command completion.
        bytes: usize,
    },
    /// Receipt was lost or retired; never blindly replay this sequence.
    Unknown,
}
/// Retained answer for one controller's input sequence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReceipt {
    /// Controller epoch of this input.
    pub epoch: u64,
    /// Monotone sequence within that epoch.
    pub sequence: u64,
    /// Actual write admission outcome.
    pub result: InputState,
}
/// One bounded output page; no transcript ACK is consumed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputPage {
    /// Terminal state at this read.
    pub terminal: Terminal,
    /// Exact attachment identity.
    pub attachment: String,
    /// Stream generation that owns the cursor.
    pub stream_epoch: u64,
    /// Whether presentation and cursor must reset before this text.
    pub reset: bool,
    /// First byte represented in this page.
    pub cursor: u64,
    /// Cursor after the exact UTF-8 bytes in this page.
    pub next_cursor: u64,
    /// Complete bounded valid UTF-8; ANSI parsing may span pages.
    pub text: String,
}
/// Bounded operations over an existing trusted scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)] // Closed operation fields are named coordinates of the documented scope/epoch protocol.
pub enum Operation {
    List,
    Attach {
        terminal: String,
    },
    Read {
        terminal: String,
        attachment: String,
        stream_epoch: u64,
        cursor: u64,
    },
    Input {
        terminal: String,
        attachment: String,
        epoch: u64,
        sequence: u64,
        bytes: Vec<u8>,
    },
    Receipt {
        terminal: String,
        epoch: u64,
        sequence: u64,
    },
    Takeover {
        terminal: String,
        attachment: String,
    },
    Resize {
        terminal: String,
        attachment: String,
        epoch: u64,
        size: Size,
    },
    Detach {
        terminal: String,
        attachment: String,
        epoch: u64,
    },
    Close {
        terminal: String,
    },
    CloseAll,
}
impl Operation {
    /// Rejects oversized framing before any live lookup or allocation.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Read { stream_epoch, .. } => positive(*stream_epoch)?,
            Self::Input {
                epoch, sequence, ..
            }
            | Self::Receipt {
                epoch, sequence, ..
            } => {
                positive(*epoch)?;
                positive(*sequence)?;
            }
            Self::Resize { epoch, .. } | Self::Detach { epoch, .. } => positive(*epoch)?,
            _ => {}
        }
        match self {
            Self::List | Self::CloseAll => {}
            Self::Attach { terminal }
            | Self::Close { terminal }
            | Self::Receipt { terminal, .. } => identifier(terminal)?,
            Self::Read {
                terminal,
                attachment,
                ..
            }
            | Self::Takeover {
                terminal,
                attachment,
            }
            | Self::Detach {
                terminal,
                attachment,
                ..
            } => {
                identifier(terminal)?;
                identifier(attachment)?;
            }
            Self::Resize {
                terminal,
                attachment,
                size,
                ..
            } => {
                identifier(terminal)?;
                identifier(attachment)?;
                size.validate()?;
            }
            Self::Input {
                terminal,
                attachment,
                bytes,
                ..
            } => {
                identifier(terminal)?;
                identifier(attachment)?;
                if bytes.is_empty() || bytes.len() > MAXIMUM_INPUT_BYTES {
                    return Err(PtyError::Invalid("input must have 1..=64 KiB".into()));
                }
            }
        }
        Ok(())
    }
}
/// Typed operation replies, all carrying finite values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Reply {
    /// Complete bounded roster.
    List(Vec<Terminal>),
    /// Newly attached stream.
    Attached(Attachment),
    /// Bounded output page.
    Output(OutputPage),
    /// Retained input receipt.
    Input(InputReceipt),
    /// Current controller/dimensions after mutation.
    Terminal(Terminal),
    /// Idempotent detach/close completion.
    Done,
}
impl Reply {
    /// Validates finite provider data at the receiving transport boundary.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::List(values) => {
                if values.len() > MAXIMUM_TERMINALS {
                    return Err(PtyError::Capacity);
                }
                for value in values {
                    value.validate()?;
                }
                if values
                    .iter()
                    .enumerate()
                    .any(|(i, value)| values[..i].iter().any(|other| other.id == value.id))
                {
                    return Err(PtyError::Invalid("duplicate terminal identity".into()));
                }
            }
            Self::Attached(value) => {
                value.terminal.validate()?;
                identifier(&value.id)?;
                positive(value.stream_epoch)?;
            }
            Self::Output(value) => {
                value.terminal.validate()?;
                identifier(&value.attachment)?;
                positive(value.stream_epoch)?;
                if value.text.len() > MAXIMUM_OUTPUT_PAGE_BYTES
                    || value.cursor.checked_add(value.text.len() as u64) != Some(value.next_cursor)
                {
                    return Err(PtyError::Invalid(
                        "output cursor or byte bound disagrees".into(),
                    ));
                }
            }
            Self::Input(value) => {
                positive(value.epoch)?;
                positive(value.sequence)?;
                if let InputState::Accepted { bytes } = value.result
                    && (bytes == 0 || bytes > MAXIMUM_INPUT_BYTES)
                {
                    return Err(PtyError::Invalid(
                        "input receipt exceeds its byte bound".into(),
                    ));
                }
            }
            Self::Terminal(value) => value.validate()?,
            Self::Done => {}
        }
        Ok(())
    }
}
impl Terminal {
    /// Revalidates a bounded status reply.
    pub fn validate(&self) -> Result<()> {
        identifier(&self.id)?;
        positive(self.controller_epoch)?;
        self.size.validate()?;
        if let Some(id) = &self.controller {
            identifier(id)?;
        }
        Ok(())
    }
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(PtyError::Invalid("invalid live terminal identity".into()));
    }
    Ok(())
}
/// Trusted live scope, owned by a product service rather than a UI pane.
#[async_trait]
pub trait PtyScope: fmt::Debug + Send + Sync + 'static {
    /// Returns a non-I/O snapshot including in-flight creation and cleanup ownership.
    /// A true result does not fence concurrent creation; callers own that serialization.
    fn is_empty(&self) -> bool;
    /// Creates a shell only from the exact product-authenticated confined plan.
    fn create(&self, spec: rsi_process::PtyProcessSpec) -> Result<Attachment>;
    /// Dispatches one bounded operation; no durable or Session authority is inferred.
    async fn execute(&self, operation: Operation) -> Result<Reply>;
    /// Permanently retires this scope, terminating and reaping every terminal.
    async fn retire(&self) -> Result<()>;
}
/// Ordinary generation-owned provider of independent live scopes.
pub trait PtyProvider: fmt::Debug + Send + Sync + 'static {
    /// Creates one finite scope; possession is trusted process-local authority.
    fn scope(&self) -> Result<Arc<dyn PtyScope>>;
}
/// Typed provider capability; no Session or filesystem authority is serialized.
#[derive(Debug)]
pub struct PtyProviderContract;
impl rsi_meta_contract::LocalContract for PtyProviderContract {
    const KEY: &'static str = "rsi.pty";
    type Service = dyn PtyProvider;
}

impl PtyError {
    /// Validates diagnostic bounds after transport decoding.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Invalid(text) | Self::Unavailable(text) | Self::Io(text) if text.len() > 1024 => {
                Err(Self::Invalid(
                    "terminal diagnostic exceeds its bound".into(),
                ))
            }
            _ => Ok(()),
        }
    }
}

fn positive(value: u64) -> Result<()> {
    if value == 0 {
        Err(PtyError::Invalid(
            "terminal epoch or sequence must be positive".into(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_coordinates_reject_zero_before_live_lookup() {
        for invalid in [
            Operation::Read {
                terminal: "pty".into(),
                attachment: "view".into(),
                stream_epoch: 0,
                cursor: 0,
            },
            Operation::Input {
                terminal: "pty".into(),
                attachment: "view".into(),
                epoch: 0,
                sequence: 1,
                bytes: vec![1],
            },
            Operation::Input {
                terminal: "pty".into(),
                attachment: "view".into(),
                epoch: 1,
                sequence: 0,
                bytes: vec![1],
            },
            Operation::Receipt {
                terminal: "pty".into(),
                epoch: 0,
                sequence: 1,
            },
            Operation::Receipt {
                terminal: "pty".into(),
                epoch: 1,
                sequence: 0,
            },
            Operation::Resize {
                terminal: "pty".into(),
                attachment: "view".into(),
                epoch: 0,
                size: Size {
                    rows: 24,
                    columns: 80,
                },
            },
            Operation::Detach {
                terminal: "pty".into(),
                attachment: "view".into(),
                epoch: 0,
            },
        ] {
            assert!(
                matches!(invalid.validate(), Err(PtyError::Invalid(_))),
                "{invalid:?}"
            );
        }
        assert!(
            Operation::Read {
                terminal: "pty".into(),
                attachment: "view".into(),
                stream_epoch: 1,
                cursor: 0
            }
            .validate()
            .is_ok()
        );
    }
}
