//! Runtime-independent process-local background Jobs contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use thiserror::Error;

/// Maximum job, producer, or scope-component bytes.
pub const MAXIMUM_JOB_IDENTIFIER_BYTES: usize = 256;
/// Maximum components in one generic owner scope.
pub const MAXIMUM_JOB_SCOPE_COMPONENTS: usize = 8;
/// Default simultaneous live jobs in one authority generation.
pub const DEFAULT_MAXIMUM_ACTIVE_JOBS_PER_SCOPE: usize = 10;
/// Default simultaneous live jobs in one provider generation.
pub const DEFAULT_MAXIMUM_ACTIVE_JOBS: usize = 256;
/// Default retained job records in one authority generation.
pub const DEFAULT_MAXIMUM_RETAINED_JOBS_PER_SCOPE: usize = 256;
/// Default retained job records in one provider generation.
pub const DEFAULT_MAXIMUM_RETAINED_JOBS: usize = 1_024;
/// Maximum records returned by one exact-scope list.
pub const MAXIMUM_JOBS_PER_LIST: usize = 256;

/// Generic bounded owner identity used for isolated authority and cleanup.
///
/// Components remain separate so higher layers preserve exact identities
/// without delimiter escaping or a dependency from Jobs back to those layers.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct JobScopeId {
    namespace: String,
    components: Vec<String>,
}

impl JobScopeId {
    /// Creates one scope from an identifier namespace and exact components.
    pub fn new<I, S>(namespace: impl Into<String>, components: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let namespace = namespace.into();
        validate_identifier("job scope namespace", &namespace)?;
        let components = components.into_iter().map(Into::into).collect::<Vec<_>>();
        if components.is_empty() || components.len() > MAXIMUM_JOB_SCOPE_COMPONENTS {
            return Err(JobsError::InvalidInput(format!(
                "job scope must contain 1..={MAXIMUM_JOB_SCOPE_COMPONENTS} components"
            )));
        }
        for component in &components {
            validate_identifier("job scope component", component)?;
        }
        Ok(Self {
            namespace,
            components,
        })
    }

    /// Returns the owning namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the exact ordered owner components.
    pub fn components(&self) -> &[String] {
        &self.components
    }
}

/// Provider-owned revocation cell behind one scope-authority generation.
///
/// This is exposed only for provider implementations. Application code should
/// use [`JobScopeAuthority`] exclusively.
#[doc(hidden)]
#[derive(Debug)]
pub struct JobScopeAuthorityState {
    provider_id: u64,
    generation: u64,
    revoked: AtomicBool,
}

impl JobScopeAuthorityState {
    /// Revokes the provider-owned state without reconstructing an authority.
    #[doc(hidden)]
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
}

/// Opaque cloneable process-local authority for one Jobs scope generation.
///
/// It deliberately implements neither serialization nor construction from a
/// [`JobScopeId`]. Identity locates a scope; this value authorizes operations.
#[derive(Clone)]
pub struct JobScopeAuthority {
    id: JobScopeId,
    state: Arc<JobScopeAuthorityState>,
}

impl JobScopeAuthority {
    /// Returns the bounded human-readable scope identity.
    pub const fn id(&self) -> &JobScopeId {
        &self.id
    }

    /// Creates provider-owned authority state.
    #[doc(hidden)]
    pub fn provider_owned(id: JobScopeId, provider_id: u64, generation: u64) -> Self {
        Self {
            id,
            state: Arc::new(JobScopeAuthorityState {
                provider_id,
                generation,
                revoked: AtomicBool::new(false),
            }),
        }
    }

    /// Rebuilds a provider handle from its live weak lookup.
    #[doc(hidden)]
    pub fn from_provider_state(id: JobScopeId, state: Arc<JobScopeAuthorityState>) -> Self {
        Self { id, state }
    }

    /// Returns a weak provider lookup.
    #[doc(hidden)]
    pub fn provider_state(&self) -> Weak<JobScopeAuthorityState> {
        Arc::downgrade(&self.state)
    }

