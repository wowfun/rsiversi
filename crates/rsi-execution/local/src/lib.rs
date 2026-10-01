//! An exact native capability tuple with explicit program and environment selection.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_execution::{
    BackendPlan, ExecutionBackend, ExecutionLocation, ExecutionPin, ExecutionProvider,
    ResolvedProgram,
};
use rsi_files_protocol::Files;
use rsi_process::{
    DuplexProcess, DuplexProcessSpec, ManagedDuplexProcess, ManagedProcess, ManagedPtyProcess,
    Process, ProcessError, ProcessSpec, PtyProcess, PtyProcessSpec, Result,
};
use rsi_sandbox::{
    ConfinedProcess, ProcessRequest, Sandbox, SandboxError, WorkspaceReadRequest,
    WorkspaceReadScope,
};
use std::{collections::BTreeMap, sync::Arc};

/// Explicit providers frozen together by native composition.
#[derive(Debug)]
pub struct NativeCapabilities {
    /// Exact sandbox generation.
    pub sandbox: Arc<dyn Sandbox>,
    /// Batch process generation.
    pub process: Arc<dyn Process>,
    /// Ongoing byte process generation.
    pub duplex: Arc<dyn DuplexProcess>,
    /// Native terminal generation.
    pub pty: Arc<dyn PtyProcess>,
    /// Native directory-handle reader generation.
    pub files: Arc<dyn Files>,
}

#[derive(Debug)]
struct NativeBackend {
    capabilities: NativeCapabilities,
    programs: BTreeMap<String, ResolvedProgram>,
}

/// Freezes native providers and a finite explicit program catalog into a Local provider.
/// Program paths must already be absolute; canonicalization happens only on this machine.
/// At most 128 selectors and 1 MiB of aggregate child environment are retained.
pub async fn provider(
    host_epoch: rsi_execution::HostEpoch,
    capabilities: NativeCapabilities,
    mut programs: BTreeMap<String, ResolvedProgram>,
) -> Result<ExecutionProvider> {
    if programs.len() > 128 {
        return Err(ProcessError::Capacity);
    }
    let mut environment_bytes = 0usize;
    for (selector, program) in &mut programs {
        if selector.is_empty()
            || selector.len() > 128
            || selector.chars().any(char::is_control)
            || !program.program.is_absolute()
        {
            return Err(ProcessError::InvalidInput(
                "invalid native program selector or path".into(),
            ));
        }
        rsi_process::validate_environment(&program.environment)?;
        for (name, value) in &program.environment {
            environment_bytes = environment_bytes
                .saturating_add(name.len())
                .saturating_add(value.len());
        }
        if environment_bytes > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
            return Err(ProcessError::Capacity);
        }
        *program = resolve_local_program(program.clone()).await?;
    }
    ExecutionProvider::new(
        host_epoch,
        ExecutionLocation::Local,
        0,
        0,
        Arc::new(NativeBackend {
            capabilities,
            programs,
        }),
    )
}

async fn resolve_local_program(mut program: ResolvedProgram) -> Result<ResolvedProgram> {
    if !program.program.is_absolute() {
        return Err(ProcessError::InvalidInput(
            "native program path must be absolute".into(),
        ));
    }
    rsi_process::validate_environment(&program.environment)?;
    program.program = tokio::fs::canonicalize(&program.program)
        .await
        .map_err(|_| {
            ProcessError::InvalidInput("configured native program is unavailable".into())
        })?;
    let metadata = tokio::fs::metadata(&program.program).await.map_err(|_| {
        ProcessError::InvalidInput("configured native program is unavailable".into())
    })?;
    if !metadata.is_file() {
        return Err(ProcessError::InvalidInput(
            "configured native program is not a file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(ProcessError::InvalidInput(
                "configured native program is not executable".into(),
            ));
        }
    }
    Ok(program)
}

fn sandbox_error(error: SandboxError) -> ProcessError {
    match error {
        SandboxError::Unsupported(_) => ProcessError::Unsupported,
        SandboxError::InvalidInput(message) => ProcessError::InvalidInput(message),
        SandboxError::Probe(message) => ProcessError::Io(message),
    }
}

#[async_trait]
impl ExecutionBackend for NativeBackend {
    async fn canonicalize(&self, path: &str) -> Result<String> {
        // Reject relative paths rather than letting Service cwd choose their meaning.
        if !std::path::Path::new(path).is_absolute() {
            return Err(ProcessError::InvalidInput(
                "native execution path must be absolute".into(),
            ));
        }
        let canonical = tokio::fs::canonicalize(path)
            .await
            .map_err(|_| ProcessError::Io("execution directory is unavailable".into()))?;
        if !tokio::fs::metadata(&canonical)
            .await
            .map_err(|_| ProcessError::Io("execution directory is unavailable".into()))?
            .is_dir()
        {
            return Err(ProcessError::InvalidInput(
                "execution path is not a directory".into(),
            ));
        }
        canonical
            .into_os_string()
            .into_string()
            .map_err(|_| ProcessError::InvalidInput("execution path is not UTF-8".into()))
    }
    async fn resolve_program(&self, selector: &str) -> Result<ResolvedProgram> {
        self.programs
            .get(selector)
            .cloned()
            .ok_or(ProcessError::Unsupported)
    }
    async fn resolve_local_program(&self, program: ResolvedProgram) -> Result<ResolvedProgram> {
        resolve_local_program(program).await
    }
    async fn prepare(
        &self,
        request: ProcessRequest,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<BackendPlan> {
        let process = self
            .capabilities
            .sandbox
            .confine(request)
            .await
            .map_err(sandbox_error)?;
        let stamp = process.stamp.clone();
        Ok(BackendPlan::new(process, stamp, environment))
    }
    async fn prepare_source_reader(
        &self,
        request: ProcessRequest,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<BackendPlan> {
        let process = self
            .capabilities
            .sandbox
            .confine_source_reader(request)
            .await
            .map_err(sandbox_error)?;
        let stamp = process.stamp.clone();
        Ok(BackendPlan::new(process, stamp, environment))
    }
    async fn spawn(
        &self,
        spec: ProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedProcess> {
        let spec =
            spec.try_map_process(|plan| Ok(pin.retain_native(plan.take::<ConfinedProcess>()?)))?;
        self.capabilities.process.spawn(spec).await
    }
    async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedDuplexProcess> {
        let spec =
            spec.try_map_process(|plan| Ok(pin.retain_native(plan.take::<ConfinedProcess>()?)))?;
        self.capabilities.duplex.spawn(spec).await
    }
    async fn spawn_pty(
        &self,
        spec: PtyProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedPtyProcess> {
        let spec =
            spec.try_map_process(|plan| Ok(pin.retain_native(plan.take::<ConfinedProcess>()?)))?;
        self.capabilities.pty.spawn(spec).await
    }
    async fn workspace_read(&self, request: WorkspaceReadRequest) -> Result<WorkspaceReadScope> {
        self.capabilities
            .sandbox
            .workspace_read(request)
            .await
            .map_err(sandbox_error)
    }
    fn files(&self) -> Arc<dyn Files> {
        self.capabilities.files.clone()
    }
}
