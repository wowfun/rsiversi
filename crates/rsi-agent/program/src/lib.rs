//! Shared native program engine and opt-in foreground Tool contribution.
#![deny(unsafe_code)]
#![warn(missing_docs)]
mod control;
mod runtime;
mod tool;
mod workflow;
use async_trait::async_trait;
use rsi_jobs::{
    JobProducerRegistration, JobRequest, JobScopeAuthority, JobSubmission, Jobs, JobsContract,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_process::{DuplexProcessContract, DuplexProcessSpec};
use rsi_tools_protocol::ToolExecution;
pub use runtime::ProgramRpc;
use serde::Deserialize;
use serde_json::Value;
use std::{ffi::OsString, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
pub use tool::ProgramToolsFactory;

const OWNER_RETIREMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const PRODUCER: &str = "rsi.agent.program";
/// Frozen native process configuration. Environment is explicit and complete.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramConfiguration {
    /// Absolute Node executable; validated before any Job admission.
    pub node: PathBuf,
    /// Complete child environment. Node startup-injection variables are rejected.
    #[serde(default)]
    pub environment: Vec<(String, String)>,
}
impl ProgramConfiguration {
    fn validate(&self) -> Result<(), String> {
        if !self.node.is_absolute() {
            return Err("Node executable must be absolute".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for (name, _) in &self.environment {
            if name.is_empty()
                || name.contains(['=', '\0'])
                || !names.insert(name)
                || name.starts_with("NODE_")
                || name.starts_with("LD_")
                || name.starts_with("DYLD_")
            {
                return Err("invalid or injecting program environment variable".into());
            }
        }
        Ok(())
    }
}
/// Process-local preparation service shared by foreground and detached owners.
#[derive(Debug)]
pub struct ProgramRuntime {
    configuration: ProgramConfiguration,
    jobs: Arc<dyn Jobs>,
    tasks: tokio_util::task::TaskTracker,
    cancellation: CancellationToken,
}
impl ProgramRuntime {
    /// Confines and admits work to an exact Jobs scope, without starting Node.
    ///
    /// # Errors
    /// Rejects oversized scripts, unavailable executables, confinement or Job admission failure.
    pub async fn prepare(
        &self,
        script: String,
        execution: &ToolExecution,
        scope: &JobScopeAuthority,
        rpc: Arc<dyn ProgramRpc>,
    ) -> Result<AdmittedProgram, String> {
        self.prepare_owned(
            script,
            execution,
            scope,
            rpc,
            execution.cancellation.clone(),
        )
        .await
    }
    /// Confines a workflow Job using a separate run-generation cancellation owner.
    ///
    /// # Errors
    /// Uses the same script, confinement and Jobs admission boundaries as `prepare`.
    pub async fn prepare_owned(
        &self,
        script: String,
        execution: &ToolExecution,
        scope: &JobScopeAuthority,
        rpc: Arc<dyn ProgramRpc>,
        cancellation: CancellationToken,
    ) -> Result<AdmittedProgram, String> {
        if script.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES {
            return Err("script exceeds 64 KiB".into());
        }
        if execution.cancellation.is_cancelled() || self.cancellation.is_cancelled() {
            return Err("program preparation cancelled".into());
        }
        let node = std::fs::canonicalize(&self.configuration.node)
            .map_err(|error| format!("Node executable is unavailable: {error}"))?;
        if !node.is_file() {
            return Err("Node executable is not a regular file".into());
        }
        let confined = execution
            .confine(
                node,
                vec![
                    "--input-type=commonjs".into(),
                    "--eval".into(),
                    include_str!("node.cjs").into(),
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        if execution.cancellation.is_cancelled()
            || cancellation.is_cancelled()
            || self.cancellation.is_cancelled()
        {
            return Err("program preparation cancelled".into());
        }
        let (outcome, result) = tokio::sync::watch::channel(None);
        let request = Arc::new(runtime::Request {
            spec: DuplexProcessSpec {
                process: confined,
                environment: self
                    .configuration
                    .environment
                    .iter()
                    .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                    .collect(),
                stdout_buffer_bytes: runtime::MAXIMUM_FRAME,
                stderr_max_bytes: 64 * 1024,
                termination_grace_ms: 500,
            },
            script,
            rpc,
            cancelled_at_settlement: std::sync::atomic::AtomicBool::new(false),
            start: CancellationToken::new(),
            cancel: cancellation.child_token(),
            outcome,
        });
        let id = self
            .jobs
            .submit(
                scope,
                JobSubmission {
                    name: "agent-program".into(),
                    producer: PRODUCER.into(),
                    origin: execution
                        .extension::<rsi_jobs::JobOrigin>()
                        .map(|origin| origin.as_str().to_owned()),
                    request: JobRequest::new(request.clone()),
                    requires_report: true,
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(AdmittedProgram {
            id,
            request,
            result,
            started: false,
        })
    }
}
/// An admitted Job whose process cannot start until its owner opens the latch.
#[derive(Debug)]
pub struct AdmittedProgram {
    id: String,
    request: Arc<runtime::Request>,
    result: tokio::sync::watch::Receiver<Option<Result<Value, String>>>,
    started: bool,
}
impl AdmittedProgram {
    /// Exact process-local Job identity.
    pub fn job_id(&self) -> &str {
        &self.id
    }
    /// Opens the latch once durable authorization is established by the caller.
    ///
    /// # Errors
    /// Rejects a repeated start or cancellation before start.
    pub fn start(&mut self) -> Result<(), String> {
        if self.started {
            return Err("program already started".into());
        }
        if self.request.cancel.is_cancelled() {
            return Err("program was cancelled before start".into());
        }
        self.started = true;
        self.request.start.cancel();
        Ok(())
    }
    /// Requests cancellation; `result` joins the engine and its RPC owners.
    pub fn cancel(&self) {
        self.request.cancel.cancel();
    }
    /// Waits for the bounded complete result or terminal diagnostic.
    ///
    /// # Errors
    /// Reports script, protocol, cancellation or process-settlement failure.
    pub async fn result(&self) -> Result<Value, String> {
        runtime::wait_result(self.result.clone()).await
    }
}
impl Drop for AdmittedProgram {
    fn drop(&mut self) {
        self.request.cancel.cancel();
    }
}
/// Local runtime service, independent of any model-visible Tool catalog.
#[derive(Debug)]
pub struct ProgramRuntimeContract;
impl LocalContract for ProgramRuntimeContract {
    const KEY: &'static str = "rsi.agent.program.runtime";
    type Service = ProgramRuntime;
}
/// Registers the shared Jobs producer and runtime in a native Host generation.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProgramRuntimeFactory;
#[async_trait]
impl PluginFactory for ProgramRuntimeFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let configuration: ProgramConfiguration = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        configuration.validate().map_err(MetaError::InvalidInput)?;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            configuration,
            serde_json::to_vec(desired)
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
                .len(),
        )
        .requiring_local::<JobsContract>()
        .requiring_local::<DuplexProcessContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let configuration = plan.take_state::<ProgramConfiguration>()?;
        let jobs = plan.local::<JobsContract>()?;
        let producer = jobs
            .register_producer(JobProducerRegistration {
                name: PRODUCER.into(),
                producer: Arc::new(runtime::Producer(plan.local::<DuplexProcessContract>()?)),
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let runtime = Arc::new(ProgramRuntime {
            configuration,
            jobs,
            tasks: tokio_util::task::TaskTracker::new(),
            cancellation: CancellationToken::new(),
        });
        let supply = plan
            .context()
            .provide_local::<ProgramRuntimeContract>(runtime.clone())?;
        plan.defer(
            "retire program runtime",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    runtime.cancellation.cancel();
                    let reaped = producer.retire().await.map_err(|error| error.to_string());
                    runtime.tasks.close();
                    let joined =
                        tokio::time::timeout(OWNER_RETIREMENT_TIMEOUT, runtime.tasks.wait())
                            .await
                            .map_err(|_| {
                                "program owner retirement timed out; admitted work remains owned"
                                    .to_owned()
                            });
                    reaped.and(joined)
                })
            }),
        )
    }
}
