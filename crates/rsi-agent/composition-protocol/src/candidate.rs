//! Candidate preparation and private-state validation.

use crate::Result;
use rsi_agent_session_protocol::GenerationSelection;
use std::fmt;

/// Validates and rebuilds private state before a replacement can be published.
#[async_trait::async_trait]
pub trait GenerationValidator: fmt::Debug + Send + Sync + 'static {
    /// Loads the exact proposed state without business effects or durable writes.
    /// A proposed revision may have been used by an abandoned candidate; it is
    /// not sufficient evidence to reuse private state during this callback.
    /// Kernel bounds pre-publication preparation to 30 seconds. Observe the
    /// cancellation token and retire unpublished work when this future is dropped.
    async fn validate(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
        domains: &[rsi_agent_session_protocol::DomainStateView],
        cancellation: tokio_util::sync::CancellationToken,
    ) -> crate::ContributionResult<()>;
}

/// One frozen executable candidate; consuming it builds from current Session state.
#[async_trait::async_trait]
pub trait AgentGenerationCandidate: fmt::Debug + Send + 'static {
    /// Diagnostic selection identity that a successful build will retain.
    fn selection(&self) -> &GenerationSelection;

    /// Constructs and validates the candidate without publishing a Session selection.
    /// Dropping this future abandons preparation and must retire unpublished work.
    ///
    /// # Errors
    /// Rejects unavailable code, invalid current state, activation or sealing failures.
    async fn build(
        self: Box<Self>,
        seed: &crate::AgentGenerationSeed,
    ) -> Result<crate::AgentCompositionPin>;
}
