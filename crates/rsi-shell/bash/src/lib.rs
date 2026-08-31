//! Linux Bash capability plugins.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_jobs::JobsContract;
#[cfg(target_os = "linux")]
use rsi_jobs::JobsError;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::ProcessContract;
#[cfg(target_os = "linux")]
use rsi_process::ProcessError;
use rsi_tools_protocol::{ToolError, ToolRegistrarContract};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Maximum UTF-8 bytes in one Bash command argument.
pub const MAXIMUM_BASH_COMMAND_BYTES: usize = 96 * 1024;
/// Default foreground Bash timeout.
pub const DEFAULT_BASH_TIMEOUT_MS: u64 = 120_000;
/// Maximum foreground Bash timeout.
pub const MAXIMUM_BASH_TIMEOUT_MS: u64 = 570_000;
/// Retained raw bytes per Bash output stream.
pub const BASH_STREAM_CAPTURE_BYTES: usize = 64_000;
/// TERM-to-KILL grace for a Bash process group.
pub const BASH_TERMINATION_GRACE_MS: u64 = 3_000;
/// Outer timeout for the model-facing Bash tool.
pub const BASH_TOOL_TIMEOUT_MS: u64 = 600_000;

#[cfg(target_os = "linux")]
const BASH_PRODUCER: &str = "rsi.coding.bash";

/// Captures and scrubs an explicit child-environment snapshot.
///
/// Non-UTF-8 names, secret-shaped names, `RSI_*`, and credential-bearing or
/// non-UTF-8 proxy values are dropped. Ordinary raw values remain byte-exact.
pub fn scrub_child_environment<I>(variables: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut retained = Vec::new();
    for (name, value) in variables {
        let Some(name_text) = name.to_str() else {
            continue;
        };
        let uppercase = name_text.to_ascii_uppercase();
        if uppercase.starts_with("RSI_")
            || uppercase.starts_with("LD_")
            || uppercase.starts_with("DYLD_")
            || uppercase.starts_with("BASH_FUNC_")
            || matches!(
                uppercase.as_str(),
                "BASH_ENV"
                    | "ENV"
                    | "SHELLOPTS"
                    | "BASHOPTS"
                    | "GCONV_PATH"
                    | "LOCPATH"
                    | "NLSPATH"
                    | "GLIBC_TUNABLES"
            )
            || ["KEY", "PASSWORD", "SECRET", "TOKEN"]
                .iter()
                .any(|needle| uppercase.contains(needle))
        {
            continue;
        }
        if matches!(
            uppercase.as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"
        ) {
            let Some(proxy) = value.to_str() else {
                continue;
            };
            if proxy_has_userinfo(proxy) {
                continue;
            }
        }
        retained.push((name, value));
    }
    retained.sort_by(|left, right| left.0.cmp(&right.0));
    retained
}

fn proxy_has_userinfo(proxy: &str) -> bool {
    let authority = proxy
        .split_once("://")
        .map_or(proxy, |(_, authority)| authority);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    authority.contains('@')
}

/// Ordinary factory for the stable Bash Job producer.
#[derive(Clone, Debug, Default)]
pub struct BashJobProducerFactory;

/// Ordinary factory for the model-facing Bash tool.
#[derive(Clone, Debug)]
pub struct BashToolFactory {
    bash: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

impl BashToolFactory {
    /// Creates a factory from an explicit absolute Bash path and frozen child environment.
    pub fn new(
        bash: impl Into<PathBuf>,
        environment: Vec<(OsString, OsString)>,
    ) -> rsi_tools_protocol::Result<Self> {
        let bash = canonical_executable("Bash", &bash.into())?;
        let environment = scrub_child_environment(environment);
        let mut seen = std::collections::BTreeSet::new();
        for (name, _) in &environment {
            if name.is_empty() || !seen.insert(name.clone()) {
                return Err(ToolError::InvalidInput(
                    "child environment names must be nonempty and unique".into(),
                ));
            }
        }
        Ok(Self { bash, environment })
    }

    #[cfg(target_os = "linux")]
    fn environment_with_bash_defaults(&self) -> Vec<(OsString, OsString)> {
        let mut environment = self
            .environment
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>();
        for (name, value) in [
            ("NO_COLOR", "1"),
            ("TERM", "dumb"),
            ("PAGER", "cat"),
            ("GIT_PAGER", "cat"),
        ] {
            environment.insert(OsString::from(name), OsString::from(value));
        }
        environment.into_iter().collect()
    }

