//! Effect-free command callbacks frozen in one Agent composition generation.

use crate::{ContributionResult, Publication, ValidatedDomainProposal};
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, CommandArguments, CommandMessage,
    CommandMessageSource, CommandRevision, DomainStateView, MAXIMUM_COMMAND_INPUT_BYTES,
    MAXIMUM_COMMAND_INPUTS, MessageAttribution, MessageOptions, SessionCommandDescriptor,
    SessionCommandInvocation, SessionHeader,
};
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;
mod draft;
mod outcome;
pub use draft::{
    AppliedDraftCommand, DraftCommandError, DraftCommandMutation, DraftCommandPreparation,
    DraftCommandResult, MAXIMUM_DRAFT_COMMAND_RECEIPTS, PreparedDraftCommand,
};
pub use outcome::{CommandOutcome, CommandOutcomeEvidence, CommandOutcomeOwner};

/// Immutable input captured before a command callback; no mutation authority.
#[derive(Clone, Debug)]
pub struct SessionCommandContext {
    /// Retained owner evidence; absent only for initial continuation reservations.
    pub outcome: Option<CommandOutcome>,
    /// Captured durable history; absent for unpublished drafts.
    pub history: Option<crate::CapturedFactHistory>,
    /// Exact immutable invocation identity, including internal reserve receipts.
    pub request_id: rsi_agent_session_protocol::DomainRequestId,
    /// Authoritative candidate or durable Session Header.
    pub header: Arc<SessionHeader>,
    /// Exact captured draft or durable control predecessor.
    pub revision: CommandRevision,
    /// Complete bounded domain set from that same predecessor.
    pub domains: Arc<[DomainStateView]>,
    /// Authenticated reserve input; absent on ordinary, draft and settle commands.
    pub continuation_input: Option<rsi_agent_session_protocol::ContinuationInput>,
}

/// Complete staged command output; its owner supplies the atomic publication evidence.
#[derive(Debug, Default)]
pub struct SessionCommandOutput {
    /// Typed complete replacements from this exact generation.
    pub domains: Vec<ValidatedDomainProposal>,
    /// Bounded messages published atomically with the replacements.
    pub inputs: Vec<CommandInput>,
    /// Resource and receipt guards retained through publication and notification.
    /// Initial automatic continuation reservations do not admit observers.
    pub publications: Vec<Publication>,
}
impl From<Vec<ValidatedDomainProposal>> for SessionCommandOutput {
    fn from(domains: Vec<ValidatedDomainProposal>) -> Self {
        Self {
            domains,
            inputs: vec![],
            publications: vec![],
        }
    }
}

/// Scheduling intent of content returned by an admitted command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandInputDelivery {
    /// Hold for the next Step without waking an idle Session.
    Inject,
    /// Join the current Turn when active, otherwise wake a new Turn.
    Steer,
}
/// Proposed content; only the Session owner assigns its durable identity and source.
#[derive(Clone, Debug)]
pub struct CommandInput {
    /// Delivery intent, independent of the current scheduling state.
    pub delivery: CommandInputDelivery,
    /// Ordered model-visible blocks; external callers verify referenced objects.
    pub content: Vec<AgentMessageContent>,
    /// Optional original presentation identity and source.
    pub attribution: Option<MessageAttribution>,
}
/// Validated immutable input retained through draft or durable publication.
#[derive(Clone, Debug)]
pub struct PreparedCommandInput {
    delivery: CommandInputDelivery,
    held: CommandMessage,
    encoded_len: usize,
}
impl PreparedCommandInput {
    /// Validates and assigns one complete batch before any owner state changes.
    ///
    /// # Errors
    /// Rejects invalid content, invocation identities or aggregate capacity.
    pub fn prepare(
        invocation: &SessionCommandInvocation,
        inputs: Vec<CommandInput>,
    ) -> ContributionResult<Vec<Self>> {
        if inputs.len() > MAXIMUM_COMMAND_INPUTS {
            return Err(crate::ContributionError::Capacity);
        }
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let digest = invocation
            .digest()
            .map_err(|error| crate::ContributionError::Invalid(error.to_string()))?;
        let batch = inputs
            .into_iter()
            .enumerate()
            .map(|(ordinal, input)| {
                let source = CommandMessageSource {
                    command: invocation.command.clone(),
                    request_id: invocation.request_id.clone(),
                    invocation_sha256: digest.clone(),
                    ordinal: u32::try_from(ordinal)
                        .map_err(|_| crate::ContributionError::Capacity)?,
                    attribution: input.attribution,
                };
                let message = AgentMessage {
                    message_id: source
                        .message_id()
                        .map_err(|error| crate::ContributionError::Invalid(error.to_string()))?,
                    source: AgentMessageSource::Command { source },
                    content: input.content,
                    options: MessageOptions::default(),
                };
                message
                    .validate()
                    .map_err(|error| crate::ContributionError::Invalid(error.to_string()))?;
                let held = CommandMessage {
                    message,
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                };
                let encoded_len = held
                    .encoded_len()
                    .map_err(|error| crate::ContributionError::Invalid(error.to_string()))?;
                Ok(Self {
                    delivery: input.delivery,
                    held,
                    encoded_len,
                })
            })
            .collect::<ContributionResult<Vec<_>>>()?;
        Self::validate_batch(&batch)?;
        Ok(batch)
    }
    /// Checks the whole unpublished draft or command batch without changing it.
    ///
    /// # Errors
    /// Rejects message-count or complete encoded byte capacity violations.
    pub fn validate_batch(inputs: &[Self]) -> ContributionResult<()> {
        if inputs.len() > MAXIMUM_COMMAND_INPUTS {
            return Err(crate::ContributionError::Capacity);
        }
        let punctuation = 2 + inputs.len().saturating_sub(1);
        if punctuation + inputs.iter().map(|input| input.encoded_len).sum::<usize>()
            > MAXIMUM_COMMAND_INPUT_BYTES
        {
            return Err(crate::ContributionError::Capacity);
        }
        Ok(())
    }
    /// Returns the exact validated message with owner-assigned provenance.
    pub const fn message(&self) -> &AgentMessage {
        &self.held.message
    }
    /// Returns immutable scheduling intent.
    pub const fn delivery(&self) -> CommandInputDelivery {
        self.delivery
    }
    /// Scheduling intent as recorded in the owning publication.
    pub const fn publication_delivery(&self, draft: bool) -> rsi_agent_session_protocol::MessageDelivery {
        if !draft && matches!(self.delivery, CommandInputDelivery::Steer) {
            rsi_agent_session_protocol::MessageDelivery::Steer
        } else {
            rsi_agent_session_protocol::MessageDelivery::NextStep
        }
    }
    /// Freezes durable intent; drafts hold all inputs for the first accepted Step.
    pub fn publication(&self, draft: bool) -> rsi_agent_session_protocol::CommandMessage {
        self.clone().into_publication(draft)
    }

