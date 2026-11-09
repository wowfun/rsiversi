use super::{
    ShellServices,
    jobs::ShellJobRequest,
    output::{
        error_result, foreground_output, foreground_result, jobs_error_result, parse_arguments,
        process_error_result, result_with_text,
    },
};
use crate::{
    DEFAULT_SHELL_TIMEOUT_MS, MAXIMUM_SHELL_COMMAND_BYTES, MAXIMUM_SHELL_TIMEOUT_MS,
    SHELL_STREAM_CAPTURE_BYTES, SHELL_TERMINATION_GRACE_MS, SHELL_TOOL_TIMEOUT_MS,
};
use async_trait::async_trait;
use rsi_jobs::{JobRequest, JobStatus, JobSubmission, JobsError};
use rsi_process::{ProcessError, ProcessOutcome, ProcessRead, ProcessSpec};
use rsi_tools_protocol::{
    ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolProcess, ToolRegistration,
    ToolResult,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub(super) fn registration(
    services: Arc<ShellServices>,
) -> rsi_tools_protocol::Result<ToolRegistration> {
    Ok(ToolRegistration {
        output: None,
        definition: ToolDefinition::new(
            services.kind.name(),
            services.kind.description(),
            json!({
                "type":"object",
                "properties":{
                    "command":{"type":"string","maxLength":MAXIMUM_SHELL_COMMAND_BYTES},
                    "timeout_ms":{"type":"integer","minimum":1,"maximum":MAXIMUM_SHELL_TIMEOUT_MS},
                    "run_in_background":{"type":"boolean"}
                },
                "required":["command"],
                "additionalProperties":false
            }),
        )?
        .with_program_role(rsi_tools_protocol::ToolProgramRole::Callable),
        timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution {
            timeout_ms: SHELL_TOOL_TIMEOUT_MS,
        },
        executor: Arc::new(ShellTool { services }),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArguments {
    command: String,
    timeout_ms: Option<u64>,
    #[serde(default)]
    run_in_background: bool,
}

impl ShellArguments {
    fn invalid_reason(&self) -> Option<String> {
        if self.command.is_empty()
            || self.command.len() > MAXIMUM_SHELL_COMMAND_BYTES
            || self.command.contains('\0')
        {
            Some(format!(
                "command must be nonempty, NUL-free UTF-8 within {MAXIMUM_SHELL_COMMAND_BYTES} bytes"
            ))
        } else if self
            .timeout_ms
            .is_some_and(|timeout| timeout == 0 || timeout > MAXIMUM_SHELL_TIMEOUT_MS)
        {
            Some(format!(
                "timeout_ms must be within 1..={MAXIMUM_SHELL_TIMEOUT_MS}"
            ))
        } else if self.run_in_background && self.timeout_ms.is_some() {
            Some("background shell does not accept timeout_ms; use job_output or job_kill".into())
        } else {
            None
        }
    }
}

fn job_label(mut command: String) -> String {
    let maximum = rsi_jobs::MAXIMUM_JOB_LABEL_BYTES;
    if command.len() > maximum {
        let end = command.floor_char_boundary(maximum - '…'.len_utf8());
        command = format!("{}…", &command[..end]);
    }
    if command.contains(char::is_control) {
        command = command.replace(char::is_control, " ");
    }
    command
}

#[derive(Debug)]
struct ShellTool {
    services: Arc<ShellServices>,
}

#[async_trait]
impl ToolExecutor for ShellTool {
    async fn prepare(
        &self,
        arguments: &Value,
        execution: &ToolExecution,
    ) -> rsi_tools_protocol::Result<Option<ToolProcess>> {
        let Ok(arguments) = serde_json::from_value::<ShellArguments>(arguments.clone()) else {
            return Ok(None);
        };
        if arguments.invalid_reason().is_some() {
            return Ok(None);
        }
        execution
            .prepare_process(
                self.services.kind.name(),
                self.services.program.clone(),
                self.services.environment.to_vec(),
                self.services.kind.arguments(arguments.command),
            )
            .await
            .map(Some)
    }
    #[allow(clippy::too_many_lines)]
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: ShellArguments = match parse_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(result) => return Ok(*result),
        };
        if let Some(reason) = arguments.invalid_reason() {
            return error_result("invalid_arguments", reason);
        }
        let scope = execution.job_scope().cloned();
        if arguments.run_in_background && scope.is_none() {
            return error_result(
                "missing_job_scope",
                "background shell requires live turn-scoped Jobs authority",
            );
        }
        let process = execution.take_prepared_process()?;
        let environment = process.environment().to_vec();
        let spec = ProcessSpec {
            process,
            stdin: Vec::new(),
            environment,
            stdout_max_bytes: SHELL_STREAM_CAPTURE_BYTES,
            stderr_max_bytes: SHELL_STREAM_CAPTURE_BYTES,
            termination_grace_ms: SHELL_TERMINATION_GRACE_MS,
        };
        if let Some(scope) = scope {
            let submission = JobSubmission {
                name: job_label(arguments.command),
                producer: self.services.kind.producer().into(),
                origin: execution
                    .extension::<rsi_jobs::JobOrigin>()
                    .map(|origin| origin.as_str().to_owned()),
                request: JobRequest::new(ShellJobRequest {
                    spec: Mutex::new(Some(spec)),
                }),
                requires_report: true,
            };
            let id = match self.services.jobs.submit(&scope, submission).await {
                Ok(id) => id,
                Err(error) => return jobs_error_result(&error),
            };
            if arguments.run_in_background {
                return result_with_text(
                    json!({"job_id":id,"status":"running"}),
                    format!("Started background shell job {id}."),
                    false,
                );
            }
            let timeout = arguments.timeout_ms.unwrap_or(DEFAULT_SHELL_TIMEOUT_MS);
            let (status, read) = tokio::select! {
                biased;
                read = self.services.jobs.wait(&scope, &id, 0, 0) => ("exited", read),
                () = execution.cancellation.cancelled() => {
                    let settled = self.services.jobs.kill(&scope, &id).await;
                    if matches!(&settled, Ok(read) if read.job.status == JobStatus::OutcomeUnknown) || matches!(settled, Err(JobsError::OutcomeUnknown)) {
                        return Err(ToolError::OutcomeUnknown);
                    }
                    return Err(ToolError::Cancelled);
                }
                () = tokio::time::sleep(Duration::from_millis(timeout)) =>
                    ("timed_out", self.services.jobs.kill(&scope, &id).await),
            };
            let read = match read {
                Ok(read) => read,
                Err(error) => return jobs_error_result(&error),
            };
            if read.job.status == JobStatus::OutcomeUnknown {
                return Err(ToolError::OutcomeUnknown);
            }
            let terminal = read.job.terminal.as_ref().ok_or_else(|| {
                ToolError::Execution("foreground shell lacks terminal status".into())
            })?;
            let outcome = ProcessOutcome {
                exit_code: terminal.exit_code,
                signal: terminal.signal,
            };
            let process_read = |read: rsi_jobs::JobOutputRead| ProcessRead {
                bytes: read.bytes,
                oldest_offset: read.oldest_offset,
                next_offset: read.next_offset,
                lossy: read.lossy,
                full_output: read.full_output,
            };
            return foreground_output(
                status,
                &process_read(read.stdout),
                &process_read(read.stderr),
                &outcome,
            );
        }

        let timeout = arguments.timeout_ms.unwrap_or(DEFAULT_SHELL_TIMEOUT_MS);
        let managed = match ToolProcess::spawn(spec, self.services.process.as_ref()).await {
            Ok(managed) => managed,
            Err(error) => return process_error_result(&error),
        };
        let waiting = managed.clone();
        let sleep = tokio::time::sleep(Duration::from_millis(timeout));
        tokio::pin!(sleep);
        let (status, outcome) = tokio::select! {
            biased;
            outcome = waiting.wait() => ("exited", outcome),
            () = execution.cancellation.cancelled() => {
                managed.terminate();
                if matches!(managed.wait().await, Err(ProcessError::OutcomeUnknown)) {
                    return Err(ToolError::OutcomeUnknown);
                }
                return Err(ToolError::Cancelled);
            }
            () = &mut sleep => {
                managed.terminate();
                ("timed_out", managed.wait().await)
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return process_error_result(&error),
        };
        foreground_result(status, &managed, &outcome)
    }
}