    /// Returns whether this authority belongs to one provider.
    #[doc(hidden)]
    pub fn belongs_to(&self, provider_id: u64) -> bool {
        self.state.provider_id == provider_id
    }

    /// Returns the provider-local authority generation.
    #[doc(hidden)]
    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    /// Returns whether two values share the exact authority generation.
    #[doc(hidden)]
    pub fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Returns whether this value owns supplied provider state.
    #[doc(hidden)]
    pub fn owns_provider_state(&self, state: &Arc<JobScopeAuthorityState>) -> bool {
        Arc::ptr_eq(&self.state, state)
    }

    /// Revokes every clone of this exact authority generation.
    #[doc(hidden)]
    pub fn revoke(&self) {
        self.state.revoke();
    }

    /// Returns whether this exact authority generation remains open.
    pub fn is_active(&self) -> bool {
        !self.state.revoked.load(Ordering::Acquire)
    }
}

impl fmt::Debug for JobScopeAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobScopeAuthority")
            .field("id", &self.id)
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}

/// Type-erased trusted request passed only to its named producer.
#[derive(Clone)]
pub struct JobRequest {
    type_name: &'static str,
    value: Arc<dyn Any + Send + Sync>,
}

impl JobRequest {
    /// Wraps one process-local typed request.
    pub fn new<T>(value: T) -> Self
    where
        T: fmt::Debug + Send + Sync + 'static,
    {
        Self {
            type_name: std::any::type_name::<T>(),
            value: Arc::new(value),
        }
    }

    /// Borrows the request when its concrete type matches.
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.value.downcast_ref()
    }
}

impl fmt::Debug for JobRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobRequest")
            .field("type_name", &self.type_name)
            .finish_non_exhaustive()
    }
}

/// Latest status of one process-local job.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Producer control has been published and remains live.
    Running,
    /// Cancellation was requested and settlement is pending.
    Stopping,
    /// Work completed successfully.
    Completed,
    /// Work settled with an execution failure.
    Failed,
    /// Work settled after cancellation.
    Cancelled,
}

impl JobStatus {
    /// Returns whether this status is terminal.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Raw producer output stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Terminal facts retained independently of producer control.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobTerminal {
    /// Terminal status.
    pub status: JobStatus,
    /// Optional process-style exit code.
    pub exit_code: Option<i32>,
    /// Optional process-style terminating signal.
    pub signal: Option<i32>,
    /// Optional bounded producer diagnostic.
    pub message: Option<String>,
}

impl JobTerminal {
    /// Validates terminal invariants.
    pub fn validate(&self) -> Result<()> {
        if !self.status.is_terminal() {
            return Err(JobsError::InvalidInput(
                "job terminal status must be terminal".into(),
            ));
        }
        if self.status == JobStatus::Completed
            && (self.signal.is_some() || self.exit_code.is_some_and(|exit_code| exit_code != 0))
        {
            return Err(JobsError::InvalidInput(
                "completed job cannot carry a signal or nonzero exit code".into(),
            ));
        }
        if self.message.as_ref().is_some_and(|message| {
            message.len() > MAXIMUM_JOB_IDENTIFIER_BYTES * 16
                || message.chars().any(|character| character == '\0')
        }) {
            return Err(JobsError::InvalidInput(
                "job terminal message is invalid or too large".into(),
            ));
        }
        Ok(())
    }
}

/// One raw offset-based output read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobOutputRead {
    /// Retained bytes at or after the requested whole-stream offset.
    pub bytes: Vec<u8>,
    /// Oldest whole-stream offset still retained.
    pub oldest_offset: u64,
    /// Whole-stream offset immediately after the current stream tail.
    pub next_offset: u64,
    /// Whether requested bytes were already dropped.
    pub lossy: bool,
}

