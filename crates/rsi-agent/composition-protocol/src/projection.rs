//! Framework-driven disposable views over complete typed domain state.

use crate::{AgentCompositionPin, ContributionError, ContributionKind, ContributionResult};
use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_agent_session_protocol::{
    DomainStateView, MAXIMUM_DOMAIN_BASELINE_BYTES, MAXIMUM_SESSION_DOMAINS, ProjectionCursor,
    ProjectionEntry, ProjectionValue, SessionHeader, SessionProjectionSnapshot,
};
use rsi_meta::{Deadline, Execution};
use std::{collections::BTreeSet, fmt, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Validated complete capture, with no mutation or subscription authority.
#[derive(Clone, Debug)]
pub struct SessionProjectionContext {
    header: Arc<SessionHeader>,
    cursor: ProjectionCursor,
    domains: Arc<[DomainStateView]>,
}
impl SessionProjectionContext {
    /// Validates aggregate count, size, identity and draft/durable domain predecessors.
    ///
    /// # Errors
    /// Rejects inconsistent or oversized aggregate inputs before entering any callback.
    pub fn new(
        header: Arc<SessionHeader>,
        cursor: ProjectionCursor,
        domains: Arc<[DomainStateView]>,
    ) -> ContributionResult<Self> {
        if domains.len() > MAXIMUM_SESSION_DOMAINS {
            return Err(ContributionError::Capacity);
        }
        let mut ids = BTreeSet::new();
        let mut bytes = 0_usize;
        for state in domains.iter() {
            let is_draft = matches!(cursor, ProjectionCursor::Draft { .. });
            if !ids.insert(state.snapshot.identity().id())
                || (state.revision.get() == 0) != is_draft
            {
                return Err(ContributionError::Invalid(
                    "projection domain identity or predecessor is inconsistent".into(),
                ));
            }
            bytes = bytes
                .checked_add(state.snapshot.encoded_len().map_err(invalid)?)
                .ok_or(ContributionError::Capacity)?;
            if bytes > MAXIMUM_DOMAIN_BASELINE_BYTES {
                return Err(ContributionError::Capacity);
            }
        }
        Ok(Self {
            header,
            cursor,
            domains,
        })
    }
    /// Borrows the exact captured Header.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }
    /// Returns the cut reflected by every captured domain value.
    pub const fn cursor(&self) -> ProjectionCursor {
        self.cursor
    }
    /// Borrows the complete bounded state set; only owning codecs perform semantic decode.
    pub fn domains(&self) -> &[DomainStateView] {
        &self.domains
    }
}

/// Pure read-side computation owned by an ordinary Agent contribution.
#[async_trait]
pub trait SessionProjection: fmt::Debug + Send + Sync + 'static {
    /// Computes a whole bounded view from captured input, without I/O or effects.
    async fn project(
        &self,
        context: &SessionProjectionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ProjectionValue>;
}

/// Central drive retaining the same immutable generation as the selected Session.
#[derive(Clone, Debug)]
pub struct SessionProjectionAdapter {
    pin: AgentCompositionPin,
}
impl SessionProjectionAdapter {
    /// Retains an exact projection catalog, codecs and generation owner.
    pub const fn new(pin: AgentCompositionPin) -> Self {
        Self { pin }
    }
    /// Computes isolated producer outcomes at one cut, using the caller's execution clock.
    ///
    /// # Errors
    /// Rejects a mismatched Header or cancellation; individual producer errors remain entries.
    pub async fn snapshot(
        &self,
        context: &SessionProjectionContext,
        execution: &Execution,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionProjectionSnapshot> {
        if context.header.agent_preset_id() != self.pin.preset_id() {
            return Err(ContributionError::Invalid(
                "projection Header does not match its generation".into(),
            ));
        }
        let deadline = execution.deadline_after(Duration::from_secs(30));
        let mut entries = Vec::new();
        for contribution in self.pin.contributions().entries() {
            let ContributionKind::Projection(callback) = contribution.kind() else {
                continue;
            };
            let value = cancellation
                .run_until_cancelled(project_one(
                    callback.as_ref(),
                    context,
                    execution,
                    &deadline,
                    cancellation.child_token(),
                ))
                .await
                .ok_or(ContributionError::Closed)?;
            entries.push(match value {
                Ok(value) => ProjectionEntry::value(contribution.id().clone(), value),
                Err(error) => {
                    let message = error.to_string();
                    let mut end = message.len().min(4096);
                    while !message.is_char_boundary(end) {
                        end -= 1;
                    }
                    ProjectionEntry::failed(contribution.id().clone(), &message[..end])
                        .or_else(|_| {
                            ProjectionEntry::failed(
                                contribution.id().clone(),
                                "projection returned an invalid diagnostic",
                            )
                        })
                        .map_err(invalid)?
                }
            });
        }
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        SessionProjectionSnapshot::new(
            context.header.session_id().clone(),
            context.header.fingerprint().map_err(invalid)?,
            self.pin.source_digest(),
            context.cursor,
            entries,
        )
        .map_err(invalid)
    }
}

async fn project_one(
    callback: &dyn SessionProjection,
    context: &SessionProjectionContext,
    execution: &Execution,
    deadline: &Deadline,
    cancellation: CancellationToken,
) -> ContributionResult<ProjectionValue> {
    let _guard = cancellation.clone().drop_guard();
    let unit_deadline = execution.deadline_after(Duration::from_secs(1));
    let call = std::panic::AssertUnwindSafe(callback.project(context, cancellation)).catch_unwind();
    deadline
        .timeout(unit_deadline.timeout(call))
        .await
        .map_err(|_| ContributionError::Invalid("projection capture deadline elapsed".into()))?
        .map_err(|_| ContributionError::Invalid("projection callback deadline elapsed".into()))?
        .unwrap_or_else(|_| {
            Err(ContributionError::Invalid(
                "projection callback panicked".into(),
            ))
        })
}

fn invalid(error: impl fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}
