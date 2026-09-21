//! Ongoing byte-protocol seam, independent of batch stdin and lossy tail readers.
use super::{
    Arc, ConfinedProcess, LocalContract, OsString, ProcessOutcome, ProcessOutput, ProcessSpec,
    Result, async_trait, fmt, validate_capture, validate_environment, validate_pipe_plan,
};
/// Maximum one duplex read or write chunk.
pub const MAXIMUM_DUPLEX_CHUNK_BYTES: usize = 64 * 1024;
/// Fully explicit ongoing protocol process request after Sandbox confinement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplexProcessSpec {
    /// Exact confined executable, argv and cwd.
    pub process: ConfinedProcess,
    /// Complete child environment; never merged with ambient values.
    pub environment: Vec<(OsString, OsString)>,
    /// Lossless queued stdout capacity, within the ordinary per-stream ceiling.
    pub stdout_buffer_bytes: usize,
    /// Retained stderr tail capacity.
    pub stderr_max_bytes: usize,
    /// TERM/KILL and final pipe drain grace.
    pub termination_grace_ms: u64,
}
impl DuplexProcessSpec {
    /// Validates the same process, environment and capture admission invariants.
    pub fn validate(&self) -> Result<()> {
        validate_pipe_plan(&self.process)?;
        validate_environment(&self.environment)?;
        validate_capture(
            self.stdout_buffer_bytes,
            self.stderr_max_bytes,
            self.termination_grace_ms,
        )
    }
    /// Shares only mechanical request validation/admission with batch execution.
    pub fn into_process_spec(self) -> ProcessSpec {
        ProcessSpec {
            process: self.process,
            stdin: vec![],
            environment: self.environment,
            stdout_max_bytes: self.stdout_buffer_bytes,
            stderr_max_bytes: self.stderr_max_bytes,
            termination_grace_ms: self.termination_grace_ms,
        }
    }
}
/// Persistent input. A partial or cancelled write never establishes replay safety.
#[async_trait]
pub trait DuplexInput: fmt::Debug + Send + Sync + 'static {
    /// Writes at most 64 KiB, returning the exact nonzero accepted prefix.
    async fn write(&self, bytes: &[u8]) -> Result<usize>;
    /// Explicitly closes stdin after prior accepted writes.
    async fn close(&self) -> Result<()>;
}
/// One ordered, consuming stdout read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplexRead {
    /// Exact next bytes. Empty only at clean EOF.
    pub bytes: Vec<u8>,
    /// True when stdout reached EOF and its buffered bytes are delivered,
    /// independently of child/stderr settlement reported by `wait`.
    pub eof: bool,
}
/// One bounded lossless stdout stream; it admits only one pending reader.
#[async_trait]
pub trait DuplexOutput: fmt::Debug + Send + Sync + 'static {
    /// Consumes at most the requested 1..=64 KiB bytes without skipping output.
    async fn read(&self, maximum: usize) -> Result<DuplexRead>;
}
/// Provider-owned ongoing process control and retained byte ports.
#[async_trait]
pub trait DuplexControl: fmt::Debug + Send + Sync + 'static {
    /// Actual direct child/group-leader identity.
    fn pid(&self) -> u32;
    /// Persistent input port.
    fn stdin(&self) -> Arc<dyn DuplexInput>;
    /// Lossless ordered stdout port.
    fn stdout(&self) -> Arc<dyn DuplexOutput>;
    /// Bounded stderr tail.
    fn stderr(&self) -> Arc<dyn ProcessOutput>;
    /// Cancels pipe work and starts idempotent TERM/KILL escalation.
    fn terminate(&self);
    /// Waits for direct-child reaping and bounded pipe/group settlement.
    async fn wait(&self) -> Result<ProcessOutcome>;
    /// Waits for reaping, group disappearance and pipe-task joins, without claiming lossless EOF.
    async fn wait_settlement(&self) -> Result<()>;
}
/// Cloneable handle to one exact ongoing protocol process.
#[derive(Clone, Debug)]
pub struct ManagedDuplexProcess(Arc<DuplexOwner>);
#[derive(Debug)]
struct DuplexOwner(Arc<dyn DuplexControl>);
impl Drop for DuplexOwner {
    fn drop(&mut self) {
        self.0.terminate();
    }
}
impl ManagedDuplexProcess {
    /// Constructs a handle from the owning provider's control object.
    pub fn new(control: Arc<dyn DuplexControl>) -> Self {
        Self(Arc::new(DuplexOwner(control)))
    }
    /// Actual direct-child identity.
    pub fn pid(&self) -> u32 {
        self.0.0.pid()
    }
    /// Persistent input port.
    pub fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.0.0.stdin()
    }
    /// Consuming lossless output port.
    pub fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.0.0.stdout()
    }
    /// Bounded stderr tail.
    pub fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.0.0.stderr()
    }
    /// Starts cancellation and group escalation.
    pub fn terminate(&self) {
        self.0.0.terminate();
    }
    /// Waits for resource cleanup without claiming that protocol output reached clean EOF.
    pub async fn wait_settlement(&self) -> Result<()> {
        self.0.0.wait_settlement().await
    }
    /// Waits for reaping and settlement.
    pub async fn wait(&self) -> Result<ProcessOutcome> {
        self.0.0.wait().await
    }
}
/// Sibling process provider for byte protocols, sharing batch-process admission.
pub trait DuplexProcess: fmt::Debug + Send + Sync + 'static {
    /// Validates and admits an explicitly confined ongoing process.
    fn spawn(&self, spec: DuplexProcessSpec) -> Result<ManagedDuplexProcess>;
}
/// Local-only protocol process authority; never supplied by the output API.
#[derive(Debug)]
pub struct DuplexProcessContract;
impl LocalContract for DuplexProcessContract {
    const KEY: &'static str = "rsi.process.duplex";
    type Service = dyn DuplexProcess;
}
