//! Process preparation shared by policy/approval orchestration and trusted Tool bodies.
use super::{
    ConfinedProcess, EnforcementStamp, ProcessRequest, Result, Sandbox, ToolEnforcement, ToolError,
    ToolExecution, ToolExecutionPolicy, ToolStart, execution_error,
};
use rsi_execution::{ExecutionLease, ExecutionReview, PreparedProcess};
use rsi_process::{ManagedProcess, Process, ProcessSpec};
use std::ffi::OsString;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// One move-only process plan, fixed before approval and consumed through its issuer.
#[derive(Debug)]
pub struct ToolProcess {
    plan: Plan,
    environment: Vec<(OsString, OsString)>,
}
#[derive(Debug)]
enum Plan {
    Native(ConfinedProcess),
    Target(PreparedProcess),
}
impl ToolProcess {
    /// Returns the complete environment frozen during preparation.
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
    /// Returns actual confinement evidence selected by the preparing provider.
    pub fn enforcement(&self) -> &EnforcementStamp {
        match &self.plan {
            Plan::Native(plan) => &plan.stamp,
            Plan::Target(plan) => plan.enforcement(),
        }
    }
    /// Consumes a duplex invocation through the exact plan's issuing tuple.
    pub async fn spawn_duplex(
        spec: rsi_process::DuplexProcessSpec<Self>,
        native: &dyn rsi_process::DuplexProcess,
    ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
        if spec.environment != spec.process.environment {
            return Err(rsi_process::ProcessError::InvalidInput(
                "Tool process environment changed after preparation".into(),
            ));
        }
        match spec.process.plan {
            Plan::Native(plan) => {
                native
                    .spawn(rsi_process::DuplexProcessSpec {
                        process: plan,
                        environment: spec.environment,
                        stdout_buffer_bytes: spec.stdout_buffer_bytes,
                        stderr_max_bytes: spec.stderr_max_bytes,
                        termination_grace_ms: spec.termination_grace_ms,
                    })
                    .await
            }
            Plan::Target(plan) => {
                plan.lease()
                    .spawn_duplex(rsi_process::DuplexProcessSpec {
                        process: plan,
                        environment: spec.environment,
                        stdout_buffer_bytes: spec.stdout_buffer_bytes,
                        stderr_max_bytes: spec.stderr_max_bytes,
                        termination_grace_ms: spec.termination_grace_ms,
                    })
                    .await
            }
        }
    }
    /// Consumes a complete invocation through the exact prepared target, or explicit native embedding.
    pub async fn spawn(
        spec: ProcessSpec<Self>,
        native: &dyn Process,
    ) -> rsi_process::Result<ManagedProcess> {
        if spec.environment != spec.process.environment {
            return Err(rsi_process::ProcessError::InvalidInput(
                "Tool process environment changed after preparation".into(),
            ));
        }
        match spec.process.plan {
            Plan::Native(plan) => {
                native
                    .spawn(ProcessSpec {
                        process: plan,
                        stdin: spec.stdin,
                        environment: spec.environment,
                        stdout_max_bytes: spec.stdout_max_bytes,
                        stderr_max_bytes: spec.stderr_max_bytes,
                        termination_grace_ms: spec.termination_grace_ms,
                    })
                    .await
            }
            Plan::Target(plan) => {
                plan.lease()
                    .spawn(ProcessSpec {
                        process: plan,
                        stdin: spec.stdin,
                        environment: spec.environment,
                        stdout_max_bytes: spec.stdout_max_bytes,
                        stderr_max_bytes: spec.stderr_max_bytes,
                        termination_grace_ms: spec.termination_grace_ms,
                    })
                    .await
            }
        }
    }
}