    fn retained_bytes(&self) -> rsi_meta::Result<usize> {
        self.environment
            .iter()
            .try_fold(self.bash.as_os_str().len(), |bytes, (name, value)| {
                bytes.checked_add(name.len())?.checked_add(value.len())
            })
            .ok_or_else(|| MetaError::InvalidInput("Bash tool retained bytes overflow".into()))
    }
}

fn canonical_executable(kind: &str, path: &Path) -> rsi_tools_protocol::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(ToolError::InvalidInput(format!(
            "{kind} program path must be absolute"
        )));
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|error| ToolError::InvalidInput(format!("{kind} is unavailable: {error}")))?;
    let metadata = resolved
        .metadata()
        .map_err(|error| ToolError::InvalidInput(format!("{kind} is unavailable: {error}")))?;
    if !metadata.is_file() {
        return Err(ToolError::InvalidInput(format!(
            "{kind} path must name a canonical regular executable file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(ToolError::InvalidInput(format!(
                "{kind} path must name a canonical regular executable file"
            )));
        }
    }
    Ok(resolved)
}

#[async_trait]
impl PluginFactory for BashJobProducerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Bash Job producer configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(Value::Null, (), 0)
            .requiring_local::<JobsContract>()
            .requiring_local::<ProcessContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        activate_producer(plan)
    }
}

#[async_trait]
impl PluginFactory for BashToolFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Bash tool configuration must be null".into(),
            ));
        }
        Ok(
            PreparedActivation::with_state(Value::Null, (), self.retained_bytes()?)
                .requiring_local::<ToolRegistrarContract>()
                .requiring_local::<JobsContract>()
                .requiring_local::<ProcessContract>(),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        activate_tool(plan, self)
    }
}

#[cfg(target_os = "linux")]
fn activate_producer(plan: ActivationPlan) -> rsi_meta::Result<()> {
    linux::activate_producer(plan)
}

#[cfg(not(target_os = "linux"))]
fn activate_producer(mut plan: ActivationPlan) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    Err(MetaError::Activation(
        "Bash Jobs require Linux process-group settlement semantics".into(),
    ))
}

#[cfg(target_os = "linux")]
fn activate_tool(plan: ActivationPlan, factory: &BashToolFactory) -> rsi_meta::Result<()> {
    linux::activate_tool(plan, factory)
}

