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
/// Program paths are absolute invocation spellings; Sandbox owns filesystem admission.
/// At most 128 selectors and 1 MiB of aggregate child environment are retained.
pub fn provider(
    host_epoch: rsi_execution::HostEpoch,
    capabilities: NativeCapabilities,
    programs: BTreeMap<String, ResolvedProgram>,
) -> Result<ExecutionProvider> {
    if programs.len() > 128 {
        return Err(ProcessError::Capacity);
    }
    let mut environment_bytes = 0usize;
    for (selector, program) in &programs {
        if selector.is_empty() || selector.len() > 128 || selector.chars().any(char::is_control) {
            return Err(ProcessError::InvalidInput(
                "invalid native program selector or path".into(),
            ));
        }
        validate_program(program)?;
        for (name, value) in &program.environment {
            environment_bytes = environment_bytes
                .saturating_add(name.len())
                .saturating_add(value.len());
        }
        if environment_bytes > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
            return Err(ProcessError::Capacity);
        }
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

fn validate_program(program: &ResolvedProgram) -> Result<()> {
    if !program.program.is_absolute()
        || program.program.as_os_str().len() > rsi_sandbox::MAXIMUM_SANDBOX_PLAN_BYTES
        || program.program.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err(ProcessError::InvalidInput(
            "native program path must be bounded, absolute and NUL-free".into(),
        ));
    }
    rsi_process::validate_environment(&program.environment)
}

fn sandbox_error(error: SandboxError) -> ProcessError {
    match error {
        SandboxError::Capacity => ProcessError::Capacity,
        other => ProcessError::Sandbox(other),
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
        rsi_workspace_path::canonical_native_path(canonical)
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
        validate_program(&program)?;
        Ok(program)
    }
    async fn prepare(
        &self,
        request: ProcessRequest,
        mut environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<BackendPlan> {
        let process = self
            .capabilities
            .sandbox
            .confine(request)
            .await
            .map_err(sandbox_error)?;
        let stamp = process.stamp.clone();
        if process.prepare_environment(&mut environment) {
            rsi_process::validate_environment(&environment)?;
        }
        Ok(BackendPlan::new(process, stamp, environment))
    }
    async fn prepare_source_reader(
        &self,
        request: ProcessRequest,
        mut environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<BackendPlan> {
        let process = self
            .capabilities
            .sandbox
            .confine_source_reader(request)
            .await
            .map_err(sandbox_error)?;
        let stamp = process.stamp.clone();
        if process.prepare_environment(&mut environment) {
            rsi_process::validate_environment(&environment)?;
        }
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

#[cfg(test)]
mod preparation_errors {
    use super::*;
    #[test]
    fn program_capture_bounds_paths_and_environment_without_accessing_the_file() {
        let mut program = ResolvedProgram {
            program: if cfg!(windows) {
                r"C:\not-created\command.exe".into()
            } else {
                "/not-created/command".into()
            },
            environment: vec![("KEPT".into(), "value".into())],
        };
        validate_program(&program).unwrap();
        let original = program.program.clone();
        for invalid in [
            "relative".into(),
            format!("{}\0suffix", original.display()).into(),
            format!(
                "{}{}",
                original.display(),
                "x".repeat(rsi_sandbox::MAXIMUM_SANDBOX_PLAN_BYTES)
            )
            .into(),
        ] {
            program.program = invalid;
            assert!(validate_program(&program).is_err());
        }
        program.program = original;
        program
            .environment
            .push(("BAD=NAME".into(), "value".into()));
        assert!(validate_program(&program).is_err());
    }

    #[test]
    fn sandbox_preparation_preserves_capacity_and_typed_causes() {
        assert!(matches!(
            sandbox_error(SandboxError::Capacity),
            ProcessError::Capacity
        ));
        assert!(matches!(
            sandbox_error(SandboxError::Preparation),
            ProcessError::Sandbox(SandboxError::Preparation)
        ));
        let cause = SandboxError::Probe("SetNamedSecurityInfoW: access denied (5)".into());
        assert_eq!(sandbox_error(cause.clone()), ProcessError::Sandbox(cause));
    }
}
