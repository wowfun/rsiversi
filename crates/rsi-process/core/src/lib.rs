//! Runtime-independent bounded managed-process contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use rsi_sandbox::{ConfinedProcess, MAXIMUM_SANDBOX_ARGUMENTS, MAXIMUM_SANDBOX_PLAN_BYTES};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Maximum captured bytes retained for either stdout or stderr.
pub const MAXIMUM_PROCESS_STREAM_BYTES: usize = 4 * 1024 * 1024;
/// Maximum explicit batch stdin bytes.
pub const MAXIMUM_PROCESS_STDIN_BYTES: usize = 4 * 1024 * 1024;
/// Maximum explicit child environment entries.
pub const MAXIMUM_PROCESS_ENVIRONMENT_ENTRIES: usize = 4_096;
/// Maximum aggregate encoded bytes retained by one explicit child environment.
pub const MAXIMUM_PROCESS_ENVIRONMENT_BYTES: usize = 1024 * 1024;
/// Maximum provider-wide simultaneous live processes.
pub const MAXIMUM_ACTIVE_PROCESSES: usize = 256;
/// Maximum provider-wide capture reservation.
pub const MAXIMUM_PROCESS_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum TERM-to-KILL grace in milliseconds.
pub const MAXIMUM_PROCESS_GRACE_MS: u64 = 60_000;

/// Fully specified managed-process request after Sandbox confinement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    /// Exact confined executable, argv, cwd, and enforcement evidence.
    pub process: ConfinedProcess,
    /// Exact batch bytes written to stdin before it is closed.
    pub stdin: Vec<u8>,
    /// Complete child environment; providers must not merge ambient state.
    pub environment: Vec<(OsString, OsString)>,
    /// Retained stdout tail bytes.
    pub stdout_max_bytes: usize,
    /// Retained stderr tail bytes.
    pub stderr_max_bytes: usize,
    /// TERM-to-KILL escalation grace.
    pub termination_grace_ms: u64,
}

impl ProcessSpec {
    /// Validates platform-neutral request bounds before spawn admission.
    pub fn validate(&self) -> Result<()> {
        self.validate_process_plan()?;
        if self.stdin.len() > MAXIMUM_PROCESS_STDIN_BYTES {
            return Err(ProcessError::InvalidInput(format!(
                "process stdin exceeds {MAXIMUM_PROCESS_STDIN_BYTES} bytes"
            )));
        }
        self.validate_environment()?;
        for (name, value) in [
            ("stdout_max_bytes", self.stdout_max_bytes),
            ("stderr_max_bytes", self.stderr_max_bytes),
        ] {
            if value == 0 || value > MAXIMUM_PROCESS_STREAM_BYTES {
                return Err(ProcessError::InvalidInput(format!(
                    "{name} must be within 1..={MAXIMUM_PROCESS_STREAM_BYTES}"
                )));
            }
        }
        if self.termination_grace_ms == 0 || self.termination_grace_ms > MAXIMUM_PROCESS_GRACE_MS {
            return Err(ProcessError::InvalidInput(format!(
                "termination_grace_ms must be within 1..={MAXIMUM_PROCESS_GRACE_MS}"
            )));
        }
        Ok(())
    }