#[cfg(not(target_os = "linux"))]
fn activate_tool(mut plan: ActivationPlan, factory: &BashToolFactory) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    let _ = factory;
    Err(MetaError::Activation(
        "the Bash tool requires Linux process-group settlement semantics".into(),
    ))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        ActivationPlan, BASH_PRODUCER, BASH_STREAM_CAPTURE_BYTES, BASH_TERMINATION_GRACE_MS,
        BASH_TOOL_TIMEOUT_MS, BashToolFactory, DEFAULT_BASH_TIMEOUT_MS, JobsContract, JobsError,
        MAXIMUM_BASH_COMMAND_BYTES, MAXIMUM_BASH_TIMEOUT_MS, MetaError, OsString, PathBuf,
        ProcessContract, ProcessError, ToolError, ToolRegistrarContract, Value,
    };
    use async_trait::async_trait;
    use rsi_jobs::{
        JobControl, JobOutputRead, JobProducer, JobProducerRegistration, JobRequest, JobStatus,
        JobStream, JobSubmission, JobTerminal, Jobs,
    };
    use rsi_process::{ManagedProcess, Process, ProcessOutcome, ProcessRead, ProcessSpec};
    use rsi_tools_protocol::{
        ToolContent, ToolDefinition, ToolExecution, ToolExecutor, ToolRegistration, ToolResult,
    };
    use serde::Deserialize;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    pub(super) fn activate_producer(mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let jobs = plan.local::<JobsContract>()?;
        let process = plan.local::<ProcessContract>()?;
        let lease = jobs
            .register_producer(JobProducerRegistration {
                name: BASH_PRODUCER.into(),
                producer: Arc::new(BashProducer { process }),
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Bash Job producer",
            Box::new(move || {
                Box::pin(async move { lease.retire().await.map_err(|error| error.to_string()) })
            }),
        )
    }

    pub(super) fn activate_tool(
        mut plan: ActivationPlan,
        factory: &BashToolFactory,
    ) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let registrar = plan.local::<ToolRegistrarContract>()?;
        let jobs = plan.local::<JobsContract>()?;
        let process = plan.local::<ProcessContract>()?;
        let services = Arc::new(BashServices {
            bash: factory.bash.clone(),
            environment: factory.environment_with_bash_defaults(),
            process,
            jobs,
        });
        let registration = bash_registration(services).map_err(|error| {
            MetaError::Activation(format!("invalid Bash tool definition: {error}"))
        })?;
        let lease = registrar
            .register_batch(vec![registration])
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release Bash Tool contribution lease",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }

    #[derive(Debug)]
    struct BashServices {
        bash: PathBuf,
        environment: Vec<(OsString, OsString)>,
        process: Arc<dyn Process>,
        jobs: Arc<dyn Jobs>,
    }

    fn bash_registration(
        services: Arc<BashServices>,
    ) -> rsi_tools_protocol::Result<ToolRegistration> {
        Ok(ToolRegistration {
            definition: ToolDefinition::new(
                "bash",
                "Run an exact Bash command. Foreground commands wait for the complete process group; use run_in_background for long-lived work. Nonzero exits, signals, and command timeout are normal outcomes.",
                json!({
                    "type":"object",
                    "properties":{
                        "command":{"type":"string","maxLength":MAXIMUM_BASH_COMMAND_BYTES},
                        "timeout_ms":{"type":"integer","minimum":1,"maximum":MAXIMUM_BASH_TIMEOUT_MS},
                        "run_in_background":{"type":"boolean"}
                    },
                    "required":["command"],
                    "additionalProperties":false
                }),
            )?,
            timeout_ms: BASH_TOOL_TIMEOUT_MS,
            executor: Arc::new(BashTool { services }),
        })
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct BashArguments {
        command: String,
        timeout_ms: Option<u64>,
        #[serde(default)]
        run_in_background: bool,
    }

    #[derive(Debug)]
    struct BashTool {
        services: Arc<BashServices>,
    }

    #[async_trait]
    impl ToolExecutor for BashTool {
        #[allow(clippy::too_many_lines)]
        async fn execute(
            &self,
            arguments: Value,
            execution: ToolExecution,
        ) -> rsi_tools_protocol::Result<ToolResult> {
            let arguments: BashArguments = match parse_arguments(arguments) {
                Ok(arguments) => arguments,
                Err(result) => return Ok(*result),
            };
            if arguments.command.is_empty()
                || arguments.command.len() > MAXIMUM_BASH_COMMAND_BYTES
                || arguments.command.contains('\0')
            {
                return error_result(
                    "invalid_arguments",
                    format!(
                        "command must be nonempty, NUL-free UTF-8 within {MAXIMUM_BASH_COMMAND_BYTES} bytes"
                    ),
                );
            }
            if arguments
                .timeout_ms
                .is_some_and(|timeout| timeout == 0 || timeout > MAXIMUM_BASH_TIMEOUT_MS)
            {
                return error_result(
                    "invalid_arguments",
                    format!("timeout_ms must be within 1..={MAXIMUM_BASH_TIMEOUT_MS}"),
                );
            }
            if arguments.run_in_background && arguments.timeout_ms.is_some() {
                return error_result(
                    "invalid_arguments",
                    "background Bash does not accept timeout_ms; use job_output or job_kill",
                );
            }
            let scope = if arguments.run_in_background {
                match execution.job_scope() {
                    Some(scope) => Some(scope.clone()),
                    None => {
                        return error_result(
                            "missing_job_scope",
                            "background Bash requires live turn-scoped Jobs authority",
                        );
                    }
                }
            } else {
                None
            };
            let confined = execution
                .confine(
                    self.services.bash.clone(),
                    vec![
                        "--noprofile".into(),
                        "--norc".into(),
                        "-c".into(),
                        arguments.command.clone(),
                    ],
                )
                .await?;
            let spec = ProcessSpec {
                process: confined,
                stdin: Vec::new(),
                environment: self.services.environment.clone(),
                stdout_max_bytes: BASH_STREAM_CAPTURE_BYTES,
                stderr_max_bytes: BASH_STREAM_CAPTURE_BYTES,
                termination_grace_ms: BASH_TERMINATION_GRACE_MS,
            };
            if let Some(scope) = scope {
                let submission = JobSubmission {
                    name: "bash".into(),
                    producer: BASH_PRODUCER.into(),
                    request: JobRequest::new(BashJobRequest { spec }),
                    requires_report: true,
                };
                return match self.services.jobs.submit(&scope, submission) {
                    Ok(id) => result_with_text(
                        json!({"job_id":id,"status":"running"}),
                        format!("Started background Bash job {id}."),
                        false,
                    ),
                    Err(error) => jobs_error_result(&error),
                };
            }

            let timeout = arguments.timeout_ms.unwrap_or(DEFAULT_BASH_TIMEOUT_MS);
            let managed = match self.services.process.spawn(spec) {
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
                    let _outcome = managed.wait().await;
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

    #[derive(Debug)]
    struct BashJobRequest {
        spec: ProcessSpec,
    }

    #[derive(Debug)]
    struct BashProducer {
        process: Arc<dyn Process>,
    }

    impl JobProducer for BashProducer {
        fn start(&self, request: &JobRequest) -> rsi_jobs::Result<Arc<dyn JobControl>> {
            let request = request.downcast_ref::<BashJobRequest>().ok_or_else(|| {
                JobsError::InvalidInput("Bash producer received the wrong request type".into())
            })?;
            let process = self
                .process
                .spawn(request.spec.clone())
                .map_err(map_process)?;
            Ok(Arc::new(BashJobControl {
                process,
                cancellation_requested: AtomicBool::new(false),
            }))
        }
    }

    #[derive(Debug)]
    struct BashJobControl {
        process: ManagedProcess,
        cancellation_requested: AtomicBool,
    }

    #[async_trait]
    impl JobControl for BashJobControl {
        fn read(&self, stream: JobStream, offset: u64) -> rsi_jobs::Result<JobOutputRead> {
            let read = match stream {
                JobStream::Stdout => self.process.stdout().read_from(offset),
                JobStream::Stderr => self.process.stderr().read_from(offset),
            }
            .map_err(map_process)?;
            Ok(JobOutputRead {
                bytes: read.bytes,
                oldest_offset: read.oldest_offset,
                next_offset: read.next_offset,
                lossy: read.lossy,
            })
        }

        fn cancel(&self) {
            self.cancellation_requested.store(true, Ordering::Release);
            self.process.terminate();
        }

        async fn wait(&self) -> rsi_jobs::Result<JobTerminal> {
            let outcome = self.process.wait().await.map_err(map_process)?;
            let cancellation_requested = self.cancellation_requested.load(Ordering::Acquire);
            Ok(JobTerminal {
                status: if cancellation_requested {
                    JobStatus::Cancelled
                } else if outcome.signal.is_none()
                    && outcome.exit_code.is_none_or(|exit_code| exit_code == 0)
                {
                    JobStatus::Completed
                } else {
                    JobStatus::Failed
                },
                exit_code: outcome.exit_code,
                signal: outcome.signal,
                message: None,
            })
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

    fn foreground_result(
        status: &str,
        managed: &ManagedProcess,
        outcome: &ProcessOutcome,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let stdout = managed
            .stdout()
            .read_from(0)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let stderr = managed
            .stderr()
            .read_from(0)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        result_with_text(
            json!({
                "status":status,
                "exit_code":outcome.exit_code,
                "signal":outcome.signal,
                "stdout":process_stream_value(&stdout),
                "stderr":process_stream_value(&stderr)
            }),
            render_stream_text(&stdout, &stderr, status),
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
        })
    }

    fn render_stream_text(stdout: &ProcessRead, stderr: &ProcessRead, fallback: &str) -> String {
        let mut stdout_text = safe_model_text(&stdout.bytes);
        let mut stderr_text = safe_model_text(&stderr.bytes);
        if stdout.lossy {
            stdout_text.insert_str(0, "[stdout truncated; showing retained tail]\n");
        }
        if stderr.lossy {
            stderr_text.insert_str(0, "[stderr truncated; showing retained tail]\n");
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
            JobsError::InvalidInput(_)
            | JobsError::DuplicateProducer(_)
            | JobsError::Execution(_) => "jobs_error",
        };
        error_result(code, error.to_string())
    }

    fn process_error_result(error: &ProcessError) -> rsi_tools_protocol::Result<ToolResult> {
        let code = match error {
            ProcessError::Capacity => "process_capacity",
            ProcessError::ShuttingDown => "process_shutting_down",
            ProcessError::Unsupported => "process_unsupported",
            ProcessError::SettlementTimeout => "process_settlement_timeout",
            ProcessError::ShutdownTimeout => "process_shutdown_timeout",
            ProcessError::InvalidInput(_) | ProcessError::Spawn(_) | ProcessError::Io(_) => {
                "process_error"
            }
        };
        error_result(code, error.to_string())
    }

    fn map_process(error: ProcessError) -> JobsError {
        match error {
            ProcessError::Capacity => JobsError::Capacity,
            ProcessError::ShuttingDown => JobsError::ShuttingDown,
            ProcessError::InvalidInput(message) => JobsError::InvalidInput(message),
            other => JobsError::Execution(other.to_string()),
        }
    }

    fn error_result(
        code: &str,
        message: impl Into<String>,
    ) -> rsi_tools_protocol::Result<ToolResult> {
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
}
