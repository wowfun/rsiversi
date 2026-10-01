//! Typed engine failures survive RPC, process settlement and cancellation.
use std::fmt;

/// Program engine failure, independent of script-visible JSON replies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramError {
    /// Known script, protocol, admission or execution failure.
    Failed(String),
    /// An admitted effect has no verifiable outcome and must not be replayed.
    OutcomeUnknown,
}
impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(message) => f.write_str(message),
            Self::OutcomeUnknown => f.write_str("program outcome is unknown; do not replay"),
        }
    }
}
impl std::error::Error for ProgramError {}
impl From<String> for ProgramError {
    fn from(value: String) -> Self {
        Self::Failed(value)
    }
}
impl From<&str> for ProgramError {
    fn from(value: &str) -> Self {
        Self::Failed(value.into())
    }
}
impl From<rsi_process::ProcessError> for ProgramError {
    fn from(value: rsi_process::ProcessError) -> Self {
        match value {
            rsi_process::ProcessError::OutcomeUnknown
            | rsi_process::ProcessError::Api(rsi_api_protocol::ApiError::OutcomeUnknown) => {
                Self::OutcomeUnknown
            }
            error => Self::Failed(error.to_string()),
        }
    }
}
impl From<rsi_jobs::JobsError> for ProgramError {
    fn from(value: rsi_jobs::JobsError) -> Self {
        match value {
            rsi_jobs::JobsError::OutcomeUnknown => Self::OutcomeUnknown,
            error => Self::Failed(error.to_string()),
        }
    }
}
impl From<rsi_tools_protocol::ToolError> for ProgramError {
    fn from(value: rsi_tools_protocol::ToolError) -> Self {
        match value {
            rsi_tools_protocol::ToolError::OutcomeUnknown => Self::OutcomeUnknown,
            error => Self::Failed(error.to_string()),
        }
    }
}
impl From<ProgramError> for rsi_tools_protocol::ToolError {
    fn from(value: ProgramError) -> Self {
        match value {
            ProgramError::OutcomeUnknown => Self::OutcomeUnknown,
            ProgramError::Failed(message) => Self::Execution(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_process_uncertainty_survives_program_and_tool_projection() {
        for error in [
            rsi_process::ProcessError::OutcomeUnknown,
            rsi_process::ProcessError::Api(rsi_api_protocol::ApiError::OutcomeUnknown),
        ] {
            assert_eq!(
                rsi_tools_protocol::ToolError::from(ProgramError::from(error)),
                rsi_tools_protocol::ToolError::OutcomeUnknown
            );
        }
    }
}
