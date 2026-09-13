//! Claim-bound process-local Jobs status; no scope or output authority escapes.

use crate::{Result, TurnError};
use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionId, TurnId};
use rsi_jobs::JobSummary;
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_util::sync::CancellationToken;

/// Maximum finite Jobs page rows.
pub const MAXIMUM_TURN_JOBS_ITEMS: usize = 32;
/// Maximum canonical encoded Jobs page bytes.
pub const MAXIMUM_TURN_JOBS_BYTES: usize = 64 * 1024;

/// Exclusive cursor request bound to an exact active Turn.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnJobsRequest {
    /// Current active Turn, never a historical selector.
    pub turn_id: TurnId,
    /// Claim generation from the previous page; required with a cursor.
    pub generation: Option<u64>,
    /// Exclusive lexicographic job identity.
    pub after: Option<String>,
    /// Upper row bound within 1..=32.
    pub limit: usize,
}
impl TurnJobsRequest {
    /// Checks finite request and cursor consistency.
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAXIMUM_TURN_JOBS_ITEMS).contains(&self.limit)
            || self.generation == Some(0)
            || (self.after.is_some() && self.generation.is_none())
        {
            return Err(TurnError::Invalid("invalid Jobs page request".into()));
        }
        if let Some(after) = &self.after {
            rsi_jobs::validate_job_identifier("job cursor", after).map_err(invalid)?;
        }
        Ok(())
    }
}

/// Complete finite status page at one process-local sample.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnJobsPage {
    /// Exact Session owner.
    pub session_id: SessionId,
    /// Immutable Header fingerprint.
    pub header_sha256: String,
    /// Exact active Turn.
    pub turn_id: TurnId,
    /// Claim generation; invalid after executor replacement.
    pub generation: u64,
    /// Exact requested exclusive cursor.
    pub after: Option<String>,
    /// Ordered status-only summaries.
    pub jobs: Vec<JobSummary>,
    /// Additional rows existed at this sample.
    pub has_more: bool,
}
impl TurnJobsPage {
    /// Checks encoded, row, identity and summary bounds.
    pub fn validate(&self) -> Result<()> {
        if self.generation == 0
            || self.header_sha256.len() != 64
            || !self
                .header_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.jobs.len() > MAXIMUM_TURN_JOBS_ITEMS
            || (self.has_more && self.jobs.is_empty())
        {
            return Err(TurnError::Invalid("invalid Jobs page".into()));
        }
        let mut previous = self.after.as_deref();
        if let Some(after) = previous {
            rsi_jobs::validate_job_identifier("job cursor", after).map_err(invalid)?;
        }
        for job in &self.jobs {
            job.validate().map_err(invalid)?;
            if previous.is_some_and(|previous| previous >= job.id.as_str()) {
                return Err(TurnError::Invalid("unordered Jobs page".into()));
            }
            previous = Some(&job.id);
        }
        if self.encoded_len()? > MAXIMUM_TURN_JOBS_BYTES {
            return Err(TurnError::Invalid("Jobs page exceeds encoded bound".into()));
        }
        Ok(())
    }
    /// Returns exact canonical bytes for retention admission.
    pub fn encoded_len(&self) -> Result<usize> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .map_err(invalid)
    }
    /// Checks a sampled page against the request and immutable handle binding.
    pub fn validate_for(
        &self,
        session: &SessionId,
        header: &str,
        request: &TurnJobsRequest,
    ) -> Result<()> {
        self.validate()?;
        if &self.session_id != session
            || self.header_sha256 != header
            || self.turn_id != request.turn_id
            || self.after != request.after
            || self.jobs.len() > request.limit
            || request
                .generation
                .is_some_and(|generation| generation != self.generation)
        {
            return Err(TurnError::Invalid("Jobs page binding differs".into()));
        }
        Ok(())
    }
}

/// Executor-owned source retaining only its original exact authority clone.
/// Implementations are synchronous, bounded to 256 summaries, and never report.
pub trait TurnJobStatusSource: fmt::Debug + Send + Sync + 'static {
    /// Whether the exact original Jobs authority remains live.
    fn is_active(&self) -> bool;
    /// Samples status only; no read, wait, kill or scope acquisition is permitted.
    fn list(&self) -> Result<Vec<JobSummary>>;
}

/// Kernel-owned public read port over current authenticated claim sources.
#[async_trait]
pub trait TurnJobs: fmt::Debug + Send + Sync + 'static {
    /// Samples one finite page, checking cancellation and live claim on both sides.
    async fn read_jobs(
        &self,
        session: &SessionId,
        header_sha256: &str,
        request: TurnJobsRequest,
        cancellation: CancellationToken,
    ) -> Result<TurnJobsPage>;
}

/// Read-only Host contract; never exposes a Jobs authority.
#[derive(Debug)]
pub struct TurnJobsContract;
impl rsi_meta_contract::LocalContract for TurnJobsContract {
    const KEY: &'static str = "rsi.agent.turn.jobs";
    type Service = dyn TurnJobs;
}
fn invalid(error: impl fmt::Display) -> TurnError {
    TurnError::Invalid(error.to_string())
}
