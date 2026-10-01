//! Bounded helper execution DTOs. Decoding data never creates an execution lease.

use crate::frame::{FrameError, Result};
use rsi_sandbox::{EnforcementStamp, ProcessRequest, ProcessStdio, SandboxMode};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;

/// Exact process preparation before approval or child creation.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    /// Explicit read-only host-scratch and isolated-network view; never inferred from argv.
    pub source_reader: bool,
    /// Exact requested effect mode; target enforcement remains authoritative.
    pub mode: SandboxMode,
    /// Whether the native plan requires a controlling terminal.
    pub pty: bool,
    /// Target-resolved absolute executable.
    pub program: String,
    /// Exact argv, excluding argv[0].
    pub arguments: Vec<String>,
    /// Target-canonical working directory candidate.
    pub cwd: String,
    /// Target-canonical workspace candidate.
    pub workspace: String,
    /// Complete target-selected environment. Values are opaque and never logged.
    pub environment: Vec<(String, String)>,
}
impl std::fmt::Debug for Preparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Preparation")
            .field("mode", &self.mode)
            .field("pty", &self.pty)
            .field("program", &self.program)
            .field("argument_count", &self.arguments.len())
            .field("environment_entries", &self.environment.len())
            .finish_non_exhaustive()
    }
}
impl Preparation {
    /// Validates all external values before native path resolution or allocation.
    pub fn validate(&self) -> Result<()> {
        if self.source_reader && (self.mode != SandboxMode::ReadOnly || self.pty) {
            return Err(FrameError::Invalid);
        }
        for path in [&self.program, &self.cwd, &self.workspace] {
            validate_path(path)?;
        }
        if self.arguments.len() > rsi_sandbox::MAXIMUM_SANDBOX_ARGUMENTS {
            return Err(FrameError::Capacity);
        }
        let mut bytes = self.program.len() + self.cwd.len() + self.workspace.len();
        for argument in &self.arguments {
            if argument.contains('\0') {
                return Err(FrameError::Invalid);
            }
            bytes = bytes
                .checked_add(argument.len())
                .ok_or(FrameError::Capacity)?;
        }
        if bytes > rsi_sandbox::MAXIMUM_SANDBOX_PLAN_BYTES {
            return Err(FrameError::Capacity);
        }
        validate_environment(&self.environment)
    }
    /// Consumes checked wire values into native target inputs, never Service paths.
    pub fn into_native(self) -> Result<(ProcessRequest, Vec<(OsString, OsString)>)> {
        self.validate()?;
        Ok((
            ProcessRequest {
                stdio: if self.pty {
                    ProcessStdio::Pty
                } else {
                    ProcessStdio::Pipes
                },
                mode: self.mode,
                program: self.program.into(),
                arguments: self.arguments,
                cwd: self.cwd.into(),
                workspace: self.workspace.into(),
            },
            self.environment
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        ))
    }
}

/// Complete finite selected executable and child environment.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    /// Absolute executable in the target namespace.
    pub program: String,
    /// Complete environment from target policy and explicitly authorized values.
    pub environment: Vec<(String, String)>,
}
impl std::fmt::Debug for Program {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Program")
            .field("program", &self.program)
            .field("environment_entries", &self.environment.len())
            .finish_non_exhaustive()
    }
}
impl Program {
    /// Validates a target's untrusted resolution reply before retaining it.
    pub fn validate(&self) -> Result<()> {
        validate_path(&self.program)?;
        validate_environment(&self.environment)
    }
}

