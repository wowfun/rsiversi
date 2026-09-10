//! Local operator inspection of actual product runtime ownership.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod api;
mod projection;
pub use api::{InspectorApi, InspectorClient};
use rsi_api_protocol::{ApiError, Result};
use rsi_meta::{FactoryIdentity, FiberId, InspectionRequest, RuntimeInspection};
use rsi_meta_profile::{ProfileSnapshot, ProfileStatus};
use serde::{Deserialize, Serialize};

/// Maximum rows in a Profile or frozen factory page.
pub const MAXIMUM_PAGE_ROWS: usize = 128;
/// Encoded finite response admission shared with the API registry.
pub const MAXIMUM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Exclusive Runtime-local Fiber cursor and Meta-owned collection bounds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeRequest {
    /// Canonical decimal Fiber identity, absent for the first page.
    pub after: Option<String>,
    /// Maximum returned Fibers.
    pub maximum_fibers: usize,
    /// Maximum items in each per-Fiber collection.
    pub maximum_items: usize,
}
impl Default for RuntimeRequest {
    fn default() -> Self {
        let request = InspectionRequest::default();
        Self {
            after: None,
            maximum_fibers: request.maximum_fibers,
            maximum_items: request.maximum_items,
        }
    }
}
impl RuntimeRequest {
    /// Validates before accessing the actual Runtime.
    pub fn validate(&self) -> Result<InspectionRequest> {
        let after = self
            .after
            .as_deref()
            .map(|value| {
                let id: u64 = value.parse().map_err(|_| invalid())?;
                if value != id.to_string() {
                    return Err(invalid());
                }
                Ok(FiberId(id))
            })
            .transpose()?;
        if !(1..=rsi_meta::MAXIMUM_INSPECTION_FIBERS).contains(&self.maximum_fibers)
            || !(1..=rsi_meta::MAXIMUM_INSPECTION_ITEMS).contains(&self.maximum_items)
        {
            return Err(invalid());
        }
        Ok(InspectionRequest {
            after,
            maximum_fibers: self.maximum_fibers,
            maximum_items: self.maximum_items,
        })
    }
}

/// Bounded offset page over one observed Profile or frozen factory declaration set.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct PageRequest {
    /// Number of preceding rows to skip.
    pub offset: usize,
    /// Maximum returned rows.
    pub limit: usize,
}
impl Default for PageRequest {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 64,
        }
    }
}
impl PageRequest {
    /// Rejects unbounded pages and offsets that cannot be represented exactly in JSON.
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAXIMUM_PAGE_ROWS).contains(&self.limit) || self.offset > u32::MAX as usize {
            return Err(invalid());
        }
        Ok(())
    }
}
fn invalid() -> ApiError {
    ApiError::Invalid("invalid Inspector request".into())
}

/// One frozen executable declaration, excluding configuration/schema/source paths.
#[derive(Clone, Debug, Serialize)]
pub struct FactoryDeclaration {
    /// Owning product addon.
    pub addon: String,
    /// Composition role.
    pub scope: String,
    /// Exact resolver-owned provenance.
    pub identity: FactoryIdentity,
    /// Declared update policy.
    pub update_mode: String,
}

/// Desired or staged native source identity; it grants no execution authority.
#[derive(Clone, Debug, Serialize)]
pub struct NativeArtifact {
    /// Local manifest identity.
    pub id: String,
    /// ABI plugin identity.
    pub plugin: String,
    /// Exact artifact target.
    pub target: String,
    /// Content digest checked before mapping.
    pub sha256: String,
    /// Explicit generation-private Portable keys.
    pub portable_services: Vec<String>,
}
/// Path-free native staging and actual Loader observations.
#[derive(Clone, Debug, Serialize)]
pub struct NativeObservation {
    /// Categorical manager health, without native diagnostics.
    pub health: NativeHealth,
    /// Validated desired source revision.
    pub source_revision: Option<String>,
    /// Last successfully staged revision.
    pub staged_revision: Option<String>,
    /// Desired records from the validated bounded store.
    pub desired: Vec<NativeArtifact>,
    /// Most recently staged records; old Session pins can retain others.
    pub staged: Vec<NativeArtifact>,
    /// Failure-retained finalizations.
    pub retained_failed_finalizations: usize,
    /// Actual staging bytes, including retained failures, as a decimal string.
    pub staging_bytes: String,
    /// Still admitted callback bodies.
    pub active_callbacks: usize,
    /// Actual live instance count.
    pub active_instances: usize,
    /// Retained ABI host capabilities.
    pub host_capabilities: usize,
    /// Retained ABI output tokens.
    pub host_outputs: usize,
}
/// Manager selection state, separate from Profile or Agent generation convergence.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHealth {
    /// New selection is admitted.
    Ready,
    /// Desired selection awaits staging.
    Pending,
    /// The latest staging attempt failed.
    Failed,
    /// Desired metadata cannot be validated.
    InvalidSource,
    /// Failed native cleanup permanently closed admission.
    Retained,
    /// The manager closed new selection.
    Closed,
}

/// Explicit product-owned observation source. Queries perform no activation or loading.
pub trait InspectorSource: std::fmt::Debug + Send + Sync + 'static {
    /// Captures actual Runtime ownership through the owning Meta inspection seam.
    fn runtime(&self, request: InspectionRequest) -> Result<RuntimeInspection>;
    /// Captures the product Profile's redacted status and tree separately.
    fn profile(&self) -> Result<(ProfileStatus, ProfileSnapshot)>;
    /// Returns immutable product declarations in their frozen order.
    fn factories(&self) -> &[FactoryDeclaration];
    /// Captures actual native state or reports that the manager is unavailable.
    fn native(&self) -> Result<NativeObservation>;
}
