//! Effect-free command callbacks frozen in one Agent composition generation.

use crate::{ContributionResult, ValidatedDomainProposal};
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, DomainStateView, SessionCommandDescriptor, SessionHeader,
};
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;
mod draft;
pub use draft::{
    DraftCommandError, DraftCommandMutation, DraftCommandPreparation, DraftCommandResult,
    MAXIMUM_DRAFT_COMMAND_RECEIPTS, PreparedDraftCommand,
};

/// Immutable input captured before a command callback; no mutation authority.
#[derive(Clone, Debug)]
pub struct SessionCommandContext {
    /// Authoritative candidate or durable Session Header.
    pub header: Arc<SessionHeader>,
    /// Exact captured draft or durable control predecessor.
    pub revision: CommandRevision,
    /// Complete bounded domain set from that same predecessor.
    pub domains: Arc<[DomainStateView]>,
}

/// A command proposes typed replacements only; the Session owner performs the commit.
#[async_trait]
pub trait SessionCommand: fmt::Debug + Send + Sync + 'static {
    /// Validates its arguments and proposes one atomic state change outside framework locks.
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>>;
}

/// Bounded metadata paired with its exact linked callback.
#[derive(Clone, Debug)]
pub struct SessionCommandRegistration {
    descriptor: SessionCommandDescriptor,
    callback: Arc<dyn SessionCommand>,
}

impl SessionCommandRegistration {
    /// Pairs validated metadata and a callback for unpublished generation admission.
    pub fn new(descriptor: SessionCommandDescriptor, callback: Arc<dyn SessionCommand>) -> Self {
        Self {
            descriptor,
            callback,
        }
    }
    /// Returns immutable discovery metadata.
    pub const fn descriptor(&self) -> &SessionCommandDescriptor {
        &self.descriptor
    }
    /// Returns the exact pinned callback; consumers do not resolve another generation.
    pub fn callback(&self) -> &Arc<dyn SessionCommand> {
        &self.callback
    }
}
