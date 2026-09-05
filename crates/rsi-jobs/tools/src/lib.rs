//! Model-facing control tools for process-local Jobs.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_jobs::{
    JobOutputRead, JobRead, Jobs, JobsContract, JobsError, MAXIMUM_JOB_IDENTIFIER_BYTES,
    MAXIMUM_JOBS_PER_LIST,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_JOB_OUTPUT_TIMEOUT_MS: u64 = 120_000;
const MAXIMUM_JOB_OUTPUT_TIMEOUT_MS: u64 = 570_000;
const JOB_OUTPUT_TOOL_TIMEOUT_MS: u64 = 600_000;
const MAXIMUM_MODEL_STREAM_BYTES: usize = 128 * 1024;

/// Ordinary factory for the generic Jobs control-tool batch.
#[derive(Clone, Debug, Default)]
pub struct JobsToolsFactory;

#[async_trait]
impl PluginFactory for JobsToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Jobs tools configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(Value::Null, (), 0)
            .requiring_local::<ToolRegistrarContract>()
            .requiring_local::<JobsContract>())
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let registrar = plan.local::<ToolRegistrarContract>()?;
        let jobs = plan.local::<JobsContract>()?;
        let registrations = registrations(jobs).map_err(|error| {
            MetaError::Activation(format!("invalid Jobs tool definition: {error}"))
        })?;
        let lease = registrar
            .register_batch(registrations)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release Jobs Tool contribution lease",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}

fn registrations(jobs: Arc<dyn Jobs>) -> rsi_tools_protocol::Result<Vec<ToolRegistration>> {
    Ok(vec![
        ToolRegistration {
            definition: ToolDefinition::new(
                "job_output",
                "Read both retained output streams for one background job. Active reads do not report completion; a terminal read or successful wait does.",
                json!({
                    "type":"object",
                    "properties":{
                        "job_id":{"type":"string","maxLength":MAXIMUM_JOB_IDENTIFIER_BYTES},
                        "wait":{"type":"boolean"},
                        "timeout_ms":{"type":"integer","minimum":1,"maximum":MAXIMUM_JOB_OUTPUT_TIMEOUT_MS}
                    },
                    "required":["job_id"],
                    "additionalProperties":false
                }),
            )?,
            timeout_ms: JOB_OUTPUT_TOOL_TIMEOUT_MS,
            executor: Arc::new(JobOutputTool {
                jobs: Arc::clone(&jobs),
            }),
        },
        ToolRegistration {
            definition: ToolDefinition::new(
                "job_list",
                "List background jobs in the current turn scope. Terminal jobs with reported=false still require job_output or job_kill before successful turn completion.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            )?
            .with_scheduling(ToolScheduling::ParallelSafe),
            timeout_ms: 30_000,
            executor: Arc::new(JobListTool {
                jobs: Arc::clone(&jobs),
            }),
        },
        ToolRegistration {
            definition: ToolDefinition::new(
                "job_kill",
                "Terminate one background job, wait for settlement, and report its final retained output.",
                json!({
                    "type":"object",
                    "properties":{"job_id":{"type":"string","maxLength":MAXIMUM_JOB_IDENTIFIER_BYTES}},
                    "required":["job_id"],
                    "additionalProperties":false
                }),
            )?,
            timeout_ms: 30_000,
            executor: Arc::new(JobKillTool { jobs }),
        },
    ])
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobOutputArguments {
    job_id: String,
    #[serde(default)]
    wait: bool,
    timeout_ms: Option<u64>,
}

#[derive(Debug)]
struct JobOutputTool {
    jobs: Arc<dyn Jobs>,
}

#[async_trait]
impl ToolExecutor for JobOutputTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: JobOutputArguments = match parse_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(result) => return Ok(*result),
        };
        if arguments.timeout_ms.is_some() && !arguments.wait {
            return error_result(
                "invalid_arguments",
                "timeout_ms requires wait=true for job_output",
            );
        }
        let timeout = arguments
            .timeout_ms
            .unwrap_or(DEFAULT_JOB_OUTPUT_TIMEOUT_MS);
        if timeout == 0 || timeout > MAXIMUM_JOB_OUTPUT_TIMEOUT_MS {
            return error_result(
                "invalid_arguments",
                format!("timeout_ms must be within 1..={MAXIMUM_JOB_OUTPUT_TIMEOUT_MS}"),
            );
        }
        let Some(scope) = execution.job_scope() else {
            return error_result(
                "missing_job_scope",
                "job_output requires live turn-scoped Jobs authority",
            );
        };
        if arguments.wait {
            let waiting = tokio::time::timeout(
                Duration::from_millis(timeout),
                self.jobs.wait(scope, &arguments.job_id, 0, 0),
            );
            tokio::pin!(waiting);
            let outcome = tokio::select! {
                biased;
                () = execution.cancellation.cancelled() => return Err(ToolError::Cancelled),
                outcome = &mut waiting => outcome,
            };
            match outcome {
                Ok(Ok(read)) => return job_read_result(&read, false),
                Ok(Err(error)) => return jobs_error_result(&error),
                Err(_) => {}
            }
        }
        match self.jobs.read(scope, &arguments.job_id, 0, 0) {
            Ok(read) => {
                let wait_timed_out = arguments.wait && read.job.terminal.is_none();
                job_read_result(&read, wait_timed_out)
            }
            Err(error) => jobs_error_result(&error),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(Debug)]
struct JobListTool {
    jobs: Arc<dyn Jobs>,
}

#[async_trait]
impl ToolExecutor for JobListTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        if let Err(result) = parse_arguments::<EmptyArguments>(arguments) {
            return Ok(*result);
        }
        let Some(scope) = execution.job_scope() else {
            return error_result(
                "missing_job_scope",
                "job_list requires live turn-scoped Jobs authority",
            );
        };
        match self.jobs.list(scope) {
            Ok(jobs) if jobs.len() <= MAXIMUM_JOBS_PER_LIST => result_with_text(
                json!({"jobs":jobs}),
                serde_json::to_string_pretty(&jobs)
                    .unwrap_or_else(|_| "Jobs list serialization failed".into()),
                false,
            ),
            Ok(_) => error_result(
                "jobs_response_too_large",
                format!("Jobs provider exceeded the {MAXIMUM_JOBS_PER_LIST}-record list contract"),
            ),
            Err(error) => jobs_error_result(&error),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobIdentityArguments {
    job_id: String,
}

#[derive(Debug)]
struct JobKillTool {
    jobs: Arc<dyn Jobs>,
}

#[async_trait]
impl ToolExecutor for JobKillTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: JobIdentityArguments = match parse_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(result) => return Ok(*result),
        };
        let Some(scope) = execution.job_scope() else {
            return error_result(
                "missing_job_scope",
                "job_kill requires live turn-scoped Jobs authority",
            );
        };
        let killing = self.jobs.kill(scope, &arguments.job_id);
        tokio::pin!(killing);
        let outcome = tokio::select! {
            biased;
            () = execution.cancellation.cancelled() => return Err(ToolError::Cancelled),
            outcome = &mut killing => outcome,
        };
        match outcome {
            Ok(read) => job_read_result(&read, false),
            Err(error) => jobs_error_result(&error),
        }
    }
}

