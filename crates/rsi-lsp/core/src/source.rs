use crate::{Error, Result};
use rsi_files_protocol::{FileKind, Files, FilesBinding, FilesCaller, RelativePath};
use rsi_sandbox::{Sandbox, SandboxMode, WorkspaceReadRequest};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
const MAXIMUM: usize = 1024 * 1024;
pub(crate) async fn read(
    files: &Arc<dyn Files>,
    sandbox: &Arc<dyn Sandbox>,
    workspace: &Path,
    execution: Option<&rsi_execution::ExecutionLease>,
    path: &str,
    stop: CancellationToken,
) -> Result<String> {
    crate::protocol::relative(path)?;
    if stop.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let request = WorkspaceReadRequest {
        cwd: workspace.to_owned(),
        workspace: workspace.to_owned(),
        mode: SandboxMode::ReadOnly,
    };
    let scope = if let Some(execution) = execution {
        execution
            .workspace_read(request)
            .await
            .map_err(crate::process_error)?
    } else {
        sandbox
            .workspace_read(request)
            .await
            .map_err(|_| Error::Unavailable)?
    };
    let files = execution
        .map_or_else(|| Ok(files.clone()), rsi_execution::ExecutionLease::files)
        .map_err(crate::process_error)?;
    let caller = FilesCaller::default();
    let _lease = CallerLease {
        files: files.clone(),
        caller: caller.clone(),
    };
    let binding = FilesBinding::new(caller.clone(), "rsi-lsp", "1", workspace.to_owned())
        .map_err(|_| Error::Invalid)?;
    let opened = files
        .open(
            binding.clone(),
            RelativePath::new(path.as_bytes()).map_err(|_| Error::Invalid)?,
            FileKind::File,
            stop.clone(),
        )
        .await
        .map_err(crate::files_error)?;
    let result = async {
        if opened.length > MAXIMUM as u64 {
            return Err(Error::Limit);
        }
        let mut bytes =
            Vec::with_capacity(usize::try_from(opened.length).map_err(|_| Error::Limit)?);
        let mut offset = 0;
        while offset < opened.length {
            let page = files
                .read(
                    binding.clone(),
                    opened.token.clone(),
                    offset,
                    65536,
                    stop.clone(),
                )
                .await
                .map_err(crate::files_error)?;
            let data = hex::decode(page.bytes_hex).map_err(|_| Error::Protocol)?;
            if data.is_empty() || page.offset != offset || page.total != opened.length {
                return Err(Error::Unavailable);
            }
            if bytes.len() + data.len() > MAXIMUM {
                return Err(Error::Limit);
            }
            offset += data.len() as u64;
            bytes.extend(data);
        }
        files
            .describe(&binding, &opened.token)
            .map_err(crate::files_error)?;
        String::from_utf8(bytes).map_err(|_| Error::Invalid)
    }
    .await;
    let _ = files.release(&binding, &opened.token);
    drop(scope);
    result
}

struct CallerLease {
    files: Arc<dyn Files>,
    caller: FilesCaller,
}
impl Drop for CallerLease {
    fn drop(&mut self) {
        self.files.release_caller(&self.caller);
    }
}