/// Native start options, independent of the move-only prepared plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StartOptions {
    /// Batch stdin uploads finish before native batch admission.
    Batch {
        /// Exact uploaded input length, including zero.
        stdin_bytes: usize,
        /// Native stdout tail reservation.
        stdout_max_bytes: usize,
        /// Native stderr tail reservation.
        stderr_max_bytes: usize,
        /// Native TERM-to-KILL grace.
        termination_grace_ms: u64,
    },
    /// Ongoing lossless protocol stdout with a bounded stderr tail.
    Duplex {
        /// Native lossless stdout buffer reservation.
        stdout_buffer_bytes: usize,
        /// Native stderr tail reservation.
        stderr_max_bytes: usize,
        /// Native TERM-to-KILL grace.
        termination_grace_ms: u64,
    },
    /// Native terminal, using the Process owner's size contract.
    Pty {
        /// Initial character columns.
        columns: u16,
        /// Initial character rows.
        rows: u16,
        /// Native TERM-to-KILL grace.
        termination_grace_ms: u64,
    },
}
impl StartOptions {
    /// Checks wire options before reserving target process or stream capacity.
    pub fn validate(&self, pty: bool) -> Result<()> {
        let grace = match *self {
            Self::Batch {
                stdin_bytes,
                stdout_max_bytes,
                stderr_max_bytes,
                termination_grace_ms,
            } => {
                if pty || stdin_bytes > rsi_process::MAXIMUM_PROCESS_STDIN_BYTES {
                    return Err(FrameError::Invalid);
                }
                captures(stdout_max_bytes, stderr_max_bytes)?;
                termination_grace_ms
            }
            Self::Duplex {
                stdout_buffer_bytes,
                stderr_max_bytes,
                termination_grace_ms,
            } => {
                if pty {
                    return Err(FrameError::Invalid);
                }
                captures(stdout_buffer_bytes, stderr_max_bytes)?;
                termination_grace_ms
            }
            Self::Pty {
                columns,
                rows,
                termination_grace_ms,
            } => {
                if !pty || (rsi_process::PtySize { columns, rows }).validate().is_err() {
                    return Err(FrameError::Invalid);
                }
                termination_grace_ms
            }
        };
        if !(1..=rsi_process::MAXIMUM_PROCESS_GRACE_MS).contains(&grace) {
            return Err(FrameError::Invalid);
        }
        Ok(())
    }
}

/// Prepared target plan and reserved connection-local stream identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    /// Opaque target plan handle; only its issuing connection may consume it.
    pub handle: u64,
    /// Native enforcement evidence, checked against the exact preparation.
    pub enforcement: EnforcementStamp,
    /// Batch upload or ongoing stdin stream.
    pub stdin: u64,
    /// Stdout or combined PTY output stream.
    pub stdout: u64,
    /// Separate stderr tail stream; absent for a terminal.
    pub stderr: Option<u64>,
}
impl Prepared {
    /// Validates the target reply against the requested policy and stream shape.
    pub fn validate_for(&self, preparation: &Preparation) -> Result<()> {
        self.enforcement
            .validate()
            .map_err(|_| FrameError::Invalid)?;
        if self.handle == 0
            || self.enforcement.requested != preparation.mode
            || self.enforcement.workspace.to_str() != Some(preparation.workspace.as_str())
            || self.stderr.is_some() == preparation.pty
            || (preparation.pty
                && (self.enforcement.scratch != rsi_sandbox::SandboxScratch::PrivateTmp
                    || self.enforcement.network != rsi_sandbox::SandboxNetwork::Host
                    || !matches!(
                        self.enforcement.backend,
                        rsi_sandbox::SandboxBackend::Bubblewrap { .. }
                    )
                    || !matches!(
                        preparation.mode,
                        SandboxMode::ReadOnly | SandboxMode::WorkspaceWrite
                    )))
        {
            return Err(FrameError::Invalid);
        }
        let streams = [Some(self.stdin), Some(self.stdout), self.stderr];
        let mut slots = std::collections::BTreeSet::new();
        for id in streams.into_iter().flatten() {
            if id >> 6 == 0 || !slots.insert(id & 63) {
                return Err(FrameError::Invalid);
            }
        }
        Ok(())
    }
}

