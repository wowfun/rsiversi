//! Pure fork-domain preparation under one immutable historical cut.

use super::{ContributionFactReader, ContributionResult};
use crate::ValidatedDomainProposal;
use async_trait::async_trait;
use rsi_agent_session_protocol::{DomainSnapshot, ForkTurnSelection, SessionHeader};
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Captured historical authority supplied by the fork owner, never by plugin input.
#[derive(Clone, Debug)]
pub struct ForkTransformContext {
    /// Source identity and frozen interpretation.
    pub source: Arc<SessionHeader>,
    /// Requested completed-turn selection.
    pub selection: ForkTurnSelection,
    /// Exclusive lower Fact boundary; earlier facts may be read only to form a baseline.
    pub after_seq: u64,
    /// Inclusive completed-turn Fact boundary.
    pub through_seq: u64,
    /// Exact control boundary associated with that completed cut.
    pub through_control_seq: u64,
    /// Complete source domain state at the captured control boundary.
    pub domains: Arc<[DomainSnapshot]>,
    /// Read-only source history bounded by the completed cut.
    pub facts: Arc<dyn ContributionFactReader>,
}

/// Domain-only transformation; returned proposals belong to the target generation.
#[async_trait]
pub trait ForkTransform: fmt::Debug + Send + Sync + 'static {
    /// Produces unpublished initial state without executing business callbacks.
    async fn transform(
        &self,
        context: &ForkTransformContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>>;
}
