//! Draft-local command preparation and atomic application; the lease owner schedules calls.

use super::{SessionCommandContext, SessionCommandOutput, SessionCommandRegistration};
use crate::{
    AgentCompositionPin, AgentSessionDraft, ContributionError, ContributionKind, DomainBaseline,
    DomainError, DraftPublicationReceipt, Publication,
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
    outcome: super::CommandOutcomeOwner,
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
        let mut output = self
            .command
            .callback()
            .execute(&self.context, &self.invocation.arguments, cancellation)
            .await?;
        if (output.domains.is_empty() && output.inputs.is_empty())
            || output.domains.len() > rsi_agent_session_protocol::MAXIMUM_SESSION_DOMAINS
            || output.publications.len() > crate::MAXIMUM_AGENT_CONTRIBUTIONS
        {
            // No state was swapped. Notification is outside the draft owner's admission.
            for publication in &mut output.publications {
                if publication.rejected().await.is_err() {
                    break;
                }
            }
            return Err(DraftCommandError::Invalid(
                "command requires inputs or at most 64 typed initial replacements".into(),
            ));
        }
        Ok(DraftCommandMutation {
            outcome: self.outcome,
            identity: self.identity,
            _composition: self.composition,
            invocation: self.invocation,
            output,
        })
    }
}

/// Complete callback output, carrying no authority for a different draft or predecessor.
#[derive(Debug)]
pub struct DraftCommandMutation {
    outcome: super::CommandOutcomeOwner,
    identity: Arc<()>,
    _composition: AgentCompositionPin,
    invocation: SessionCommandInvocation,
    output: SessionCommandOutput,
}

/// Completed local admission with deferred notification outside the draft mutation lock.
#[derive(Debug)]
pub struct AppliedDraftCommand {
    result: DraftCommandResult<SessionCommandReceipt>,
    session: rsi_agent_session_protocol::SessionId,
    baseline: DomainBaseline,
    publications: Vec<Publication>,
    inputs: Vec<super::PreparedCommandInput>,
}
impl AppliedDraftCommand {
    /// Delivers exact success evidence or definite rejection, then returns the owner result.
    /// Failed/cancelled notification never rolls back an already visible draft swap.
    ///
    /// # Errors
    /// Propagates local admission or notification failure. The caller owns its deadline.
    pub async fn finish(mut self) -> DraftCommandResult<SessionCommandReceipt> {
        let notification = async {
            for publication in &mut self.publications {
                match &self.result {
                    Ok(receipt) => {
                        publication
                            .draft_committed(&DraftPublicationReceipt {
                                session_id: &self.session,
                                receipt,
                                baseline: &self.baseline,
                                inputs: &self.inputs,
                            })
                            .await?;
                    }
                    Err(_) => publication.rejected().await?,
                }
            }
            Ok::<_, ContributionError>(())
        }
        .await;
        let receipt = self.result?;
        notification?;
        Ok(receipt)
    }
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
                ContributionKind::Command(command) if !command.is_continuation_only() => {
                    Some(command.descriptor().clone())
                }
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
                ContributionKind::Command(command)
                    if entry.id() == &invocation.command && !command.is_continuation_only() =>
                {
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
        let outcome = super::CommandOutcomeOwner::default();
        let outcome_view = outcome.view();
        Ok(DraftCommandPreparation::Run(Box::new(
            PreparedDraftCommand {
                outcome,
                identity: self.identity.clone(),
                composition: self.composition.clone(),
                context: SessionCommandContext {
                    outcome: Some(outcome_view),
                    history: None,
                    request_id: invocation.request_id.clone(),
                    continuation_input: None,
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
    /// The returned completion carries refusal of another draft, stale revision,
    /// changed request, invalid proposals or capacity, and must finish outside admission.
    pub fn apply_command(&mut self, mut mutation: DraftCommandMutation) -> AppliedDraftCommand {
        let result = self.apply_command_state(&mut mutation);
        match &result {
            Ok(receipt) => mutation.outcome.draft_committed(&DraftPublicationReceipt {
                session_id: self.header.session_id(),
                receipt,
                baseline: &self.baseline,
                inputs: &self.command_inputs,
            }),
            Err(_) => mutation.outcome.rejected(),
        }
        AppliedDraftCommand {
            result,
            session: self.header.session_id().clone(),
            baseline: self.baseline.clone(),
            inputs: self.command_inputs.clone(),
            publications: std::mem::take(&mut mutation.output.publications),
        }
    }

    fn apply_command_state(
        &mut self,
        mutation: &mut DraftCommandMutation,
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
        baseline.apply_batch(&mutation.output.domains)?;
        let mut inputs = self.command_inputs.clone();
        inputs.extend(super::PreparedCommandInput::prepare(
            &mutation.invocation,
            std::mem::take(&mut mutation.output.inputs),
        )?);
        super::PreparedCommandInput::validate_batch(&inputs)?;
        let state_sha256 = super::initial_commit(&baseline, &inputs)?.map_or_else(
            || baseline.digest().to_owned(),
            |commit| commit.request_sha256().to_owned(),
        );
        let receipt = SessionCommandReceipt::draft_changed(&mutation.invocation, state_sha256)?;
        let CommandRevision::Draft { revision } = receipt.revision() else {
            unreachable!("draft receipt")
        };
        self.baseline = baseline;
        self.command_inputs = inputs;
        self.revision = revision;
        self.command_receipts
            .insert(mutation.invocation.request_id.clone(), receipt.clone());
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
