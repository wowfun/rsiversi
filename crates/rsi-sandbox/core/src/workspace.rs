use crate::{Result, SandboxError, SandboxMode};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Opaque process-local provider generation; identity is preserved only by cloning.
#[derive(Clone, Default)]
pub struct SandboxGeneration(Arc<()>);
impl fmt::Debug for SandboxGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxGeneration").finish_non_exhaustive()
    }
}
impl PartialEq for SandboxGeneration {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for SandboxGeneration {}

/// Explicit orchestrator inputs to a workspace-only read plan.
#[derive(Clone, Debug)]
pub struct WorkspaceReadRequest {
    /// Exact resolved file-effect mode.
    pub mode: SandboxMode,
    /// Native normalized working directory.
    pub cwd: PathBuf,
    /// Native normalized workspace root.
    pub workspace: PathBuf,
}

/// Immutable read scope issued by a trusted Sandbox provider; never decoded from model data.
#[derive(Clone, Debug)]
pub struct WorkspaceReadScope {
    request: WorkspaceReadRequest,
    generation: SandboxGeneration,
}
impl WorkspaceReadScope {
    /// Validates a provider's native request and retains its exact generation identity.
    pub fn new(request: WorkspaceReadRequest, generation: SandboxGeneration) -> Result<Self> {
        if !request.cwd.is_absolute()
            || !request.workspace.is_absolute()
            || !rsi_workspace_path::is_normalized_absolute_path(&request.cwd)
            || !rsi_workspace_path::is_normalized_absolute_path(&request.workspace)
            || !request.cwd.starts_with(&request.workspace)
        {
            return Err(SandboxError::InvalidInput(
                "workspace reads require native normalized paths with cwd inside workspace".into(),
            ));
        }
        Ok(Self {
            request,
            generation,
        })
    }
    /// Exact resolved mode; every mode permits workspace-only reads.
    pub const fn mode(&self) -> SandboxMode {
        self.request.mode
    }
    /// Exact working directory, without reopening or canonicalizing it.
    pub fn cwd(&self) -> &Path {
        &self.request.cwd
    }
    /// Exact workspace, without reopening or canonicalizing it.
    pub fn workspace(&self) -> &Path {
        &self.request.workspace
    }
    /// Provider generation retained by this scope.
    pub const fn generation(&self) -> &SandboxGeneration {
        &self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_identity_is_opaque_and_scope_rejects_foreign_roots_and_lexical_escape() {
        let generation = SandboxGeneration::default();
        assert_eq!(generation, generation.clone());
        assert_ne!(generation, SandboxGeneration::default());
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\workspace")
        } else {
            PathBuf::from("/workspace")
        };
        for mode in [
            SandboxMode::ReadOnly,
            SandboxMode::WorkspaceWrite,
            SandboxMode::DangerFullAccess,
        ] {
            let request = WorkspaceReadRequest {
                mode,
                cwd: root.join("child"),
                workspace: root.clone(),
            };
            let scope = WorkspaceReadScope::new(request, generation.clone()).unwrap();
            assert_eq!(scope.generation(), &generation);
            assert_eq!(scope.mode(), mode);
            assert_eq!(scope.cwd(), root.join("child"));
            assert_eq!(scope.workspace(), root);
            for cwd in [
                PathBuf::from("relative"),
                root.join(".."),
                root.with_file_name("workspace-other"),
            ] {
                assert!(
                    WorkspaceReadScope::new(
                        WorkspaceReadRequest {
                            mode,
                            cwd,
                            workspace: root.clone()
                        },
                        generation.clone()
                    )
                    .is_err()
                );
            }
        }
    }
}
