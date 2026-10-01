use super::*;
use rsi_execution::{
    BackendPlan, ExecutionBackend, ExecutionLease, ExecutionPin, ExecutionProvider, ResolvedProgram,
};
use rsi_process::{
    DuplexProcessSpec, ManagedDuplexProcess, ManagedProcess, ProcessError, ProcessSpec,
};
use rsi_sandbox::{ProcessRequest, WorkspaceReadRequest, WorkspaceReadScope};

#[derive(Debug)]
struct Backend {
    process: Arc<Fake>,
    files: Arc<rsi_files::LocalFiles>,
}
#[async_trait]
impl ExecutionBackend for Backend {
    async fn canonicalize(&self, _: &str) -> rsi_process::Result<String> {
        unreachable!()
    }
    async fn resolve_program(&self, _: &str) -> rsi_process::Result<ResolvedProgram> {
        Ok(ResolvedProgram {
            program: "/target/bash".into(),
            environment: vec![],
        })
    }
    async fn prepare(
        &self,
        request: ProcessRequest,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> rsi_process::Result<BackendPlan> {
        assert_eq!(request.stdio, rsi_sandbox::ProcessStdio::Pty);
        Ok(BackendPlan::new(
            (),
            spawn_spec().process.stamp,
            environment,
        ))
    }
    async fn spawn(
        &self,
        _: ProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedProcess> {
        unreachable!()
    }
    async fn spawn_duplex(
        &self,
        _: DuplexProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedDuplexProcess> {
        unreachable!()
    }
    async fn spawn_pty(
        &self,
        spec: PtyProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> rsi_process::Result<ManagedPtyProcess> {
        spec.process.take::<()>()?;
        Ok(ManagedPtyProcess::new(self.process.clone()))
    }
    async fn workspace_read(
        &self,
        _: WorkspaceReadRequest,
    ) -> rsi_process::Result<WorkspaceReadScope> {
        unreachable!()
    }
    fn files(&self) -> Arc<dyn rsi_files_protocol::Files> {
        self.files.clone()
    }
}
#[derive(Debug, Default)]
struct Gate(AtomicBool);
impl rsi_execution::ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> rsi_process::Result<rsi_execution::ExecutionOperation> {
        if self.0.load(Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        Ok(rsi_execution::ExecutionOperation::new(()))
    }
}
fn provider(backend: Arc<Backend>, epoch: u64) -> ExecutionProvider {
    ExecutionProvider::new(
        rsi_execution::HostEpoch::from_bytes([1; 16]),
        rsi_execution::ExecutionLocation::Ssh {
            target: rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        1,
        epoch,
        backend,
    )
    .unwrap()
}
async fn plan(lease: &ExecutionLease) -> PtyProcessSpec<rsi_execution::PreparedProcess> {
    let native = spawn_spec();
    let program = lease.resolve_program("terminal").await.unwrap();
    let process = lease
        .prepare(ProcessRequest {
            program,
            arguments: vec![],
            stdio: rsi_sandbox::ProcessStdio::Pty,
            mode: rsi_sandbox::SandboxMode::WorkspaceWrite,
            cwd: native.process.cwd.clone(),
            workspace: native.process.cwd,
        })
        .await
        .unwrap();
    PtyProcessSpec {
        process,
        environment: vec![],
        size: native.size,
        termination_grace_ms: 50,
    }
}

#[tokio::test]
async fn scope_uses_current_attachment_caller_and_never_retargets_an_old_terminal() {
    let (scope, _, _) = fixture();
    scope.execute(Operation::CloseAll).await.unwrap();
    let files = Arc::new(rsi_files::LocalFiles::new().unwrap());
    let target = Arc::new(Fake::default());
    let backend = Arc::new(Backend {
        process: target.clone(),
        files: files.clone(),
    });
    let provider_a = provider(backend.clone(), 1);
    let gate_a = Arc::new(Gate::default());
    let creator = provider_a.lease(gate_a.clone()).unwrap();
    let current = provider_a.lease(Arc::new(Gate::default())).unwrap();
    let created = scope.create_execution(plan(&creator).await).await.unwrap();
    gate_a.0.store(true, Ordering::SeqCst);
    let Reply::Attached(attached) = scope
        .execute(Operation::Attach {
            terminal: created.terminal.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let Reply::Terminal(control) = scope
        .execute(Operation::Takeover {
            terminal: created.terminal.id.clone(),
            attachment: attached.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let input = Operation::Input {
        terminal: control.id.clone(),
        attachment: attached.id.clone(),
        epoch: control.controller_epoch,
        sequence: 1,
        bytes: b"ok".to_vec(),
    };
    assert!(scope.execute(input.clone()).await.is_err());
    assert!(scope.execute_with(input.clone(), creator).await.is_err());
    let replacement = provider(backend, 2)
        .lease(Arc::new(Gate::default()))
        .unwrap();
    assert!(
        scope
            .execute_with(input.clone(), replacement.clone())
            .await
            .is_err()
    );
    assert!(lock(&target.writes).is_empty());
    let Reply::Input(receipt) = scope.execute_with(input, current.clone()).await.unwrap() else {
        panic!()
    };
    assert_eq!(receipt.result, InputState::Accepted { bytes: 2 });
    let resize = Operation::Resize {
        terminal: control.id,
        attachment: attached.id,
        epoch: control.controller_epoch,
        size: Size {
            rows: 30,
            columns: 90,
        },
    };
    assert!(
        scope
            .execute_with(resize.clone(), replacement)
            .await
            .is_err()
    );
    let Reply::Terminal(resized) = scope.execute_with(resize, current).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        resized.size,
        Size {
            rows: 30,
            columns: 90
        }
    );
    assert!(!target.stopped.load(Ordering::SeqCst));
    scope.retire().await.unwrap();
    assert!(target.stopped.load(Ordering::SeqCst));
    files.close().await;
}