/// Immutable externally visible job facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSummary {
    /// Process-local identifier.
    pub id: String,
    /// Human-facing bounded name.
    pub name: String,
    /// Exact named producer.
    pub producer: String,
    /// Latest status.
    pub status: JobStatus,
    /// Whether terminal observation is required before turn completion.
    pub requires_report: bool,
    /// Whether terminal facts have been reported.
    pub reported: bool,
    /// Terminal facts when settled.
    pub terminal: Option<JobTerminal>,
    /// Whether producer output/control remains retained.
    pub output_retained: bool,
}

/// Result of one output operation, including an atomic status snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobRead {
    /// Job facts at the read boundary.
    pub job: JobSummary,
    /// Standard-output bytes and offsets.
    pub stdout: JobOutputRead,
    /// Standard-error bytes and offsets.
    pub stderr: JobOutputRead,
}

/// Scope-finalization report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFinalization {
    /// Jobs that required reporting and had not been observed before finalization.
    pub unreported: Vec<JobSummary>,
}

/// One admitted producer request.
#[derive(Clone, Debug)]
pub struct JobSubmission {
    /// Human-facing bounded name.
    pub name: String,
    /// Exact registered producer name.
    pub producer: String,
    /// Producer-specific trusted request.
    pub request: JobRequest,
    /// Whether completion must be reported before a turn may complete.
    pub requires_report: bool,
}

/// Producer-owned control transferred to Jobs before identifier publication.
#[async_trait]
pub trait JobControl: fmt::Debug + Send + Sync + 'static {
    /// Reads raw retained producer output at one whole-stream offset.
    fn read(&self, stream: JobStream, offset: u64) -> Result<JobOutputRead>;
    /// Requests idempotent cancellation.
    fn cancel(&self);
    /// Waits for terminal facts. Multiple callers must observe the same value.
    async fn wait(&self) -> Result<JobTerminal>;
}

/// Trusted named work producer.
pub trait JobProducer: fmt::Debug + Send + Sync + 'static {
    /// Validates and starts one request, returning ownership control on success.
    fn start(&self, request: &JobRequest) -> Result<Arc<dyn JobControl>>;
}

/// One exact-name producer registration.
#[derive(Clone)]
pub struct JobProducerRegistration {
    /// Exact bounded producer name.
    pub name: String,
    /// Producer implementation.
    pub producer: Arc<dyn JobProducer>,
}

impl fmt::Debug for JobProducerRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobProducerRegistration")
            .field("name", &self.name)
            .field("producer", &"<job producer>")
            .finish()
    }
}

type ProducerSettlement =
    Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync + 'static>;

/// Opaque lease for one exact producer generation.
///
/// Dropping withdraws and cancels without waiting. [`Self::retire`] also waits
/// for work admitted through the exact generation to settle.
pub struct JobProducerLease {
    withdraw: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
    settle: Option<ProducerSettlement>,
}

impl JobProducerLease {
    /// Creates a producer lease from provider-owned lifecycle actions.
    pub fn new<F, S, Fut>(withdraw: F, settle: S) -> Self
    where
        F: FnOnce() + Send + Sync + 'static,
        S: FnOnce() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self {
            withdraw: Some(Box::new(withdraw)),
            settle: Some(Box::new(move || Box::pin(settle()))),
        }
    }

    /// Withdraws, cancels, and waits for exact-generation settlement.
    pub async fn retire(mut self) -> Result<()> {
        if let Some(withdraw) = self.withdraw.take() {
            withdraw();
        }
        if let Some(settle) = self.settle.take() {
            settle().await
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for JobProducerLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JobProducerLease(..)")
    }
}

impl Drop for JobProducerLease {
    fn drop(&mut self) {
        if let Some(withdraw) = self.withdraw.take() {
            withdraw();
        }
    }
}