    /// Moves the callback's original content into its final publication.
    pub fn into_publication(mut self, draft: bool) -> CommandMessage {
        self.held.delivery = self.publication_delivery(draft);
        self.held
    }
}

pub(crate) fn initial_commit(
    baseline: &crate::DomainBaseline,
    inputs: &[PreparedCommandInput],
) -> ContributionResult<Option<rsi_agent_session_protocol::DomainStateCommit>> {
    match baseline.commit() {
        Some(commit) => commit
            .clone()
            .with_command_inputs(inputs.iter().map(|input| input.publication(true)).collect())
            .map(Some)
            .map_err(|error| crate::ContributionError::Invalid(error.to_string())),
        None if inputs.is_empty() => Ok(None),
        None => rsi_agent_session_protocol::DomainStateCommit::new_with_command_inputs(
            None,
            rsi_agent_session_protocol::DomainMutationSource::Baseline,
            vec![],
            inputs.iter().map(|input| input.publication(true)).collect(),
        )
        .map(Some)
        .map_err(|error| crate::ContributionError::Invalid(error.to_string())),
    }
}

/// A command stages typed replacements; the Session owner performs the commit.
#[async_trait]
pub trait SessionCommand: fmt::Debug + Send + Sync + 'static {
    /// Validates its arguments and proposes one atomic state change outside framework locks.
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionCommandOutput>;
}

/// Bounded metadata paired with its exact linked callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContinuationCommand {
    /// Requires an armed lease and exactly one complete reserved input.
    Reserve,
    /// May reconcile with a retained revoked lease; cannot authorize input.
    Settle,
}

/// Bounded metadata paired with its exact linked callback.
#[derive(Clone, Debug)]
pub struct SessionCommandRegistration {
    descriptor: SessionCommandDescriptor,
    callback: Arc<dyn SessionCommand>,
    continuation: Option<ContinuationCommand>,
}

impl SessionCommandRegistration {
    /// Pairs validated metadata and a callback for unpublished generation admission.
    pub fn new(descriptor: SessionCommandDescriptor, callback: Arc<dyn SessionCommand>) -> Self {
        Self {
            descriptor,
            callback,
            continuation: None,
        }
    }
    /// Restricts dispatch to the Kernel's authenticated continuation service.
    /// This process-local flag is frozen with the callback, never supplied on the wire.
    #[must_use]
    pub fn continuation_only(mut self, kind: ContinuationCommand) -> Self {
        self.continuation = Some(kind);
        self
    }
    /// Whether ordinary command discovery and dispatch must exclude this callback.
    pub const fn is_continuation_only(&self) -> bool {
        self.continuation.is_some()
    }
    /// Returns the internal reserve or settlement contract selected by registration.
    pub const fn continuation_kind(&self) -> Option<ContinuationCommand> {
        self.continuation
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