/// Validates normalized POSIX path data without any native filesystem effects.
pub fn validate_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || !rsi_workspace_path::is_normalized_absolute(path) {
        return Err(FrameError::Invalid);
    }
    Ok(())
}
/// Validates finite selectors; they are catalog data rather than shell expressions.
pub fn validate_selector(selector: &str) -> Result<()> {
    if selector.is_empty() || selector.len() > 128 || selector.chars().any(char::is_control) {
        return Err(FrameError::Invalid);
    }
    Ok(())
}
fn validate_environment(environment: &[(String, String)]) -> Result<()> {
    if environment.len() > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_ENTRIES {
        return Err(FrameError::Capacity);
    }
    let mut bytes = 0usize;
    for (key, value) in environment {
        bytes = bytes
            .checked_add(key.len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .and_then(|bytes| bytes.checked_add(2))
            .ok_or(FrameError::Capacity)?;
        if bytes > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
            return Err(FrameError::Capacity);
        }
        if key == "NOTIFY_SOCKET"
            || key.starts_with("LD_")
            || key.starts_with("WATCHDOG_")
            || key.starts_with("DBUS_")
            || matches!(key.as_str(), "SSH_AUTH_SOCK" | "SSH_AGENT_PID")
        {
            return Err(FrameError::Invalid);
        }
    }
    let native = environment
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect::<Vec<_>>();
    rsi_process::validate_environment(&native).map_err(|_| FrameError::Invalid)
}
fn captures(stdout: usize, stderr: usize) -> Result<()> {
    if [stdout, stderr]
        .into_iter()
        .any(|bytes| !(1..=rsi_process::MAXIMUM_PROCESS_STREAM_BYTES).contains(&bytes))
    {
        return Err(FrameError::Invalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn preparation() -> Preparation {
        Preparation {
            source_reader: false,
            mode: SandboxMode::ReadOnly,
            pty: false,
            program: "/usr/bin/node".into(),
            arguments: vec!["--version".into()],
            cwd: "/workspace".into(),
            workspace: "/workspace".into(),
            environment: vec![
                ("HOME".into(), "/target-home".into()),
                ("TOKEN".into(), "private-test-value".into()),
            ],
        }
    }
    #[test]
    fn source_reader_choice_cannot_request_write_access_or_a_terminal() {
        let mut request = preparation();
        request.source_reader = true;
        request.validate().unwrap();
        request.pty = true;
        assert!(request.validate().is_err());
        request.pty = false;
        for mode in [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess] {
            request.mode = mode;
            assert!(request.validate().is_err());
        }
    }
    #[test]
    fn process_preparation_rejects_foreign_paths_invalid_environment_and_lifecycle_exports() {
        let request = preparation();
        request.validate().unwrap();
        assert!(!format!("{request:?}").contains("private-test-value"));
        for path in [
            "relative",
            r"C:\workspace",
            "/workspace/../escape",
            "/workspace//nested",
        ] {
            let mut bad = request.clone();
            bad.cwd = path.into();
            assert!(bad.validate().is_err());
        }
        for key in [
            "NOTIFY_SOCKET",
            "WATCHDOG_PID",
            "WATCHDOG_USEC",
            "DBUS_SESSION_BUS_ADDRESS",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
            "BAD=NAME",
        ] {
            let mut bad = request.clone();
            bad.environment.push((key.into(), "value".into()));
            assert!(bad.validate().is_err());
        }
        let mut duplicate = request.clone();
        duplicate.environment.push(("HOME".into(), "/other".into()));
        assert!(duplicate.validate().is_err());
        let (native, env) = request.into_native().unwrap();
        assert_eq!(native.program, std::path::Path::new("/usr/bin/node"));
        assert_eq!(
            env[0],
            (OsString::from("HOME"), OsString::from("/target-home"))
        );
    }
    #[test]
    fn start_options_preserve_large_batch_stdin_and_reject_mismatched_terminal_plans() {
        let batch = StartOptions::Batch {
            stdin_bytes: rsi_process::MAXIMUM_PROCESS_STDIN_BYTES,
            stdout_max_bytes: 4096,
            stderr_max_bytes: 4096,
            termination_grace_ms: 2000,
        };
        batch.validate(false).unwrap();
        assert!(batch.validate(true).is_err());
        let pty = StartOptions::Pty {
            columns: 500,
            rows: 200,
            termination_grace_ms: 2000,
        };
        pty.validate(true).unwrap();
        assert!(pty.validate(false).is_err());
        assert!(
            StartOptions::Pty {
                columns: 501,
                rows: 200,
                termination_grace_ms: 2000
            }
            .validate(true)
            .is_err()
        );
    }
}
