//! One source policy over native user files and a lease-bound target project collector.
use super::*;
use project::{Capture, Request};
use rsi_execution::ExecutionLease;
use rsi_process::{DuplexProcessSpec, ManagedDuplexProcess, ProcessError};
use rsi_sandbox::{ProcessRequest, ProcessStdio, SandboxMode};
use std::{future::Future, time::Duration};

/// Workspace context source that keeps configured user sources on the Service.
#[derive(Clone, Debug)]
pub struct TargetWorkspaceContext {
    pub(super) local: LocalWorkspaceContext,
}
impl TargetWorkspaceContext {
    /// Validates the Service's user source configuration without resolving any target.
    pub fn new(config: WorkspaceContextConfig) -> Result<Self, WorkspaceContextError> {
        Ok(Self {
            local: LocalWorkspaceContext::new(config)?,
        })
    }

    async fn remote(
        &self,
        header: &SessionHeader,
        execution: Option<&ExecutionLease>,
        request: Request,
        cancellation: CancellationToken,
    ) -> Result<(Capture, Vec<(String, String)>), WorkspaceContextError> {
        request.validate()?;
        let operation = admit_source(header, execution)?;
        let execution = execution.cloned().ok_or(WorkspaceContextError::Closed)?;
        let cwd = PathBuf::from(header.canonical_cwd());
        let config = self.local.config.clone();
        let stop = self.local.owner.cancellation.child_token();
        let _guard = stop.clone().drop_guard();
        let lane = cancellation
            .run_until_cancelled(self.local.owner.acquire())
            .await
            .ok_or(WorkspaceContextError::Closed)??;
        cancellation
            .run_until_cancelled(lane.run_async(async move {
                let _operation = operation;
                combined(config, cwd, execution, request, stop).await
            }))
            .await
            .ok_or(WorkspaceContextError::Closed)?
    }
}

async fn combined(
    config: Arc<WorkspaceContextConfig>,
    cwd: PathBuf,
    execution: ExecutionLease,
    mut request: Request,
    stop: CancellationToken,
) -> Result<(Capture, Vec<(String, String)>), WorkspaceContextError> {
    let mut user = Vec::new();
    let mut user_diagnostic = None;
    if let Request::Snapshot {
        retained_instruction_bytes,
        ..
    } = &mut request
    {
        let (sections, diagnostic) = blocking({
            let (config, cwd, stop) = (config.clone(), cwd.clone(), stop.clone());
            move || {
                let budget = SnapshotBudget::new(&config, &cwd, stop)?;
                let mut observation = Observation::default();
                Ok((
                    read_user_sections(&config, &budget, &mut observation)?,
                    observation.diagnostic,
                ))
            }
        })
        .await?;
        user = sections;
        user_diagnostic = diagnostic;
        *retained_instruction_bytes = instruction_retained_bytes(&user);
    }
    let capture = exchange(&execution, &cwd, &request, stop.clone()).await?;
    let capture = blocking(move || {
        let budget = SnapshotBudget::new(&config, &cwd, stop)?;
        match (capture, &request) {
            (Capture::Context(mut context), Request::Snapshot { .. } | Request::Skills { .. }) => {
                if context.diagnostic.is_none() {
                    context.diagnostic = user_diagnostic;
                }
                project::add_user_sources(&config, &request, context, budget).map(Capture::Context)
            }
            (Capture::Agents(collection), Request::Agents { id, reserved }) => {
                agents::read_partition(
                    &config,
                    &cwd,
                    agents::Selection {
                        id: id.as_deref(),
                        reserved_names: reserved,
                        project: false,
                    },
                    budget,
                    collection,
                )
                .map(Capture::Agents)
            }
            _ => Err(WorkspaceContextError::Invalid(
                "project context response kind changed".into(),
            )),
        }
    })
    .await?;
    Ok((capture, user))
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, WorkspaceContextError> + Send + 'static,
) -> Result<T, WorkspaceContextError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| WorkspaceContextError::Failed("workspace source task failed".into()))?
}
fn process_error(error: &ProcessError) -> WorkspaceContextError {
    match error {
        ProcessError::Capacity => WorkspaceContextError::Capacity,
        ProcessError::ShuttingDown | ProcessError::Api(_) => WorkspaceContextError::Closed,
        _ => WorkspaceContextError::Failed(
            "target project context unavailable; refresh to retry".into(),
        ),
    }
}
async fn bounded<T>(
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
    future: impl Future<Output = Result<T, ProcessError>>,
) -> Result<T, WorkspaceContextError> {
    stop.run_until_cancelled(tokio::time::timeout_at(deadline, future))
        .await
        .ok_or(WorkspaceContextError::Closed)?
        .map_err(|_| {
            WorkspaceContextError::Failed("target project context deadline elapsed".into())
        })?
        .map_err(|error| process_error(&error))
}
async fn exchange(
    execution: &ExecutionLease,
    cwd: &Path,
    request: &Request,
    stop: CancellationToken,
) -> Result<Capture, WorkspaceContextError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let program = bounded(
        &stop,
        deadline,
        execution.resolve_program("workspace_context"),
    )
    .await?;
    let plan = bounded(
        &stop,
        deadline,
        execution.prepare_source_reader(ProcessRequest {
            stdio: ProcessStdio::Pipes,
            mode: SandboxMode::ReadOnly,
            program,
            arguments: vec![project::MARKER.into()],
            cwd: cwd.to_owned(),
            workspace: cwd.to_owned(),
        }),
    )
    .await?;
    if plan.enforcement().scratch != rsi_sandbox::SandboxScratch::Host
        || plan.enforcement().network != rsi_sandbox::SandboxNetwork::Isolated
    {
        return Err(WorkspaceContextError::Failed(
            "target source-reader view is unavailable".into(),
        ));
    }
    let environment = plan.environment().to_vec();
    let process = bounded(
        &stop,
        deadline,
        execution.spawn_duplex(DuplexProcessSpec {
            process: plan,
            environment,
            stdout_buffer_bytes: rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES,
            stderr_max_bytes: 4096,
            termination_grace_ms: 100,
        }),
    )
    .await?;
    let bytes = read_exchange(
        &process,
        project::encode(request, project::MAXIMUM_REQUEST_BYTES)?,
        &stop,
        deadline,
    )
    .await;
    if bytes.is_err() {
        process.terminate();
        let _ = tokio::time::timeout(Duration::from_secs(3), process.wait_settlement()).await;
    }
    let bytes = bytes?;
    let reply: project::Reply = serde_json::from_slice(&bytes).map_err(|_| {
        WorkspaceContextError::Invalid("invalid target project context response".into())
    })?;
    match reply {
        project::Reply::Captured { capture } => {
            capture.validate(request)?;
            Ok(capture)
        }
        project::Reply::Failed { capacity: true } => Err(WorkspaceContextError::Capacity),
        project::Reply::Failed { capacity: false } => {
            Err(process_error(&ProcessError::Unsupported))
        }
    }
}