/// Private execution selection retained by a prepared Tool call, distinct from its request hash.
#[derive(Debug)]
pub struct ToolPreparation {
    policy: ToolExecutionPolicy,
    sandbox: Arc<dyn Sandbox>,
    lease: Option<Arc<ExecutionLease>>,
    process: Option<ToolProcess>,
}
impl ToolPreparation {
    /// Freezes one preparation result from the exact context that produced it.
    pub fn new(execution: ToolExecution, process: Option<ToolProcess>) -> Result<Self> {
        let lease = execution.execution_lease();
        let matches = match (process.as_ref().map(|process| &process.plan), &lease) {
            (None, _) | (Some(Plan::Native(_)), None) => true,
            (Some(Plan::Target(plan)), Some(lease)) => plan.lease() == **lease,
            _ => false,
        };
        if !matches {
            return Err(ToolError::Execution(
                "prepared Tool process belongs to another execution tuple".into(),
            ));
        }
        Ok(Self {
            policy: execution.policy,
            sandbox: execution.sandbox,
            lease,
            process,
        })
    }
    /// Returns bounded non-authorizing review metadata, never the plan or environment.
    pub fn review(&self) -> Result<Option<ExecutionReview>> {
        let Some(lease) = &self.lease else {
            return Ok(None);
        };
        let sequence = match self.process.as_ref().map(|process| &process.plan) {
            Some(Plan::Target(plan)) => std::num::NonZeroU64::new(plan.identity().sequence()),
            _ => None,
        };
        let workspace =
            self.policy.workspace.to_str().ok_or_else(|| {
                ToolError::InvalidInput("execution workspace must be UTF-8".into())
            })?;
        ExecutionReview::new(lease.binding().clone(), sequence, workspace.to_owned())
            .map(Some)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))
    }
    /// Rejects changed execution authority before any Tool body or process can start.
    pub fn validate_start(&self, start: &ToolStart) -> Result<()> {
        let lease = start.extensions.get::<ExecutionLease>();
        if self.policy != start.policy
            || self.lease != lease
            || (lease.is_none() && !Arc::ptr_eq(&self.sandbox, &start.sandbox))
        {
            return Err(ToolError::Execution(
                "Tool execution changed after preparation".into(),
            ));
        }
        Ok(())
    }
    /// Admits the reviewed authority again and transfers the original plan into one execution.
    pub fn start(
        self,
        call_id: String,
        start: ToolStart,
    ) -> Result<(ToolExecution, ToolEnforcement)> {
        self.validate_start(&start)?;
        let (mut execution, enforcement) = ToolExecution::from_start(call_id, start)?;
        execution.prepared_process = Arc::new(Mutex::new(self.process));
        Ok((execution, enforcement))
    }
}
impl ToolExecution {
    /// Prepares one process before Approval using a target selector or explicit native embedding.
    pub async fn prepare_process(
        &self,
        selector: &str,
        native_program: PathBuf,
        native_environment: Vec<(OsString, OsString)>,
        arguments: Vec<String>,
    ) -> Result<ToolProcess> {
        if self.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let request = ProcessRequest {
            stdio: rsi_sandbox::ProcessStdio::Pipes,
            mode: self.policy.mode,
            program: native_program,
            arguments,
            cwd: self.policy.cwd.clone(),
            workspace: self.policy.workspace.clone(),
        };
        if let Some(lease) = self.execution_lease() {
            let program = if *lease.binding().location() == rsi_execution::ExecutionLocation::Local
            {
                lease
                    .resolve_local_program(rsi_execution::ResolvedProgram {
                        program: request.program.clone(),
                        environment: native_environment,
                    })
                    .await
            } else {
                lease.resolve_program(selector).await
            }
            .map_err(execution_error)?;
            let plan = lease
                .prepare(request.map_program(|_| program))
                .await
                .map_err(execution_error)?;
            let environment = plan.environment().to_vec();
            Ok(ToolProcess {
                plan: Plan::Target(plan),
                environment,
            })
        } else {
            let plan = self
                .sandbox
                .confine(request)
                .await
                .map_err(ToolError::Sandbox)?;
            Ok(ToolProcess {
                plan: Plan::Native(plan),
                environment: native_environment,
            })
        }
    }
    /// Moves the one reviewed process into its producer and records its real enforcement once.
    pub fn take_prepared_process(&self) -> Result<ToolProcess> {
        let plan = self
            .prepared_process
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                ToolError::Execution("Tool has no unconsumed prepared process".into())
            })?;
        self.record_enforcement(plan.enforcement().clone())?;
        Ok(plan)
    }
}