    fn validate_process_plan(&self) -> Result<()> {
        if self
            .process
            .program
            .as_os_str()
            .as_encoded_bytes()
            .contains(&b'\0')
            || self
                .process
                .cwd
                .as_os_str()
                .as_encoded_bytes()
                .contains(&b'\0')
        {
            return Err(ProcessError::InvalidInput(
                "process executable or working directory contains an invalid byte".into(),
            ));
        }
        if self.process.arguments.len() > MAXIMUM_SANDBOX_ARGUMENTS {
            return Err(ProcessError::InvalidInput(format!(
                "process argument count exceeds {MAXIMUM_SANDBOX_ARGUMENTS}"
            )));
        }
        let mut process_plan_bytes = self
            .process
            .program
            .as_os_str()
            .len()
            .checked_add(self.process.cwd.as_os_str().len())
            .and_then(|bytes| bytes.checked_add(self.process.stamp.workspace.as_os_str().len()))
            .ok_or_else(|| {
                ProcessError::InvalidInput("process plan byte count overflowed".into())
            })?;
        for argument in &self.process.arguments {
            if argument.as_encoded_bytes().contains(&b'\0') {
                return Err(ProcessError::InvalidInput(
                    "process arguments contain an invalid byte".into(),
                ));
            }
            process_plan_bytes =
                process_plan_bytes
                    .checked_add(argument.len())
                    .ok_or_else(|| {
                        ProcessError::InvalidInput("process plan byte count overflowed".into())
                    })?;
        }
        if process_plan_bytes > MAXIMUM_SANDBOX_PLAN_BYTES {
            return Err(ProcessError::InvalidInput(format!(
                "process plan exceeds {MAXIMUM_SANDBOX_PLAN_BYTES} bytes"
            )));
        }
        Ok(())
    }

    fn validate_environment(&self) -> Result<()> {
        if self.environment.len() > MAXIMUM_PROCESS_ENVIRONMENT_ENTRIES {
            return Err(ProcessError::InvalidInput(format!(
                "process environment exceeds {MAXIMUM_PROCESS_ENVIRONMENT_ENTRIES} entries"
            )));
        }
        let mut environment_bytes = 0_usize;
        let mut names = BTreeSet::new();
        for (name, value) in &self.environment {
            if name.is_empty() {
                return Err(ProcessError::InvalidInput(
                    "process environment names must be nonempty".into(),
                ));
            }
            if name.as_encoded_bytes().contains(&b'=')
                || name.as_encoded_bytes().contains(&b'\0')
                || value.as_encoded_bytes().contains(&b'\0')
            {
                return Err(ProcessError::InvalidInput(
                    "process environment contains an invalid byte".into(),
                ));
            }
            environment_bytes = environment_bytes
                .checked_add(name.len())
                .and_then(|bytes| bytes.checked_add(value.len()))
                .and_then(|bytes| bytes.checked_add(2))
                .ok_or_else(|| {
                    ProcessError::InvalidInput("process environment byte count overflowed".into())
                })?;
            if environment_bytes > MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
                return Err(ProcessError::InvalidInput(format!(
                    "process environment exceeds {MAXIMUM_PROCESS_ENVIRONMENT_BYTES} bytes"
                )));
            }
            if !names.insert(name) {
                return Err(ProcessError::InvalidInput(
                    "process environment names must be unique".into(),
                ));
            }
        }
        Ok(())
    }

    /// Returns the aggregate capture reservation for this request.
    pub fn capture_bytes(&self) -> Result<usize> {
        self.stdout_max_bytes
            .checked_add(self.stderr_max_bytes)
            .ok_or_else(|| ProcessError::InvalidInput("capture reservation overflow".into()))
    }
}

/// One raw offset-based output read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRead {
    /// Retained bytes at or after the requested whole-stream offset.
    pub bytes: Vec<u8>,
    /// Oldest whole-stream offset still retained.
    pub oldest_offset: u64,
    /// Whole-stream offset immediately after the returned/current stream tail.
    pub next_offset: u64,
    /// Whether bytes between the requested offset and retained window were lost.
    pub lossy: bool,
}

/// Cursor-free reader for one retained raw process stream.
pub trait ProcessOutput: fmt::Debug + Send + Sync + 'static {
    /// Reads retained bytes using whole-stream byte coordinates.
    fn read_from(&self, offset: u64) -> Result<ProcessRead>;
}

/// Direct-child exit facts without caller-owned timeout classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutcome {
    /// Exit code when the process exited normally.
    pub exit_code: Option<i32>,
    /// Platform signal number when a signal terminated the process.
    pub signal: Option<i32>,
}

