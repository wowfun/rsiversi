//! Native Windows ACL plans; grant policy stays with Sandbox.
use super::*;
use rsi_sandbox::{ProcessPlanOwner, ProcessStdio};
mod acl;
mod native;
mod token;

#[derive(Debug)]
struct TemporaryGrant {
    directory: Option<tempfile::TempDir>,
    sid: String,
}
impl Drop for TemporaryGrant {
    fn drop(&mut self) {
        let Some(directory) = self.directory.take() else {
            return;
        };
        let sid = std::mem::take(&mut self.sid);
        let cleanup = move || {
            let path = acl::canonical(directory.path().to_owned());
            let revoked = native::sid(&sid).and_then(|sid| acl::revoke_private(&path, &sid));
            if let Err(error) = directory.close() {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr(),
                    "private Sandbox cleanup failed at {}: {error}; ACL revocation: {revoked:?}",
                    path.display()
                );
            }
        };
        // A potentially populated private tree must not be walked on an async
        // worker. The blocking operation owns it until revoke/removal completes.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(cleanup);
        } else {
            cleanup();
        }
    }
}

pub(super) fn confine(
    request: ProcessRequest,
    program: PathBuf,
    cwd: PathBuf,
    workspace: PathBuf,
) -> Result<ConfinedProcess> {
    if request.stdio != ProcessStdio::Pipes {
        return Err(SandboxError::Unsupported(request.mode));
    }
    let workspace = acl::canonical(workspace);
    let result = prepare(request.mode, &workspace)
        .map_err(|error| SandboxError::Native(format!("{}: {error}", workspace.display())))?;
    let (owner, scratch, filesystem) = result;
    let stamp = EnforcementStamp {
        requested: request.mode,
        backend: SandboxBackend::WindowsAcl,
        workspace,
        filesystem,
        scratch,
        network: SandboxNetwork::Host,
    };
    Ok(ConfinedProcess {
        owner: Some(owner),
        stdio: ProcessStdio::Pipes,
        program,
        arguments: request.arguments.into_iter().map(OsString::from).collect(),
        cwd,
        stamp,
    })
}

fn prepare(
    mode: SandboxMode,
    workspace: &Path,
) -> std::io::Result<(ProcessPlanOwner, SandboxScratch, SandboxFileSystem)> {
    let root = std::env::temp_dir();
    if !root.is_absolute() {
        return Err(std::io::Error::other(
            "Windows temporary root must be absolute",
        ));
    }
    let root = acl::canonical(root.canonicalize()?);
    if root.starts_with(workspace) {
        return Err(std::io::Error::other(
            "Windows private temporary root must be outside the workspace",
        ));
    }
    let directory = tempfile::Builder::new()
        .prefix("rsi-sandbox-")
        .tempdir_in(root)?;
    let temporary = acl::canonical(directory.path().canonicalize()?);
    let grant = TemporaryGrant {
        sid: acl::write_sid(&temporary, true)?,
        directory: Some(directory),
    };
    let temp_sid = native::sid(&grant.sid)?;
    acl::grant_private(&temporary, &temp_sid)?;
    let (token, filesystem) = if mode == SandboxMode::WorkspaceWrite {
        let workspace_sid = native::sid(&acl::write_sid(workspace, false)?)?;
        acl::grant_workspace(workspace, &workspace_sid)?;
        (
            token::restricted(&workspace_sid, Some(&temp_sid))?,
            SandboxFileSystem::WindowsAclWorkspaceWrite,
        )
    } else {
        (
            token::restricted(&temp_sid, None)?,
            SandboxFileSystem::WindowsAclReadOnly,
        )
    };
    Ok((
        ProcessPlanOwner::windows(grant, token, temporary),
        SandboxScratch::PrivateDirectory,
        filesystem,
    ))
}
