//! Shared native program engine and opt-in foreground Tool contribution.
#![deny(unsafe_code)]
#![warn(missing_docs)]
mod control;
mod error;
pub use error::ProgramError;
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
use rsi_tools_protocol::{ToolExecution, ToolProcess};
pub use runtime::ProgramRpc;
use serde::Deserialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, Mutex},
};
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
    /// Validates static native configuration without starting or probing Node.
    ///
    /// # Errors
    /// Returns a diagnostic for a relative Node path or invalid/injecting environment.
    pub fn validate(&self) -> Result<(), String> {
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
    /// Resolves and confines the exact Node bootstrap before requesting Approval.
    ///
    /// # Errors
    /// Rejects missing target dependencies, cancellation or confinement failures.
    pub async fn prepare_process(
        &self,
        execution: &ToolExecution,
    ) -> Result<ToolProcess, ProgramError> {
        if execution.cancellation.is_cancelled() || self.cancellation.is_cancelled() {
            return Err("program preparation cancelled".into());
        }
        let node = if execution.execution_lease().is_some() {
            self.configuration.node.clone()
        } else {
            let node = tokio::fs::canonicalize(&self.configuration.node)
                .await
                .map_err(|error| format!("Node executable is unavailable: {error}"))?;
            if !tokio::fs::metadata(&node)
                .await
                .map_err(|error| format!("Node executable is unavailable: {error}"))?
                .is_file()
            {
                return Err("Node executable is not a regular file".into());
            }
            node
        };
        let plan = execution
            .prepare_process(
                "node",
                node,
                self.configuration
                    .environment
                    .iter()
                    .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                    .collect(),
                vec![
                    "--input-type=commonjs".into(),
                    "--eval".into(),
                    include_str!("node.cjs").into(),
                ],
            )
            .await?;
        if execution.cancellation.is_cancelled() || self.cancellation.is_cancelled() {
            return Err("program preparation cancelled".into());
        }
        Ok(plan)
    }

    /// Admits a reviewed process to an exact Jobs scope, without starting Node.
    ///
    /// # Errors
    /// Rejects oversized scripts, unavailable executables, confinement or Job admission failure.
    pub async fn admit(
        &self,
        script: String,
        execution: &ToolExecution,
        scope: &JobScopeAuthority,
        rpc: Arc<dyn ProgramRpc>,
        process: ToolProcess,
    ) -> Result<AdmittedProgram, ProgramError> {
        self.admit_owned(
            script,
            execution,
            scope,
            rpc,
            execution.cancellation.clone(),
            process,
        )
        .await
    }
    /// Admits a reviewed workflow plan with a separate run-generation cancellation owner.
    ///
    /// # Errors
    /// Uses the same script, confinement and Jobs admission boundaries as `admit`.
    #[allow(clippy::too_many_arguments)] // One admission binds script, reviewed plan, Jobs, RPC and cancellation ownership.
    pub async fn admit_owned(
        &self,
        script: String,
        execution: &ToolExecution,
        scope: &JobScopeAuthority,
        rpc: Arc<dyn ProgramRpc>,
        cancellation: CancellationToken,
        process: ToolProcess,
    ) -> Result<AdmittedProgram, ProgramError> {
        if script.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES {
            return Err("script exceeds 64 KiB".into());
        }
        if execution.cancellation.is_cancelled()
            || cancellation.is_cancelled()
            || self.cancellation.is_cancelled()
        {
            return Err("program preparation cancelled".into());
        }
        let definitions = runtime::contain_sync(|| rpc.definitions()).map_err(|_| {
            ProgramError::Failed("program RPC definitions panicked before admission".into())
        })?;
        let (outcome, result) = tokio::sync::watch::channel(None);
        let request = Arc::new(runtime::Request {
            spec: Mutex::new(Some(DuplexProcessSpec {
                environment: process.environment().to_vec(),
                process,
                stdout_buffer_bytes: runtime::MAXIMUM_FRAME,
                stderr_max_bytes: 64 * 1024,
                termination_grace_ms: 500,
            })),
            script,
            definitions,
            rpc,
            cancelled_at_settlement: std::sync::atomic::AtomicBool::new(false),
            start: CancellationToken::new(),
            cancel: cancellation.child_token(),
            outcome: result.clone(),
            completion: Mutex::new(Some(outcome)),
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
            .await
            .map_err(ProgramError::from)?;
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
    result: tokio::sync::watch::Receiver<Option<Result<Value, ProgramError>>>,
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
    pub(crate) async fn cancel_with(&self, reason: &str) -> Result<Value, ProgramError> {
        self.cancel();
        match self.result().await {
            Err(ProgramError::OutcomeUnknown) => Err(ProgramError::OutcomeUnknown),
            _ => Err(reason.into()),
        }
    }
    /// Waits for RPC and process settlement and their bounded result/diagnostic.
    /// A non-cooperative handler can retain settlement after process termination.
    ///
    /// # Errors
    /// Reports script, protocol, cancellation or process-settlement failure.
    pub async fn result(&self) -> Result<Value, ProgramError> {
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
        let tasks = tokio_util::task::TaskTracker::new();
        let producer = jobs
            .register_producer(JobProducerRegistration {
                name: PRODUCER.into(),
                producer: Arc::new(runtime::Producer {
                    process: plan.local::<DuplexProcessContract>()?,
                    tasks: tasks.clone(),
                }),
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let runtime = Arc::new(ProgramRuntime {
            configuration,
            jobs,
            tasks,
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