/// Provider-owned implementation behind one managed process handle.
#[async_trait]
pub trait ProcessControl: fmt::Debug + Send + Sync + 'static {
    /// Returns the direct child/group-leader process id.
    fn pid(&self) -> u32;
    /// Returns the stdout tail reader.
    fn stdout(&self) -> Arc<dyn ProcessOutput>;
    /// Returns the stderr tail reader.
    fn stderr(&self) -> Arc<dyn ProcessOutput>;
    /// Starts the idempotent TERM-to-KILL group escalation.
    fn terminate(&self);
    /// Waits for the direct child outcome after both captured pipes settle or hit their drain bound.
    async fn wait(&self) -> Result<ProcessOutcome>;
}

/// Cloneable ownership handle for one exact managed process and retained capture.
#[derive(Clone)]
pub struct ManagedProcess {
    control: Arc<dyn ProcessControl>,
}

impl ManagedProcess {
    /// Creates a handle from one provider-owned control object.
    pub fn new(control: Arc<dyn ProcessControl>) -> Self {
        Self { control }
    }

    /// Returns the direct child/group-leader process id.
    pub fn pid(&self) -> u32 {
        self.control.pid()
    }

    /// Returns the stdout tail reader.
    pub fn stdout(&self) -> Arc<dyn ProcessOutput> {
        self.control.stdout()
    }

    /// Returns the stderr tail reader.
    pub fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.control.stderr()
    }

    /// Starts idempotent group termination.
    pub fn terminate(&self) {
        self.control.terminate();
    }

    /// Waits for the direct child outcome.
    pub async fn wait(&self) -> Result<ProcessOutcome> {
        self.control.wait().await
    }
}

impl fmt::Debug for ManagedProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedProcess")
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

/// Bounded managed-process provider.
pub trait Process: fmt::Debug + Send + Sync + 'static {
    /// Spawns one fully specified confined process or rejects before publishing a handle.
    fn spawn(&self, spec: ProcessSpec) -> Result<ManagedProcess>;
}

/// Nominal Local contract for [`Process`].
#[derive(Debug)]
pub struct ProcessContract;

impl LocalContract for ProcessContract {
    const KEY: &'static str = "rsi.process";
    type Service = dyn Process;
}

/// Closed managed-process failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProcessError {
    /// Malformed or out-of-bounds request/read.
    #[error("invalid process request: {0}")]
    InvalidInput(String),
    /// Active or retained-capture capacity is exhausted.
    #[error("process capacity is exhausted")]
    Capacity,
    /// Provider admission closed before the spawn could be published.
    #[error("managed process provider is shutting down")]
    ShuttingDown,
    /// The local platform cannot provide the required lifecycle semantics.
    #[error("managed process execution is unsupported on this platform")]
    Unsupported,
    /// The OS rejected process creation.
    #[error("process spawn failed: {0}")]
    Spawn(String),
    /// An admitted process failed during wait or output drain.
    #[error("process I/O failed: {0}")]
    Io(String),
    /// The direct child was reaped but its terminated process group did not disappear in time.
    #[error("managed process group settlement timed out")]
    SettlementTimeout,
    /// Provider retirement exceeded its finite wait.
    #[error("process shutdown timed out")]
    ShutdownTimeout,
}

