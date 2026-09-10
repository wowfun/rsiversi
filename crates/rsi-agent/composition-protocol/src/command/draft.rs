//! Draft-local command preparation and atomic application; the lease owner schedules calls.

use super::{SessionCommandContext, SessionCommandRegistration};
use crate::{
    AgentCompositionPin, AgentSessionDraft, ContributionError, ContributionKind, DomainError,
    ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    CommandRevision, DomainRequestId, DomainRevision, DomainStateView, SessionCommandDescriptor,
    SessionCommandInvocation, SessionCommandReceipt,
};
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Maximum compact idempotency receipts retained during one unpublished draft lease.
pub const MAXIMUM_DRAFT_COMMAND_RECEIPTS: usize = 256;

/// Failure before a complete draft mutation becomes visible.
#[derive(Debug, Error)]
pub enum DraftCommandError {
    /// The caller observed a different draft or durable revision.
    #[error("draft command revision conflict: expected {expected:?}, actual {actual:?}")]
    Revision {
        /// Frozen caller predecessor.
        expected: CommandRevision,
        /// Current lease-local predecessor.
        actual: CommandRevision,
    },
    /// One existing idempotency identity names different logical input.
    #[error("draft command request {request_id} conflicts with its original invocation")]
    RequestConflict {
        /// Exact conflicting identity.
        request_id: DomainRequestId,
    },
    /// The proposal batch was prepared by a different draft instance.
    #[error("command proposal belongs to another draft")]
    WrongDraft,
    /// The lease has reached its retained receipt bound.
    #[error("draft command receipt capacity exhausted")]
    Capacity,
    /// Unknown command, non-draft-safe callback, invalid output or exhausted counter.
    #[error("invalid draft command: {0}")]
    Invalid(String),
    /// The registered linked callback rejected the invocation.
    #[error(transparent)]
    Contribution(#[from] ContributionError),
    /// A typed proposal failed exact-generation or complete-state validation.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// Mechanical invocation or receipt validation failed.
    #[error(transparent)]
    Protocol(#[from] rsi_agent_session_protocol::SessionError),
}

/// Result of draft-local command admission or application.
pub type DraftCommandResult<T> = Result<T, DraftCommandError>;

/// A retry returns its original result; a new operation carries one consuming callback.
#[derive(Debug)]
pub enum DraftCommandPreparation {
    /// Already applied by this exact draft lease.
    Completed(SessionCommandReceipt),
    /// Prepared immutable inputs, ready to run outside mutation admission.
    Run(Box<PreparedDraftCommand>),
}

/// One move-only callback input bound to a draft instance and generation.
#[derive(Debug)]
pub struct PreparedDraftCommand {
    identity: Arc<()>,
    composition: AgentCompositionPin,
    context: SessionCommandContext,
    invocation: SessionCommandInvocation,
    command: SessionCommandRegistration,
}

impl PreparedDraftCommand {
    /// Runs once; the lease owner supplies deadline and cancellation outside mutation admission.
    ///
    /// # Errors
    /// Propagates callback failure or rejects an empty or oversized proposal batch.
    pub async fn execute(
        self,
        cancellation: CancellationToken,
    ) -> DraftCommandResult<DraftCommandMutation> {
        let proposals = self
            .command
            .callback()
            .execute(&self.context, &self.invocation.arguments, cancellation)
            .await?;
        if proposals.is_empty()
            || proposals.len() > rsi_agent_session_protocol::MAXIMUM_SESSION_DOMAINS
        {
            return Err(DraftCommandError::Invalid(
                "command requires 1..=64 typed initial replacements".into(),
            ));
        }
        Ok(DraftCommandMutation {
            identity: self.identity,
            _composition: self.composition,
            invocation: self.invocation,
            proposals,
        })
    }
}

/// Complete callback output, carrying no authority for a different draft or predecessor.
#[derive(Debug)]
pub struct DraftCommandMutation {
    identity: Arc<()>,
    _composition: AgentCompositionPin,
    invocation: SessionCommandInvocation,
    proposals: Vec<ValidatedDomainProposal>,
}

impl AgentSessionDraft {
    /// Returns the lease-local mutation revision, independent of domain revision zero.
    pub const fn revision(&self) -> CommandRevision {
        CommandRevision::Draft {
            revision: self.revision,
        }
    }

    /// Lists metadata from the pinned generation, including non-draft-safe commands.
    pub fn command_descriptors(&self) -> Vec<SessionCommandDescriptor> {
        self.composition
            .contributions()
            .entries()
            .iter()
            .filter_map(|entry| match entry.kind() {
                ContributionKind::Command(command) => Some(command.descriptor().clone()),
                _ => None,
            })
            .collect()
    }

    /// Queries a previously applied command without invoking callbacks.
    pub fn command_receipt(&self, request_id: &DomainRequestId) -> Option<SessionCommandReceipt> {
        self.command_receipts.get(request_id).cloned()
    }

    /// Captures inputs under lease-owner mutation admission; no callback runs here.
    ///
    /// # Errors
    /// Rejects changed requests, stale revisions, unavailable commands and capacity.
    pub fn prepare_command(
        &self,
        invocation: SessionCommandInvocation,
    ) -> DraftCommandResult<DraftCommandPreparation> {
        if let Some(receipt) = self.matching_command_receipt(&invocation)? {
            return Ok(DraftCommandPreparation::Completed(receipt));
        }
        self.check_command_revision(&invocation)?;
        if self.command_receipts.len() >= MAXIMUM_DRAFT_COMMAND_RECEIPTS {
            return Err(DraftCommandError::Capacity);
        }
        let command = self
            .composition
            .contributions()
            .entries()
            .iter()
            .find_map(|entry| match entry.kind() {
                ContributionKind::Command(command) if entry.id() == &invocation.command => {
                    Some(command.clone())
                }
                _ => None,
            })
            .ok_or_else(|| {
                DraftCommandError::Invalid("command is unavailable in this draft generation".into())
            })?;
        if !command.descriptor().draft_safe() {
            return Err(DraftCommandError::Invalid(
                "command does not admit draft initial state".into(),
            ));
        }
        let domains = self
            .baseline
            .initial_states()
            .into_iter()
            .map(|snapshot| DomainStateView {
                revision: DomainRevision::new(0),
                snapshot,
            })
            .collect();
        Ok(DraftCommandPreparation::Run(Box::new(
            PreparedDraftCommand {
                identity: self.identity.clone(),
                composition: self.composition.clone(),
                context: SessionCommandContext {
                    header: Arc::new(self.header.clone()),
                    revision: self.revision(),
                    domains,
                },
                invocation,
                command,
            },
        )))
    }

    /// Applies all replacements under the same admission as first submit and preset selection.
    ///
    /// # Errors
    /// Rejects another draft, stale revision, changed request, invalid proposals or capacity.
    pub fn apply_command(
        &mut self,
        mutation: DraftCommandMutation,
    ) -> DraftCommandResult<SessionCommandReceipt> {
        if !Arc::ptr_eq(&self.identity, &mutation.identity) {
            return Err(DraftCommandError::WrongDraft);
        }
        if let Some(receipt) = self.matching_command_receipt(&mutation.invocation)? {
            return Ok(receipt);
        }
        self.check_command_revision(&mutation.invocation)?;
        if self.command_receipts.len() >= MAXIMUM_DRAFT_COMMAND_RECEIPTS {
            return Err(DraftCommandError::Capacity);
        }
        let mut baseline = self.baseline.clone();
        baseline.apply_batch(&mutation.proposals)?;
        let receipt =
            SessionCommandReceipt::draft_changed(&mutation.invocation, baseline.digest())?;
        let CommandRevision::Draft { revision } = receipt.revision() else {
            unreachable!("draft receipt")
        };
        self.baseline = baseline;
        self.revision = revision;
        self.command_receipts
            .insert(mutation.invocation.request_id, receipt.clone());
        Ok(receipt)
    }

    fn check_command_revision(
        &self,
        invocation: &SessionCommandInvocation,
    ) -> DraftCommandResult<()> {
        if invocation.expected_revision != self.revision() {
            return Err(DraftCommandError::Revision {
                expected: invocation.expected_revision,
                actual: self.revision(),
            });
        }
        Ok(())
    }

    fn matching_command_receipt(
        &self,
        invocation: &SessionCommandInvocation,
    ) -> DraftCommandResult<Option<SessionCommandReceipt>> {
        let receipt = self.command_receipts.get(&invocation.request_id);
        if let Some(receipt) = receipt
            && receipt.invocation_sha256() != invocation.digest()?
        {
            return Err(DraftCommandError::RequestConflict {
                request_id: invocation.request_id.clone(),
            });
        }
        Ok(receipt.cloned())
    }
}