async fn read_exchange(
    process: &ManagedDuplexProcess,
    request: Vec<u8>,
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, WorkspaceContextError> {
    let input = process.stdin();
    let mut offset = 0;
    while offset < request.len() {
        let end = (offset + rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).min(request.len());
        offset += bounded(stop, deadline, input.write(&request[offset..end])).await?;
    }
    bounded(stop, deadline, input.close()).await?;
    let output = process.stdout();
    let mut bytes = Vec::new();
    loop {
        let chunk = bounded(
            stop,
            deadline,
            output.read(rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES),
        )
        .await?;
        if chunk.bytes.len() > project::MAXIMUM_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(WorkspaceContextError::Capacity);
        }
        bytes.extend_from_slice(&chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    if bounded(stop, deadline, process.wait()).await?.exit_code != Some(0) {
        return Err(process_error(&ProcessError::Unsupported));
    }
    Ok(bytes)
}

#[async_trait]
impl WorkspaceContext for TargetWorkspaceContext {
    async fn agents(
        &self,
        header: &SessionHeader,
        execution: Option<&ExecutionLease>,
        id: Option<&str>,
        reserved_names: &BTreeSet<String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<WorkspaceAgentDefinition>, WorkspaceContextError> {
        if *header.coordinates().location() == rsi_execution::ExecutionLocation::Local {
            return self
                .local
                .agents(header, execution, id, reserved_names, cancellation)
                .await;
        }
        let (capture, _) = self
            .remote(
                header,
                execution,
                Request::Agents {
                    id: id.map(str::to_owned),
                    reserved: reserved_names.clone(),
                },
                cancellation,
            )
            .await?;
        let Capture::Agents(collection) = capture else {
            return Err(WorkspaceContextError::Invalid(
                "expected Agent definitions".into(),
            ));
        };
        Ok(collection.selected.into_values().collect())
    }
    async fn skills(
        &self,
        header: &SessionHeader,
        execution: Option<&ExecutionLease>,
        id: Option<&str>,
        audience: SkillAudience,
        cancellation: CancellationToken,
    ) -> Result<rsi_agent_session_protocol::SessionResourceValue, WorkspaceContextError> {
        if *header.coordinates().location() == rsi_execution::ExecutionLocation::Local {
            return self
                .local
                .skills(header, execution, id, audience, cancellation)
                .await;
        }
        let (capture, _) = self
            .remote(
                header,
                execution,
                Request::Skills {
                    id: id.map(str::to_owned),
                    audience,
                },
                cancellation,
            )
            .await?;
        let Capture::Context(context) = capture else {
            return Err(WorkspaceContextError::Invalid(
                "expected skill context".into(),
            ));
        };
        project::resource(context, id, audience)
    }
    async fn snapshot(
        &self,
        header: &SessionHeader,
        execution: Option<&ExecutionLease>,
        requests: &WorkspaceSkillRequests,
    ) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        if *header.coordinates().location() == rsi_execution::ExecutionLocation::Local {
            return self.local.snapshot(header, execution, requests).await;
        }
        let response = self
            .remote(
                header,
                execution,
                Request::Snapshot {
                    names: requests.names().to_vec(),
                    retained_instruction_bytes: INSTRUCTIONS_PREAMBLE.len(),
                },
                self.local.owner.cancellation.child_token(),
            )
            .await;
        let (capture, user) = match response {
            Ok(response) => response,
            Err(WorkspaceContextError::Failed(diagnostic)) => {
                return Ok(project::snapshot(
                    project::Context {
                        diagnostic: Some(diagnostic),
                        ..project::Context::default()
                    },
                    &[],
                    &[],
                ));
            }
            Err(error) => return Err(error),
        };
        let Capture::Context(context) = capture else {
            return Err(WorkspaceContextError::Invalid(
                "expected workspace context".into(),
            ));
        };
        Ok(project::snapshot(context, &user, requests.names()))
    }
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