/// Managed-process result.
pub type Result<T> = std::result::Result<T, ProcessError>;

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_sandbox::{
        EnforcementStamp, SandboxBackend, SandboxFileSystem, SandboxMode, SandboxNetwork,
        SandboxScratch,
    };

    #[test]
    fn aggregate_environment_bytes_are_bounded_before_spawn() {
        let spec = ProcessSpec {
            process: ConfinedProcess {
                program: "/bin/true".into(),
                arguments: Vec::new(),
                cwd: "/".into(),
                stamp: EnforcementStamp {
                    requested: SandboxMode::DangerFullAccess,
                    backend: SandboxBackend::Unconfined,
                    workspace: "/".into(),
                    filesystem: SandboxFileSystem::Unconfined,
                    scratch: SandboxScratch::Host,
                    network: SandboxNetwork::Host,
                },
            },
            stdin: Vec::new(),
            environment: vec![(
                OsString::from("X"),
                OsString::from("x".repeat(2 * 1024 * 1024)),
            )],
            stdout_max_bytes: 1,
            stderr_max_bytes: 1,
            termination_grace_ms: 1,
        };
        assert!(
            matches!(spec.validate(), Err(ProcessError::InvalidInput(message)) if message.contains("environment"))
        );
    }

    #[test]
    fn environment_names_and_values_reject_process_api_delimiters() {
        for (name, value) in [
            (OsString::from("BAD=NAME"), OsString::from("value")),
            (OsString::from("BAD\0NAME"), OsString::from("value")),
            (OsString::from("NAME"), OsString::from("bad\0value")),
        ] {
            let spec = ProcessSpec {
                process: ConfinedProcess {
                    program: "/bin/true".into(),
                    arguments: Vec::new(),
                    cwd: "/".into(),
                    stamp: EnforcementStamp {
                        requested: SandboxMode::DangerFullAccess,
                        backend: SandboxBackend::Unconfined,
                        workspace: "/".into(),
                        filesystem: SandboxFileSystem::Unconfined,
                        scratch: SandboxScratch::Host,
                        network: SandboxNetwork::Host,
                    },
                },
                stdin: Vec::new(),
                environment: vec![(name, value)],
                stdout_max_bytes: 1,
                stderr_max_bytes: 1,
                termination_grace_ms: 1,
            };
            assert!(
                matches!(spec.validate(), Err(ProcessError::InvalidInput(message)) if message.contains("invalid byte")),
                "accepted an environment entry that process APIs cannot represent: {spec:?}"
            );
        }
    }

    #[test]
    fn confined_argv_is_bounded_again_at_the_process_boundary() {
        let spec_with_too_many_arguments = ProcessSpec {
            process: ConfinedProcess {
                program: "/bin/true".into(),
                arguments: vec![OsString::from("x"); rsi_sandbox::MAXIMUM_SANDBOX_ARGUMENTS + 1],
                cwd: "/".into(),
                stamp: EnforcementStamp {
                    requested: SandboxMode::DangerFullAccess,
                    backend: SandboxBackend::Unconfined,
                    workspace: "/".into(),
                    filesystem: SandboxFileSystem::Unconfined,
                    scratch: SandboxScratch::Host,
                    network: SandboxNetwork::Host,
                },
            },
            stdin: Vec::new(),
            environment: Vec::new(),
            stdout_max_bytes: 1,
            stderr_max_bytes: 1,
            termination_grace_ms: 1,
        };
        assert!(matches!(
            spec_with_too_many_arguments.validate(),
            Err(ProcessError::InvalidInput(message)) if message.contains("argument count")
        ));

        let mut oversized = spec_with_too_many_arguments;
        oversized.process.arguments = vec![OsString::from(
            "x".repeat(rsi_sandbox::MAXIMUM_SANDBOX_PLAN_BYTES + 1),
        )];
        assert!(matches!(
            oversized.validate(),
            Err(ProcessError::InvalidInput(message)) if message.contains("process plan")
        ));
    }

    #[test]
    fn program_and_cwd_reject_process_api_nul() {
        for (program, cwd) in [
            (OsString::from("bad\0program"), OsString::from("/")),
            (OsString::from("/bin/true"), OsString::from("bad\0cwd")),
        ] {
            let spec = ProcessSpec {
                process: ConfinedProcess {
                    program: program.into(),
                    arguments: Vec::new(),
                    cwd: cwd.into(),
                    stamp: EnforcementStamp {
                        requested: SandboxMode::DangerFullAccess,
                        backend: SandboxBackend::Unconfined,
                        workspace: "/".into(),
                        filesystem: SandboxFileSystem::Unconfined,
                        scratch: SandboxScratch::Host,
                        network: SandboxNetwork::Host,
                    },
                },
                stdin: Vec::new(),
                environment: Vec::new(),
                stdout_max_bytes: 1,
                stderr_max_bytes: 1,
                termination_grace_ms: 1,
            };
            assert!(
                matches!(spec.validate(), Err(ProcessError::InvalidInput(message)) if message.contains("invalid byte")),
                "accepted a process path that the platform API cannot represent: {spec:?}"
            );
        }
    }
}
