//! Controlling-terminal execution; the consumer owns terminal emulation and policy.
use super::{
    Arc, ConfinedProcess, LocalContract, OsString, ProcessError, ProcessOutcome, Result,
    async_trait, fmt, validate_environment, validate_process_plan,
};

/// Maximum single PTY input or output payload.
pub const MAXIMUM_PTY_IO_BYTES: usize = 64 * 1024;
/// Explicit dimensions, revalidated before allocation and resize.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtySize {
    /// Character rows in 1..=200.
    pub rows: u16,
    /// Character columns in 1..=500.
    pub columns: u16,
}
impl PtySize {
    /// Checks dimension bounds before native calls.
    pub fn validate(self) -> Result<()> {
        if self.rows == 0 || self.rows > 200 || self.columns == 0 || self.columns > 500 {
            return Err(ProcessError::InvalidInput(
                "PTY size must be within 1..=200 rows and 1..=500 columns".into(),
            ));
        }
        Ok(())
    }
}
/// Exact controlling-terminal process request with no ambient environment merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtyProcessSpec {
    /// Restricted Sandbox plan with explicit PTY intent.
    pub process: ConfinedProcess,
    /// Complete child environment.
    pub environment: Vec<(OsString, OsString)>,
    /// Initial terminal dimensions.
    pub size: PtySize,
    /// TERM-to-KILL escalation grace in milliseconds.
    pub termination_grace_ms: u64,
}
impl PtyProcessSpec {
    /// Validates framing and the supported restricted Linux backend.
    pub fn validate(&self) -> Result<()> {
        validate_process_plan(&self.process)?;
        validate_environment(&self.environment)?;
        self.size.validate()?;
        if self.termination_grace_ms == 0
            || self.termination_grace_ms > super::MAXIMUM_PROCESS_GRACE_MS
        {
            return Err(ProcessError::InvalidInput(
                "PTY termination grace is out of bounds".into(),
            ));
        }
        if self.process.stdio != rsi_sandbox::ProcessStdio::Pty
            || !matches!(
                self.process.stamp.backend,
                rsi_sandbox::SandboxBackend::Bubblewrap { .. }
            )
            || !matches!(
                self.process.stamp.requested,
                rsi_sandbox::SandboxMode::ReadOnly | rsi_sandbox::SandboxMode::WorkspaceWrite
            )
        {
            return Err(ProcessError::Unsupported);
        }
        Ok(())
    }
}
/// One ordered raw output chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtyRead {
    /// Raw merged stdout/stderr bytes; at most 64 KiB.
    pub bytes: Vec<u8>,
    /// All native output has closed and queued bytes have been read.
    pub eof: bool,
}
/// Native terminal handle; registry and screen policy remain above Process.
#[async_trait]
pub trait PtyControl: fmt::Debug + Send + Sync + 'static {
    /// Native direct child identity, not a durable terminal ID.
    fn pid(&self) -> u32;
    /// Reads the next bounded chunk. Only one read may be in flight.
    async fn read(&self) -> Result<PtyRead>;
    /// Returns accepted bytes from one write. Cancellation leaves receipt unknown.
    async fn write(&self, bytes: &[u8]) -> Result<usize>;
    /// Changes dimensions and notifies the controlling terminal.
    fn resize(&self, size: PtySize) -> Result<()>;
    /// Begins TERM/KILL and closes blocked I/O admission.
    fn terminate(&self);
    /// Waits for direct-child reaping, group settlement and reader closure.
    async fn wait(&self) -> Result<ProcessOutcome>;
}
#[derive(Debug)]
struct PtyOwner(Arc<dyn PtyControl>);
impl Drop for PtyOwner {
    fn drop(&mut self) {
        self.0.terminate();
    }
}
/// Cloneable owner; dropping the final owner initiates cleanup.
#[derive(Clone, Debug)]
pub struct ManagedPtyProcess(Arc<PtyOwner>);
impl ManagedPtyProcess {
    /// Wraps one provider-owned native lifecycle.
    pub fn new(control: Arc<dyn PtyControl>) -> Self {
        Self(Arc::new(PtyOwner(control)))
    }
    /// Native direct child identity.
    pub fn pid(&self) -> u32 {
        self.0.0.pid()
    }
    /// Reads one bounded raw chunk.
    pub async fn read(&self) -> Result<PtyRead> {
        self.0.0.read().await
    }
    /// Writes once; accepted bytes do not imply shell command completion.
    pub async fn write(&self, bytes: &[u8]) -> Result<usize> {
        self.0.0.write(bytes).await
    }
    /// Resizes the controlling terminal.
    pub fn resize(&self, size: PtySize) -> Result<()> {
        self.0.0.resize(size)
    }
    /// Requests termination.
    pub fn terminate(&self) {
        self.0.0.terminate();
    }
    /// Waits for native cleanup.
    pub async fn wait(&self) -> Result<ProcessOutcome> {
        self.0.0.wait().await
    }
}
/// Local-only native PTY provider, sharing Process admission and retirement.
pub trait PtyProcess: fmt::Debug + Send + Sync + 'static {
    /// Admits one exact restricted PTY plan.
    fn spawn(&self, spec: PtyProcessSpec) -> Result<ManagedPtyProcess>;
}
/// Typed native PTY authority, never exposed by the output cache API.
#[derive(Debug)]
pub struct PtyProcessContract;
impl LocalContract for PtyProcessContract {
    const KEY: &'static str = "rsi.process.pty";
    type Service = dyn PtyProcess;
}
