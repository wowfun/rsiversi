//! Process-local background Jobs contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Maximum job identifier or name bytes.
pub const MAXIMUM_JOB_IDENTIFIER_BYTES: usize = 256;
/// Maximum encoded terminal JSON result bytes.
pub const MAXIMUM_JOB_RESULT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum retained UTF-8 bytes in one job failure.
pub const MAXIMUM_JOB_FAILURE_BYTES: usize = 4 * 1024;
/// Maximum components in one generic owner scope.
pub const MAXIMUM_JOB_SCOPE_COMPONENTS: usize = 8;

/// Generic bounded owner identity used for isolated cancellation.
///
/// Components remain separate so higher layers can preserve exact identities
/// without delimiter escaping or a dependency from Jobs back to those layers.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct JobScope {
    namespace: String,
    components: Vec<String>,
}

impl JobScope {
    /// Creates one scope from an identifier namespace and one or more exact components.
    pub fn new<I, S>(namespace: impl Into<String>, components: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let namespace = namespace.into();
        validate_scope_component("job scope namespace", &namespace)?;
        let components = components.into_iter().map(Into::into).collect::<Vec<_>>();
        if components.is_empty() || components.len() > MAXIMUM_JOB_SCOPE_COMPONENTS {
            return Err(JobsError::InvalidInput(format!(
                "job scope must contain 1..={MAXIMUM_JOB_SCOPE_COMPONENTS} components"
            )));
        }
        for component in &components {
            validate_scope_component("job scope component", component)?;
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

/// Latest live status of one process-local job.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Registered but body not yet entered.
    Queued,
    /// Body is running.
    Running,
    /// Body completed successfully.
    Completed,
    /// Body returned failure.
    Failed,
    /// Cooperative cancellation settled.
    Cancelled,
}

/// Terminal process-local job outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum JobOutcome {
    /// Successful bounded JSON value.
    Completed(Value),
    /// Body failure message.
    Failed(JobFailure),
    /// Cooperative cancellation settled.
    Cancelled,
}

impl JobOutcome {
    /// Creates a failed outcome while bounding retained diagnostic text.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(JobFailure::bounded(message.into()))
    }
}

/// Bounded diagnostic retained by a failed job outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFailure(String);

impl JobFailure {
    fn bounded(mut message: String) -> Self {
        if message.len() > MAXIMUM_JOB_FAILURE_BYTES {
            let mut boundary = MAXIMUM_JOB_FAILURE_BYTES;
            while !message.is_char_boundary(boundary) {
                boundary -= 1;
            }
            message.truncate(boundary);
        }
        Self(message)
    }

    /// Returns the bounded diagnostic text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Trusted process-local job body.
#[async_trait]
pub trait JobTask: fmt::Debug + Send + Sync + 'static {
    /// Runs until completion or cooperative cancellation.
    async fn run(&self, cancellation: CancellationToken) -> Result<Value>;
}

/// One job submission.
#[derive(Clone)]
pub struct JobSpec {
    /// Human/debug name.
    pub name: String,
    /// Trusted body.
    pub task: Arc<dyn JobTask>,
}

impl fmt::Debug for JobSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobSpec")
            .field("name", &self.name)
            .field("task", &"<job task>")
            .finish()
    }
}

/// Opaque join/cancel control implemented by a Jobs provider.
#[async_trait]
pub trait JobControl: fmt::Debug + Send + Sync + 'static {
    /// Returns the latest live status.
    fn status(&self) -> JobStatus;
    /// Requests cooperative cancellation.
    fn cancel(&self);
    /// Waits for the terminal outcome. Multiple callers observe the same value.
    async fn join(&self) -> JobOutcome;
}

/// Cloneable job identity and control.
#[derive(Clone, Debug)]
pub struct JobHandle {
    id: String,
    control: Arc<dyn JobControl>,
}

impl JobHandle {
    /// Creates a handle from provider-owned control.
    pub fn new(id: String, control: Arc<dyn JobControl>) -> Self {
        Self { id, control }
    }

    /// Returns the process-local identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns latest live status.
    pub fn status(&self) -> JobStatus {
        self.control.status()
    }

    /// Requests cooperative cancellation.
    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// Waits for the terminal outcome.
    pub async fn join(&self) -> JobOutcome {
        self.control.join().await
    }
}

/// Process-local job scheduler.
#[async_trait]
pub trait Jobs: fmt::Debug + Send + Sync + 'static {
    /// Submits one bounded job.
    fn submit(&self, spec: JobSpec) -> Result<JobHandle>;
    /// Submits one bounded job owned by an exact cancellation scope.
    fn submit_scoped(&self, scope: JobScope, spec: JobSpec) -> Result<JobHandle>;
    /// Closes one exact scope for the cancellation snapshot and waits through the provider bound.
    async fn cancel_scope(&self, scope: &JobScope) -> Result<()>;
    /// Temporarily closes admission, cancels every unfinished job, and waits through the provider
    /// bound. A timed-out snapshot keeps admission closed until its tracked work settles.
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
    /// Malformed or out-of-bounds input/output.
    #[error("invalid job value: {0}")]
    InvalidInput(String),
    /// Active job capacity is exhausted.
    #[error("job capacity is exhausted")]
    Capacity,
    /// Scheduler has stopped accepting work.
    #[error("job scheduler is shutting down")]
    ShuttingDown,
    /// Cooperative cancellation did not settle every job within the provider bound.
    #[error("job cancellation timed out")]
    CancellationTimeout,
    /// Job body failed.
    #[error("job failed: {0}")]
    Execution(String),
}

/// Jobs result.
pub type Result<T> = std::result::Result<T, JobsError>;

/// Validates one bounded job name.
pub fn validate_job_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAXIMUM_JOB_IDENTIFIER_BYTES {
        return Err(JobsError::InvalidInput(format!(
            "job name must be within 1..={MAXIMUM_JOB_IDENTIFIER_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_scope_component(kind: &str, value: &str) -> Result<()> {
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
