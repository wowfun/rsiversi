//! Shared, renderer-independent conversation source semantics.
#![forbid(unsafe_code)]

mod source;
mod window;
pub use source::{FactField, FieldValue, SourceRef, select_field};
pub use window::{FieldWindow, MAXIMUM_WINDOW_BYTES, WindowError};

/// Distinguishes an execution error from a failed process returned by a Tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// The Tool's structured result reports no failure.
    Completed,
    /// The Tool itself reported an error.
    ToolFailed,
    /// The returned process exited unsuccessfully or was killed by a signal.
    ProcessFailed,
}
impl ToolOutcome {
    /// Classifies the complete validated outcome without parsing rendered text.
    pub fn from_result(result: &rsi_tools_protocol::ToolResult) -> Self {
        if result.is_error {
            Self::ToolFailed
        } else if result
            .value
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|code| code != 0)
            || result
                .value
                .get("signal")
                .is_some_and(|value| !value.is_null())
        {
            Self::ProcessFailed
        } else {
            Self::Completed
        }
    }
}