fn parse_arguments<T>(arguments: Value) -> std::result::Result<T, Box<ToolResult>>
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

fn job_read_result(read: &JobRead, wait_timed_out: bool) -> rsi_tools_protocol::Result<ToolResult> {
    let stdout = project_stream(&read.stdout);
    let stderr = project_stream(&read.stderr);
    let value = json!({
        "id":read.job.id,
        "name":read.job.name,
        "producer":read.job.producer,
        "status":read.job.status,
        "requires_report":read.job.requires_report,
        "reported":read.job.reported,
        "terminal":read.job.terminal,
        "wait_timed_out":wait_timed_out,
        "stdout":job_stream_value(&stdout),
        "stderr":job_stream_value(&stderr)
    });
    result_with_text(
        value,
        render_stream_text(
            &stdout,
            &stderr,
            if wait_timed_out {
                "wait timed out; job remains active"
            } else {
                "job output"
            },
        ),
        false,
    )
}

fn project_stream(read: &JobOutputRead) -> JobOutputRead {
    let start = read.bytes.len().saturating_sub(MAXIMUM_MODEL_STREAM_BYTES);
    let bytes = read.bytes[start..].to_vec();
    let projection_truncated = start != 0;
    JobOutputRead {
        oldest_offset: if projection_truncated {
            read.next_offset.saturating_sub(bytes.len() as u64)
        } else {
            read.oldest_offset
        },
        next_offset: read.next_offset,
        lossy: read.lossy || projection_truncated,
        bytes,
    }
}

fn job_stream_value(read: &JobOutputRead) -> Value {
    let text = String::from_utf8_lossy(&read.bytes);
    let utf8_lossy = matches!(text, std::borrow::Cow::Owned(_));
    json!({
        "text":text,
        "oldest_offset":read.oldest_offset,
        "next_offset":read.next_offset,
        "truncated":read.lossy,
        "utf8_lossy":utf8_lossy
    })
}

fn render_stream_text(stdout: &JobOutputRead, stderr: &JobOutputRead, fallback: &str) -> String {
    let stdout_text = safe_model_text(&stdout.bytes);
    let stderr_text = safe_model_text(&stderr.bytes);
    let stdout = render_one_stream("stdout", stdout, &stdout_text);
    let stderr = render_one_stream("stderr", stderr, &stderr_text);
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout,
        (true, false) => format!("[stderr]\n{stderr}"),
        (false, false) => format!("{stdout}\n[stderr]\n{stderr}"),
        (true, true) => fallback.to_owned(),
    }
}

fn render_one_stream(kind: &str, read: &JobOutputRead, text: &str) -> String {
    if read.lossy {
        format!(
            "[{kind} truncated; showing bytes {}..{}]\n{text}",
            read.oldest_offset, read.next_offset
        )
    } else {
        text.to_owned()
    }
}

fn safe_model_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|character| {
            if (character.is_ascii_control() && !matches!(character, '\t' | '\n' | '\r'))
                || character == '\u{7f}'
            {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn jobs_error_result(error: &JobsError) -> rsi_tools_protocol::Result<ToolResult> {
    let code = match error {
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

fn error_result(code: &str, message: impl Into<String>) -> rsi_tools_protocol::Result<ToolResult> {
    let message = message.into();
    result_with_text(json!({"code":code,"message":message}), message, true)
}

fn result_with_text(
    value: Value,
    text: String,
    is_error: bool,
) -> rsi_tools_protocol::Result<ToolResult> {
    ToolResult::new(value, vec![ToolContent::Text { text }], is_error)
}
