use super::{ProcessConnection, RemoteFiles, RemotePlan, Reply, Request, State, invalid};
use async_trait::async_trait;
use rsi_execution::{
    BackendPlan, ExecutionBackend, ExecutionLocation, ExecutionPin, ExecutionProvider,
    ExecutionTargetId, ResolvedProgram,
};
use rsi_files_protocol::Files;
use rsi_process::{
    DuplexProcessSpec, ManagedDuplexProcess, ManagedProcess, ManagedPtyProcess, ProcessSpec,
    PtyProcessSpec, Result,
};
use rsi_sandbox::{
    ProcessRequest, ProcessStdio, SandboxGeneration, WorkspaceReadRequest, WorkspaceReadScope,
};
use rsi_ssh_protocol::execution::{Preparation, StartOptions};
use std::{ffi::OsString, path::Path, sync::Arc};

/// Freezes one already admitted target connection into a complete Execution backend.
/// No address lookup, trust decision, grant issuance or reconnect is performed here.
pub fn execution_provider(
    host_epoch: rsi_execution::HostEpoch,
    client: ProcessConnection,
    target: ExecutionTargetId,
    revision: u64,
) -> Result<ExecutionProvider> {
    let epoch = client.epoch();
    ExecutionProvider::new(
        host_epoch,
        ExecutionLocation::Ssh { target },
        revision,
        epoch,
        Arc::new(RemoteBackend {
            files: Arc::new(RemoteFiles::new(client.clone())),
            client,
            generation: SandboxGeneration::default(),
        }),
    )
}
#[derive(Debug)]
struct RemoteBackend {
    client: ProcessConnection,
    files: Arc<RemoteFiles>,
    generation: SandboxGeneration,
}
fn path(value: &Path) -> Result<String> {
    value.to_str().map(str::to_owned).ok_or_else(invalid)
}
impl RemoteBackend {
    async fn prepare_view(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
        source_reader: bool,
    ) -> Result<BackendPlan> {
        let wire_environment = environment
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.to_str().ok_or_else(invalid)?.to_owned(),
                    value.to_str().ok_or_else(invalid)?.to_owned(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let plan = self
            .client
            .prepare(Preparation {
                source_reader,
                mode: request.mode,
                pty: request.stdio == ProcessStdio::Pty,
                program: path(&request.program)?,
                arguments: request.arguments,
                cwd: path(&request.cwd)?,
                workspace: path(&request.workspace)?,
                environment: wire_environment,
            })
            .await?;
        let enforcement = plan.enforcement().clone();
        Ok(BackendPlan::new(plan, enforcement, environment))
    }
}
#[async_trait]
impl ExecutionBackend for RemoteBackend {
    async fn canonicalize(&self, path: &str) -> Result<String> {
        self.client.canonicalize(path).await
    }
    async fn resolve_program(&self, selector: &str) -> Result<ResolvedProgram> {
        let program = self.client.resolve(selector).await?;
        Ok(ResolvedProgram {
            program: program.program.into(),
            environment: program
                .environment
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        })
    }
    async fn resolve_target_program(
        &self,
        program: rsi_execution::TargetProgram,
    ) -> Result<ResolvedProgram> {
        let program = self.client.resolve_configured(program).await?;
        Ok(ResolvedProgram {
            program: program.program.into(),
            environment: program
                .environment
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        })
    }
    async fn prepare(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<BackendPlan> {
        self.prepare_view(request, environment, false).await
    }
    async fn prepare_source_reader(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<BackendPlan> {
        self.prepare_view(request, environment, true).await
    }
    async fn spawn(
        &self,
        spec: ProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedProcess> {
        let options = StartOptions::Batch {
            stdin_bytes: spec.stdin.len(),
            stdout_max_bytes: spec.stdout_max_bytes,
            stderr_max_bytes: spec.stderr_max_bytes,
            termination_grace_ms: spec.termination_grace_ms,
        };
        let owner = self
            .client
            .start(spec.process.take::<RemotePlan>()?, options, spec.stdin)
            .await?;
        owner.0.retain_pin(pin);
        Ok(ManagedProcess::new(owner))
    }
    async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedDuplexProcess> {
        let options = StartOptions::Duplex {
            stdout_buffer_bytes: spec.stdout_buffer_bytes,
            stderr_max_bytes: spec.stderr_max_bytes,
            termination_grace_ms: spec.termination_grace_ms,
        };
        let owner = self
            .client
            .start(spec.process.take::<RemotePlan>()?, options, vec![])
            .await?;
        owner.0.retain_pin(pin);
        Ok(ManagedDuplexProcess::new(owner))
    }
    async fn spawn_pty(
        &self,
        spec: PtyProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedPtyProcess> {
        let options = StartOptions::Pty {
            columns: spec.size.columns,
            rows: spec.size.rows,
            termination_grace_ms: spec.termination_grace_ms,
        };
        let owner = self
            .client
            .start(spec.process.take::<RemotePlan>()?, options, vec![])
            .await?;
        owner.0.retain_pin(pin);
        Ok(ManagedPtyProcess::new(owner))
    }
    async fn workspace_read(&self, request: WorkspaceReadRequest) -> Result<WorkspaceReadScope> {
        let scope = WorkspaceReadScope::new(request.clone(), self.generation.clone())
            .map_err(|_| invalid())?;
        match self
            .client
            .call(Request::WorkspaceRead {
                mode: request.mode,
                cwd: path(&request.cwd)?,
                workspace: path(&request.workspace)?,
            })
            .await?
        {
            Reply::Done => Ok(scope),
            _ => Err(self.client.malformed()),
        }
    }
    fn files(&self) -> Arc<dyn Files> {
        self.files.clone()
    }
}
#[derive(Debug, Default)]
pub(super) struct PinOwner {
    pub finished: bool,
    pub pin: Option<ExecutionPin>,
}
impl State {
    pub(super) fn retain_pin(&self, pin: ExecutionPin) {
        let mut owner = self
            .pin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !owner.finished {
            owner.pin = Some(pin);
        }
    }
    pub(super) fn finish_pin(&self) {
        let mut owner = self
            .pin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owner.finished = true;
        owner.pin.take();
    }
}
