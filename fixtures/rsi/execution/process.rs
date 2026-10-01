//! Recording target tuple for consumer routing tests; only batch execution is admitted.
use async_trait::async_trait;
use rsi_execution::{BackendPlan, ExecutionBackend, ExecutionPin, ResolvedProgram};
use rsi_process::*;
use rsi_sandbox::{
    ConfinedProcess, ProcessRequest, Sandbox, WorkspaceReadRequest, WorkspaceReadScope,
};
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
pub struct Backend {
    pub sandbox: Arc<dyn Sandbox>,
    pub process: Arc<dyn Process>,
    pub files: Arc<dyn rsi_files_protocol::Files>,
    pub program: ResolvedProgram,
    pub resolved: AtomicUsize,
    pub prepared: AtomicUsize,
    pub spawned: AtomicUsize,
}
#[async_trait]
impl ExecutionBackend for Backend {
    async fn canonicalize(&self, _: &str) -> Result<String> {
        panic!("process test must not canonicalize a target path")
    }
    async fn resolve_program(&self, selector: &str) -> Result<ResolvedProgram> {
        assert_eq!(selector, "bash");
        self.resolved.fetch_add(1, Ordering::SeqCst);
        Ok(self.program.clone())
    }
    async fn prepare(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<BackendPlan> {
        assert_eq!(request.program, self.program.program);
        assert_eq!(environment, self.program.environment);
        self.prepared.fetch_add(1, Ordering::SeqCst);
        let plan = self
            .sandbox
            .confine(request)
            .await
            .map_err(|e| ProcessError::InvalidInput(e.to_string()))?;
        Ok(BackendPlan::new(plan.clone(), plan.stamp, environment))
    }
    async fn spawn(
        &self,
        spec: ProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedProcess> {
        assert_eq!(spec.environment, self.program.environment);
        self.spawned.fetch_add(1, Ordering::SeqCst);
        self.process
            .spawn(
                spec.try_map_process(
                    |plan| Ok(pin.retain_native(plan.take::<ConfinedProcess>()?)),
                )?,
            )
            .await
    }
    async fn spawn_duplex(
        &self,
        _: DuplexProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> Result<ManagedDuplexProcess> {
        panic!("batch test spawned duplex")
    }
    async fn spawn_pty(
        &self,
        _: PtyProcessSpec<BackendPlan>,
        _: ExecutionPin,
    ) -> Result<ManagedPtyProcess> {
        panic!("batch test spawned PTY")
    }
    async fn workspace_read(&self, _: WorkspaceReadRequest) -> Result<WorkspaceReadScope> {
        panic!("process test read workspace")
    }
    fn files(&self) -> Arc<dyn rsi_files_protocol::Files> {
        self.files.clone()
    }
}

#[derive(Debug, Default)]
pub struct Gate(pub std::sync::atomic::AtomicBool);
impl rsi_execution::ExecutionAdmission for Gate {
    fn admit(
        &self,
        _kind: rsi_execution::ExecutionAdmissionKind,
    ) -> Result<rsi_execution::ExecutionOperation> {
        if self.0.load(Ordering::SeqCst) {
            return Err(ProcessError::ShuttingDown);
        }
        Ok(rsi_execution::ExecutionOperation::new(()))
    }
}
