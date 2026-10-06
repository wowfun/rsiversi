//! Typed engine failures survive RPC, process settlement and cancellation.
use std::fmt;

/// Program engine failure, independent of script-visible JSON replies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramError {
    /// Known cancellation from a Tool or Turn-service boundary.
    Cancelled,
    /// A Tool or Turn service refused capacity admission.
    Capacity,
    /// A Tool or Turn service rejected the request as invalid.
    InvalidInput(String),
    /// Known script, protocol, admission or execution failure.
    Failed(String),
    /// An admitted effect has no verifiable outcome and must not be replayed.
    OutcomeUnknown,
}
impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("program operation was cancelled"),
            Self::Capacity => f.write_str("program capacity is exhausted"),
            Self::InvalidInput(message) | Self::Failed(message) => f.write_str(message),
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
            rsi_tools_protocol::ToolError::Cancelled => Self::Cancelled,
            rsi_tools_protocol::ToolError::Capacity => Self::Capacity,
            rsi_tools_protocol::ToolError::InvalidInput(message) => Self::InvalidInput(message),
            rsi_tools_protocol::ToolError::OutcomeUnknown => Self::OutcomeUnknown,
            error => Self::Failed(error.to_string()),
        }
    }
}
impl From<rsi_agent_turn_protocol::TurnError> for ProgramError {
    fn from(value: rsi_agent_turn_protocol::TurnError) -> Self {
        match value {
            rsi_agent_turn_protocol::TurnError::Cancelled => Self::Cancelled,
            rsi_agent_turn_protocol::TurnError::Capacity => Self::Capacity,
            rsi_agent_turn_protocol::TurnError::Invalid(message) => Self::InvalidInput(message),
            rsi_agent_turn_protocol::TurnError::ExecutionOutcomeUnknown
            | rsi_agent_turn_protocol::TurnError::DomainOutcomeUnknown { .. } => {
                Self::OutcomeUnknown
            }
            error => Self::Failed(error.to_string()),
        }
    }
}
impl From<ProgramError> for rsi_tools_protocol::ToolError {
    fn from(value: ProgramError) -> Self {
        match value {
            ProgramError::Cancelled => Self::Cancelled,
            ProgramError::Capacity => Self::Capacity,
            ProgramError::InvalidInput(message) => Self::InvalidInput(message),
            ProgramError::OutcomeUnknown => Self::OutcomeUnknown,
            ProgramError::Failed(message) => Self::Execution(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_turn_uncertainty_survives_program_and_tool_projection() {
        use rsi_agent_turn_protocol::TurnError;
        for error in [
            TurnError::ExecutionOutcomeUnknown,
            TurnError::DomainOutcomeUnknown {
                request_id: "exact-request".into(),
            },
        ] {
            assert_eq!(
                rsi_tools_protocol::ToolError::from(ProgramError::from(error)),
                rsi_tools_protocol::ToolError::OutcomeUnknown
            );
        }
    }
    #[test]
    fn known_refusals_survive_setup_projection() {
        use rsi_agent_turn_protocol::TurnError;
        use rsi_tools_protocol::ToolError;
        for (turn, expected) in [
            (TurnError::Cancelled, ToolError::Cancelled),
            (TurnError::Capacity, ToolError::Capacity),
            (
                TurnError::Invalid("known refusal".into()),
                ToolError::InvalidInput("known refusal".into()),
            ),
        ] {
            assert_eq!(ToolError::from(ProgramError::from(turn)), expected);
            assert_eq!(
                ToolError::from(ProgramError::from(expected.clone())),
                expected
            );
        }
    }
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
