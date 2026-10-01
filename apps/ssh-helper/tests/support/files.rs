use rsi_execution::{ExecutionAdmission, ExecutionOperation, ExecutionTargetId, HostEpoch};
use rsi_files_protocol::{FileKind, FilesBinding, FilesCaller, FilesError, RelativePath};
use rsi_ssh_client::ProcessConnection;
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct Gate(CancellationToken);
impl ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<ExecutionOperation> {
        if self.0.is_cancelled() {
            return Err(rsi_process::ProcessError::ShuttingDown);
        }
        Ok(ExecutionOperation::new(()))
    }
}
pub async fn verify(client: &ProcessConnection, workspace: &Path) {
    std::fs::write(workspace.join("files-proof"), b"target\0bytes").unwrap();
    let provider = || {
        rsi_ssh_client::execution_provider(
            HostEpoch::from_bytes([1; 16]),
            client.clone(),
            ExecutionTargetId::parse("a".repeat(32)).unwrap(),
            1,
        )
        .unwrap()
    };
    let owner = provider();
    let creator = Arc::new(Gate::default());
    let first = owner.lease(creator.clone()).unwrap();
    let resource = first.retain_files().unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "files-session",
        "header",
        workspace.to_path_buf(),
    )
    .unwrap();
    let file = resource
        .view(&first)
        .unwrap()
        .open(
            binding.clone(),
            RelativePath::new(b"files-proof").unwrap(),
            FileKind::File,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(first);
    creator.0.cancel();
    let current = Arc::new(Gate::default());
    let second = owner.lease(current.clone()).unwrap();
    let view = resource.view(&second).unwrap();
    let page = view
        .read(
            binding.clone(),
            file.token.clone(),
            0,
            12,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(hex::decode(page.bytes_hex).unwrap(), b"target\0bytes");
    drop(view);
    let view = resource.view(&second).unwrap();
    assert_eq!(view.describe(&binding, &file.token).unwrap(), file);
    let other = provider().lease(Arc::new(Gate::default())).unwrap();
    assert!(matches!(
        resource.view(&other),
        Err(rsi_process::ProcessError::InvalidInput(_))
    ));
    current.0.cancel();
    assert_eq!(
        view.read(
            binding.clone(),
            file.token.clone(),
            0,
            1,
            CancellationToken::new()
        )
        .await,
        Err(FilesError::Cancelled)
    );
    view.release(&binding, &file.token).unwrap();
    drop((view, second, resource));
}