/// Process-local background-job control plane.
#[async_trait]
pub trait Jobs: fmt::Debug + Send + Sync + 'static {
    /// Registers one exact-name producer generation.
    fn register_producer(&self, registration: JobProducerRegistration) -> Result<JobProducerLease>;
    /// Acquires the live authority generation for one identity.
    fn acquire_scope(&self, id: JobScopeId) -> Result<JobScopeAuthority>;
    /// Admits one scoped request and publishes its identifier after ownership transfer.
    fn submit(&self, scope: &JobScopeAuthority, submission: JobSubmission) -> Result<String>;
    /// Lists at most [`MAXIMUM_JOBS_PER_LIST`] exact-scope records oldest-first.
    fn list(&self, scope: &JobScopeAuthority) -> Result<Vec<JobSummary>>;
    /// Gets one exact-scope record.
    fn get(&self, scope: &JobScopeAuthority, id: &str) -> Result<JobSummary>;
    /// Atomically reads both streams. A terminal read also reports the job.
    fn read(
        &self,
        scope: &JobScopeAuthority,
        id: &str,
        stdout_offset: u64,
        stderr_offset: u64,
    ) -> Result<JobRead>;
    /// Waits, atomically reads both streams, and reports the terminal job.
    async fn wait(
        &self,
        scope: &JobScopeAuthority,
        id: &str,
        stdout_offset: u64,
        stderr_offset: u64,
    ) -> Result<JobRead>;
    /// Cancels, waits, reads both complete retained tails, and reports one job.
    async fn kill(&self, scope: &JobScopeAuthority, id: &str) -> Result<JobRead>;
    /// Revokes, cancels, reaps, and reports remaining required work in one scope.
    async fn finalize_scope(&self, scope: &JobScopeAuthority) -> Result<JobFinalization>;
    /// Withdraws admission, cancels all work, and waits under the provider bound.
    async fn cancel_all(&self) -> Result<()>;
}

/// Nominal Local contract for [`Jobs`].
#[derive(Debug)]
pub struct JobsContract;

impl LocalContract for JobsContract {
    const KEY: &'static str = "rsi.jobs";
    type Service = dyn Jobs;
}

/// Closed Jobs failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum JobsError {
    /// Malformed or out-of-bounds value.
    #[error("invalid job value: {0}")]
    InvalidInput(String),
    /// Active or retained record capacity is exhausted.
    #[error("job capacity is exhausted")]
    Capacity,
    /// Duplicate exact producer registration.
    #[error("job producer `{0}` is already registered")]
    DuplicateProducer(String),
    /// No active exact-name producer.
    #[error("job producer `{0}` is not registered")]
    UnknownProducer(String),
    /// Exact scope authority is revoked, foreign, or stale.
    #[error("job scope authority is closed")]
    ScopeClosed,
    /// No job with this identity is visible to the exact scope.
    #[error("job `{0}` is not available in this scope")]
    UnknownJob(String),
    /// Provider has stopped accepting work.
    #[error("job provider is shutting down")]
    ShuttingDown,
    /// Cancellation did not settle the selected work within the provider bound.
    #[error("job cancellation timed out")]
    CancellationTimeout,
    /// Producer or control failed.
    #[error("job execution failed: {0}")]
    Execution(String),
}

/// Jobs result.
pub type Result<T> = std::result::Result<T, JobsError>;

/// Validates one bounded producer/job identifier.
pub fn validate_job_identifier(kind: &str, value: &str) -> Result<()> {
    validate_identifier(kind, value)
}

fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_JOB_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(JobsError::InvalidInput(format!(
            "{kind} must be bounded nonempty ASCII"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{JobStatus, JobTerminal};

    #[test]
    fn completed_terminal_rejects_process_failure_evidence() {
        for (exit_code, signal) in [(Some(7), None), (None, Some(9)), (Some(0), Some(9))] {
            let terminal = JobTerminal {
                status: JobStatus::Completed,
                exit_code,
                signal,
                message: None,
            };
            assert!(terminal.validate().is_err(), "accepted {terminal:?}");
        }
        for exit_code in [None, Some(0)] {
            JobTerminal {
                status: JobStatus::Completed,
                exit_code,
                signal: None,
                message: None,
            }
            .validate()
            .unwrap();
        }
    }
}
