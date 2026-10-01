//! Resource lifetime is independent of the caller admitted for one controller operation.
use rsi_execution::{ExecutionLease, ExecutionPty, ExecutionPtyView, PreparedProcess};
use rsi_process::{
    ManagedPtyProcess, ProcessError, ProcessOutcome, PtyProcess, PtyProcessSpec, PtyRead, PtySize,
    Result,
};

pub(super) enum Launch {
    Native(PtyProcessSpec),
    Execution(PtyProcessSpec<PreparedProcess>),
}
impl Launch {
    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Native(spec) => spec.validate(),
            Self::Execution(spec) => {
                spec.size.validate()?;
                rsi_process::validate_environment(&spec.environment)?;
                if spec.termination_grace_ms == 0
                    || spec.termination_grace_ms > rsi_process::MAXIMUM_PROCESS_GRACE_MS
                {
                    return Err(ProcessError::InvalidInput(
                        "PTY termination grace is out of bounds".into(),
                    ));
                }
                Ok(())
            }
        }
    }
    pub(super) fn size(&self) -> PtySize {
        match self {
            Self::Native(spec) => spec.size,
            Self::Execution(spec) => spec.size,
        }
    }
    pub(super) async fn spawn(self, native: &dyn PtyProcess) -> Result<TermProcess> {
        match self {
            Self::Native(spec) => native.spawn(spec).await.map(TermProcess::Native),
            Self::Execution(spec) => spec
                .process
                .lease()
                .spawn_terminal(spec)
                .await
                .map(TermProcess::Execution),
        }
    }
}
#[derive(Debug)]
pub(super) enum TermProcess {
    Native(ManagedPtyProcess),
    Execution(ExecutionPty),
}
impl TermProcess {
    pub(super) fn view(&self, lease: Option<&ExecutionLease>) -> Result<OperationView> {
        match (self, lease) {
            (Self::Native(process), None) => Ok(OperationView::Native(process.clone())),
            (Self::Execution(process), Some(lease)) => {
                process.view(lease).map(OperationView::Execution)
            }
            _ => Err(ProcessError::InvalidInput(
                "terminal requires its exact operation execution lease".into(),
            )),
        }
    }
    pub(super) fn terminate(&self) {
        match self {
            Self::Native(process) => process.terminate(),
            Self::Execution(process) => process.terminate(),
        }
    }
    pub(super) async fn read(&self) -> Result<PtyRead> {
        match self {
            Self::Native(process) => process.read().await,
            Self::Execution(process) => process.read_output().await,
        }
    }
    pub(super) async fn wait(&self) -> Result<ProcessOutcome> {
        match self {
            Self::Native(process) => process.wait().await,
            Self::Execution(process) => process.wait().await,
        }
    }
}

pub(super) enum OperationView {
    Native(ManagedPtyProcess),
    Execution(ExecutionPtyView),
}
impl OperationView {
    pub(super) async fn write(&self, bytes: &[u8]) -> Result<usize> {
        match self {
            Self::Native(view) => view.write(bytes).await,
            Self::Execution(view) => view.write(bytes).await,
        }
    }
    pub(super) async fn resize(&self, size: PtySize) -> Result<()> {
        match self {
            Self::Native(view) => view.resize(size).await,
            Self::Execution(view) => view.resize(size).await,
        }
    }
}
