//! Explicit resumable mechanical Session retirement, independent of live authority.
use super::{
    CasObjectRef, Deserialize, MaintenanceOperationId, Result, Serialize, SessionId, StoreError,
    async_trait, fmt,
};

/// One exact optimistic Session selection, never a recursive tree request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeTarget {
    /// Exact selected identity.
    pub session_id: SessionId,
    /// Immutable Header correlation.
    pub header_sha256: String,
    /// Expected canonical Fact tail.
    pub fact_seq: u64,
    /// Expected canonical control tail.
    pub control_seq: u64,
}
/// One idempotent bounded retirement request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeRequest {
    /// Caller-preallocated identity, retained across uncertain responses.
    pub operation_id: MaintenanceOperationId,
    /// Strictly increasing exact IDs; at most 256 Sessions.
    pub targets: Vec<PurgeTarget>,
}
impl PurgeRequest {
    /// Rejects invalid identities, duplicate/unsorted selections and scalar bounds.
    pub fn validate(&self) -> Result<()> {
        if self.targets.is_empty()
            || self.targets.len() > 256
            || self
                .targets
                .windows(2)
                .any(|p| p[0].session_id >= p[1].session_id)
        {
            return Err(StoreError::Invalid(
                "purge requires 1..=256 sorted distinct Session IDs".into(),
            ));
        }
        for target in &self.targets {
            if target.header_sha256.len() != 64
                || !target
                    .header_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || target.fact_seq > i64::MAX as u64
                || target.control_seq > i64::MAX as u64
            {
                return Err(StoreError::Invalid("invalid purge expected version".into()));
            }
        }
        Ok(())
    }
}
/// Exact refusal reason; public adapters must redact unauthorized source identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PurgeBlocker {
    /// A selected Session is missing or has changed since selection.
    Changed {
        /// Selected identity.
        session_id: SessionId,
    },
    /// Durable work remains active or queued.
    Busy {
        /// Selected identity.
        session_id: SessionId,
    },
    /// Native work lost its settlement reporter; retry is not cleanup evidence.
    UncertainWork {
        /// Selected identity requiring owner cleanup and Service restart.
        session_id: SessionId,
    },
    /// Product-owned state cannot be removed by ordinary Session maintenance.
    ProductOwned {
        /// Selected identity.
        session_id: SessionId,
    },
    /// An unselected canonical source retains this target.
    Referenced {
        /// Referencing owner, subject to adapter authorization.
        source: Option<SessionId>,
        /// Selected target.
        target: SessionId,
    },
}
/// Maximum refusal evidence across one bounded maintenance selection.
pub const MAXIMUM_PURGE_BLOCKERS: usize = 1024;
/// Read-only admission result; execution must recheck inside its writer transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PurgePreview {
    /// Bounded refusal evidence.
    pub blockers: Vec<PurgeBlocker>,
}
/// Durable progress, independent of any waiter or process generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PurgeReceipt {
    /// Original operation identity.
    pub operation_id: MaintenanceOperationId,
    /// True only after every selected Header is gone and tombstones are final.
    pub complete: bool,
    /// Canonical/derived logical rows actually deleted.
    pub deleted_records: u64,
    /// Persisted TEXT/BLOB bytes deleted, including reference rows; excludes numeric storage, indexes and WAL.
    pub deleted_payload_bytes: u64,
    /// `SQLite` free pages at the receipt snapshot; no physical shrink claim.
    pub free_pages: u64,
    /// Store-wide registered Agent-CAS objects at this snapshot; purge deletes none.
    pub retained_cas_objects: u64,
}
/// Optional backend maintenance seam; the product owns caller and live-owner admission.
#[async_trait]
pub trait StoreMaintenance: fmt::Debug + Send + Sync {
    /// Lists 1..=256 unfinished purge IDs, strictly after the optional ID cursor.
    /// The page is an observation, never authorization or automatic recovery.
    async fn pending_purges(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>>;
    /// Lists 1..=256 unfinished CAS collection IDs under the same cursor contract.
    async fn pending_cas_collections(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>>;
    /// Lists at most 256 unreferenced registrations; collection rechecks ownership.
    async fn preview_cas_gc(&self, limit: usize) -> Result<Vec<CasGcTarget>>;
    /// Collects one exact generation with idempotent durable namespace progress.
    async fn collect_cas(&self, operation_id: &str, target: CasGcTarget) -> Result<CasGcReceipt>;
    /// Reads an exact collection receipt without performing file operations.
    async fn cas_gc_status(&self, operation_id: &str) -> Result<Option<CasGcReceipt>>;

    /// Validates reference coverage and reports current blockers without deleting data.
    async fn preview_purge(&self, request: &PurgeRequest) -> Result<PurgePreview>;
    /// Atomically rechecks admission and records durable retirement; blockers leave it untouched.
    async fn begin_purge(
        &self,
        request: PurgeRequest,
    ) -> Result<std::result::Result<PurgeReceipt, PurgePreview>>;
    /// Deletes one bounded transaction; the caller yields before requesting another batch.
    async fn purge_batch(&self, operation_id: &str) -> Result<PurgeReceipt>;
    /// Original selection and execution locations for authorization after Header removal.
    async fn purge_binding(
        &self,
        operation_id: &str,
    ) -> Result<Option<(PurgeRequest, Vec<rsi_execution_protocol::ExecutionLocation>)>>;
    /// Recovers an exact operation after unknown completion or restart.
    async fn purge_status(&self, operation_id: &str) -> Result<Option<PurgeReceipt>>;
}

/// Exact registered CAS publication selected by an explicit collection preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CasGcTarget {
    /// Content identity and authenticated length.
    pub object: CasObjectRef,
    /// Monotonic publication generation for this hash.
    pub generation: u64,
}
impl CasGcTarget {
    /// Validates content identity, length and durable publication generation.
    pub fn validate(&self) -> Result<()> {
        self.object.validate()?;
        if self.generation == 0 || self.generation > i64::MAX as u64 {
            return Err(StoreError::Invalid("invalid CAS generation".into()));
        }
        Ok(())
    }
}
/// Durable one-object namespace deletion receipt, separate from Session purge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CasGcReceipt {
    /// Caller allocated operation identity.
    pub operation_id: MaintenanceOperationId,
    /// Exact publication selected originally.
    pub target: CasGcTarget,
    /// True only after unlink, namespace sync and metadata commit.
    pub complete: bool,
}

/// Explicit collection of exactly one previewed publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CasGcRequest {
    /// Caller allocated identity retained across retries.
    pub operation_id: MaintenanceOperationId,
    /// Exact publication selected by preview.
    pub target: CasGcTarget,
}
impl CasGcRequest {
    /// Validates durable identity and publication scalar bounds.
    pub fn validate(&self) -> Result<()> {
        self.target.validate()
    }
}
impl CasGcReceipt {
    /// Checks the publication evidence; the operation identity is validated on decode.
    pub fn validate(&self) -> Result<()> {
        self.target.validate()
    }
}
