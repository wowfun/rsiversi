use rsi_jobs::JobsError;
use rsi_process::{ManagedProcess, ProcessError, ProcessOutcome, ProcessRead};
use rsi_tools_protocol::{ToolContent, ToolError, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn parse_arguments<T>(arguments: Value) -> std::result::Result<T, Box<ToolResult>>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).map_err(|error| {
        Box::new(
            error_result(
                "invalid_arguments",
                format!("arguments do not match the tool schema: {error}"),
            )
            .expect("bounded static error result is valid"),
        )
    })
}

pub(super) fn foreground_result(
    status: &str,
    managed: &ManagedProcess,
    outcome: &ProcessOutcome,
) -> rsi_tools_protocol::Result<ToolResult> {
    let stdout = managed.stdout().read_from(0).map_err(process_failure)?;
    let stderr = managed.stderr().read_from(0).map_err(process_failure)?;
    foreground_output(status, &stdout, &stderr, outcome)
}

pub(super) fn foreground_output(
    status: &str,
    stdout: &ProcessRead,
    stderr: &ProcessRead,
    outcome: &ProcessOutcome,
) -> rsi_tools_protocol::Result<ToolResult> {
    result_with_text(
        json!({
            "status":status,
            "exit_code":outcome.exit_code,
            "signal":outcome.signal,
            "stdout":process_stream_value(stdout),
            "stderr":process_stream_value(stderr)
        }),
        format!(
            "{}\n[status: {status}; exit code: {}; signal: {}]",
            render_stream_text(stdout, stderr, status),
            outcome
                .exit_code
                .map_or_else(|| "none".into(), |code| code.to_string()),
            outcome
                .signal
                .map_or_else(|| "none".into(), |signal| signal.to_string()),
        ),
        false,
    )
}

fn process_stream_value(read: &ProcessRead) -> Value {
    let text = String::from_utf8_lossy(&read.bytes);
    let utf8_lossy = matches!(text, std::borrow::Cow::Owned(_));
    json!({
        "text":text,
        "oldest_offset":read.oldest_offset,
        "next_offset":read.next_offset,
        "truncated":read.lossy,
        "utf8_lossy":utf8_lossy
        ,"full_output":read.full_output
    })
}

fn render_stream_text(stdout: &ProcessRead, stderr: &ProcessRead, fallback: &str) -> String {
    use std::fmt::Write as _;
    let mut stdout_text = rsi_tools_protocol::safe_tool_text(&stdout.bytes);
    let mut stderr_text = rsi_tools_protocol::safe_tool_text(&stderr.bytes);
    if stdout.lossy {
        stdout_text.insert_str(0, "[stdout truncated; showing retained tail]\n");
        let _ = write!(
            stdout_text,
            "\n[full stdout: {}]",
            stdout.full_output.as_deref().unwrap_or("unavailable")
        );
    }
    if stderr.lossy {
        stderr_text.insert_str(0, "[stderr truncated; showing retained tail]\n");
        let _ = write!(
            stderr_text,
            "\n[full stderr: {}]",
            stderr.full_output.as_deref().unwrap_or("unavailable")
        );
    }
    let stdout = stdout_text;
    let stderr = stderr_text;
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout,
        (true, false) => format!("[stderr]\n{stderr}"),
        (false, false) => format!("{stdout}\n[stderr]\n{stderr}"),
        (true, true) => fallback.to_owned(),
    }
}

pub(super) fn jobs_error_result(error: &JobsError) -> rsi_tools_protocol::Result<ToolResult> {
    let code = match error {
        JobsError::OutcomeUnknown => return Err(ToolError::OutcomeUnknown),
        JobsError::Capacity => "job_capacity",
        JobsError::UnknownProducer(_) => "job_producer_unavailable",
        JobsError::ScopeClosed => "job_scope_closed",
        JobsError::UnknownJob(_) => "unknown_job",
        JobsError::CancellationTimeout => "job_cancellation_timeout",
        JobsError::ShuttingDown => "jobs_shutting_down",
        JobsError::InvalidInput(_) | JobsError::DuplicateProducer(_) | JobsError::Execution(_) => {
            "jobs_error"
        }
    };
    error_result(code, error.to_string())
}

fn process_failure(error: ProcessError) -> ToolError {
    match error {
        ProcessError::OutcomeUnknown => ToolError::OutcomeUnknown,
        other => ToolError::Execution(other.to_string()),
    }
}

pub(super) fn process_error_result(error: &ProcessError) -> rsi_tools_protocol::Result<ToolResult> {
    let code = match error {
        ProcessError::OutcomeUnknown => return Err(ToolError::OutcomeUnknown),
        ProcessError::Capacity => "process_capacity",
        ProcessError::ShuttingDown => "process_shutting_down",
        ProcessError::Unsupported => "process_unsupported",
        ProcessError::SettlementTimeout => "process_settlement_timeout",
        ProcessError::ShutdownTimeout => "process_shutdown_timeout",
        ProcessError::InvalidInput(_)
        | ProcessError::Sandbox(_)
        | ProcessError::Spawn(_)
        | ProcessError::Io(_)
        | ProcessError::Api(_) => "process_error",
    };
    error_result(code, error.to_string())
}

pub(super) fn map_process(error: ProcessError) -> JobsError {
    match error {
        ProcessError::OutcomeUnknown => JobsError::OutcomeUnknown,
        ProcessError::Capacity => JobsError::Capacity,
        ProcessError::ShuttingDown => JobsError::ShuttingDown,
        ProcessError::InvalidInput(message) => JobsError::InvalidInput(message),
        other => JobsError::Execution(other.to_string()),
    }
}

pub(super) fn error_result(
    code: &str,
    message: impl Into<String>,
) -> rsi_tools_protocol::Result<ToolResult> {
    let message = message.into();
    result_with_text(json!({"code":code,"message":message}), message, true)
}

pub(super) fn result_with_text(
    value: Value,
    text: String,
    is_error: bool,
) -> rsi_tools_protocol::Result<ToolResult> {
    ToolResult::new(value, vec![ToolContent::Text { text }], is_error)
}
