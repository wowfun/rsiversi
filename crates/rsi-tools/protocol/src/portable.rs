//! Closed duplex protocol for trusted Portable tool contributors.

use crate::{ToolCall, ToolDefinition, ToolError, ToolExecutionPolicy, ToolResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// Exact Portable contract name; service keys are selected by composition.
pub const CONTRACT: &str = "rsi.tools.portable";
/// Contract version.
pub const VERSION: u32 = 1;
/// Maximum encoded bytes of one request or response frame.
pub const MAXIMUM_FRAME_BYTES: usize = 256 * 1024;
/// Maximum process-plan requests during one Tool execution.
pub const MAXIMUM_CONFINE_REQUESTS: usize = 256;

/// Owner-declared execution overlap; never taken from model arguments.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheduling {
    /// Runs alone.
    Exclusive,
    /// Runs alone and must be the last call in a model response.
    ExclusiveFinal,
    /// May overlap adjacent explicitly parallel-safe tools.
    ParallelSafe,
}

/// One declaration in the atomic Describe batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// Bounded model-visible declaration.
    pub definition: ToolDefinition,
    /// Owner-declared cooperative timeout, in milliseconds.
    pub timeout_ms: u64,
    /// Owner-declared overlap policy.
    pub scheduling: Scheduling,
}

/// Host-to-provider messages, in the exact phase order described by the contract.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Reads one immutable batch without granting execution authority.
    Describe {},
    /// Begins an approved invocation with its exact pinned policy.
    Execute {
        /// Canonical model call.
        call: ToolCall,
        /// Orchestrator-owned paths and Sandbox mode.
        policy: ToolExecutionPolicy,
    },
    /// Returns a plan from the invocation's exact Sandbox generation.
    Confined {
        /// Program, arguments and cwd chosen by the host Sandbox.
        plan: ProcessPlan,
    },
}

/// Provider-to-host messages. Result must be followed by clean terminal.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    /// Complete atomic Describe result.
    Description {
        /// At most the Tool catalog's maximum registration count.
        tools: Vec<Definition>,
    },
    /// Requests confinement without permitting path-policy overrides.
    Confine {
        /// Native program path.
        program: PathBuf,
        /// UTF-8 argv, excluding `argv[0]`.
        arguments: Vec<String>,
    },
    /// Complete typed result; enforcement is supplied only by the host.
    Result {
        /// Bounded semantic output, with an empty enforcement field.
        result: ToolResult,
    },
}

/// Lossless same-platform OS string for a native process plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "platform",
    content = "units",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OsValue {
    /// Unix bytes, including non-UTF-8 names.
    Unix(Vec<u8>),
    /// Windows UTF-16 code units, including unpaired surrogates.
    Windows(Vec<u16>),
}

impl OsValue {
    /// Captures an OS string without lossy conversion.
    pub fn capture(value: &OsStr) -> crate::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Ok(Self::Unix(value.as_bytes().to_vec()))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt as _;
            Ok(Self::Windows(value.encode_wide().collect()))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = value;
            Err(invalid())
        }
    }

    /// Restores only an OS string belonging to the current platform.
    pub fn restore(self) -> crate::Result<OsString> {
        match self {
            #[cfg(unix)]
            Self::Unix(bytes) => {
                use std::os::unix::ffi::OsStringExt as _;
                Ok(OsString::from_vec(bytes))
            }
            #[cfg(windows)]
            Self::Windows(units) => {
                use std::os::windows::ffi::OsStringExt as _;
                Ok(OsString::from_wide(&units))
            }
            _ => Err(invalid()),
        }
    }
}

/// Actual host-confined invocation; policy and enforcement remain host-owned.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessPlan {
    /// Program selected by Sandbox.
    pub program: OsValue,
    /// Exact wrapper or target argv.
    pub arguments: Vec<OsValue>,
    /// Host spawn cwd.
    pub cwd: OsValue,
}

/// Encodes without retaining a buffer larger than the wire's own bound.
pub fn encode<T: Serialize>(value: &T) -> crate::Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAXIMUM_FRAME_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            let required = self.0.len() + bytes.len();
            if required > self.0.capacity() {
                self.0.reserve_exact(required - self.0.len());
            }
            self.0.write_all(bytes)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| invalid())?;
    Ok(output.0)
}

/// Checks framing before decoding a closed protocol value.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> crate::Result<T> {
    if bytes.len() > MAXIMUM_FRAME_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let value = crate::parse_tool_arguments(text).map_err(|_| invalid())?;
    serde_json::from_value(value).map_err(|_| invalid())
}

fn invalid() -> ToolError {
    ToolError::InvalidInput("invalid or oversized Portable Tool frame".into())
}
