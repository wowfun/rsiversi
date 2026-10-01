//! Explicit target configuration, independent of any Service environment snapshot.
use crate::{ExecutionCoordinates, ExecutionLocation};
use rsi_process::{ProcessError, Result};
use serde::{Deserialize, Serialize};

/// Operator-selected target executable and explicitly authorized extra environment.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetProgram {
    /// Absolute target path or basename resolved in the target's fixed PATH.
    pub command: String,
    /// Extra values; target account and lifecycle environment cannot be overridden.
    pub environment: Vec<(String, String)>,
}
impl std::fmt::Debug for TargetProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetProgram")
            .field("environment_entries", &self.environment.len())
            .finish_non_exhaustive()
    }
}
impl TargetProgram {
    /// Bounds and validates configuration before resolution or credential export.
    pub fn validate(&self) -> Result<()> {
        let invalid = || ProcessError::InvalidInput("invalid target program policy".into());
        if self.command.starts_with('/') {
            ExecutionCoordinates::new(ExecutionLocation::Local, &self.command)
                .map_err(|_| invalid())?;
        } else if self.command.is_empty()
            || self.command.len() > 255
            || self.command.starts_with('-')
            || !self.command.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+')
            })
            || matches!(self.command.as_str(), "." | "..")
        {
            return Err(invalid());
        }
        if self.environment.len() > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_ENTRIES {
            return Err(ProcessError::Capacity);
        }
        let mut bytes = self.command.len();
        for (key, value) in &self.environment {
            bytes = bytes
                .saturating_add(key.len())
                .saturating_add(value.len())
                .saturating_add(2);
            if bytes > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
                return Err(ProcessError::Capacity);
            }
            if matches!(
                key.as_str(),
                "HOME"
                    | "PATH"
                    | "USER"
                    | "LOGNAME"
                    | "NOTIFY_SOCKET"
                    | "SSH_AUTH_SOCK"
                    | "SSH_AGENT_PID"
            ) || key.starts_with("LD_")
                || key.starts_with("WATCHDOG_")
                || key.starts_with("DBUS_")
            {
                return Err(invalid());
            }
        }
        rsi_process::validate_environment(
            &self
                .environment
                .iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect::<Vec<_>>(),
        )
    }
}
