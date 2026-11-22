//! Effect-free command callbacks frozen in one Agent composition generation.

use crate::{ContributionError, ContributionResult, Publication, ValidatedDomainProposal};
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, CommandArguments, CommandMessage,
    CommandMessageSource, CommandRevision, DomainStateView, MAXIMUM_COMMAND_INPUT_BYTES,
    MAXIMUM_COMMAND_INPUTS, MessageAttribution, MessageDelivery, MessageOptions,
    SessionCommandDescriptor, SessionCommandInvocation, SessionHeader,
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
    /// On an unpublished Session, publish the last steering input as its first turn.
    pub start_draft: bool,
    /// Resource and receipt guards retained through publication and notification.
    /// Initial automatic continuation reservations do not admit observers.
    pub publications: Vec<Publication>,
}
impl From<Vec<ValidatedDomainProposal>> for SessionCommandOutput {
    fn from(domains: Vec<ValidatedDomainProposal>) -> Self {
        Self {
            domains,
            inputs: vec![],
            start_draft: false,
            publications: vec![],
        }
    }
}

/// Proposed content; only the Session owner assigns its durable identity and source.
#[derive(Clone, Debug)]
pub struct CommandInput {
    /// Delivery intent, independent of the current scheduling state.
    pub delivery: MessageDelivery,
    /// Ordered model-visible blocks; external callers verify referenced objects.
    pub content: Vec<AgentMessageContent>,
    /// Optional original presentation identity and source.
    pub attribution: Option<MessageAttribution>,
}
impl CommandInput {
    /// Verify referenced files and images with the composing Scope before publication.
    pub async fn verify_attachments(
        &self,
        context: &rsi_meta::Context,
        cancellation: &CancellationToken,
    ) -> ContributionResult<()> {
        let mut files = std::collections::HashSet::new();
        for block in &self.content {
            if cancellation.is_cancelled() {
                return Err(ContributionError::Closed);
            }
            match block {
                AgentMessageContent::File { file } if files.insert(file) => context
                    .lookup_local::<rsi_media_protocol::FileReadContract>()
                    .ok_or_else(|| {
                        ContributionError::Invalid("File attachment reader is unavailable".into())
                    })?
                    .verify(file)
                    .await
                    .map_err(|error| ContributionError::Invalid(error.to_string()))?,
                AgentMessageContent::Image { media, .. } => {
                    context
                        .lookup_local::<rsi_media_protocol::MediaContract>()
                        .ok_or_else(|| {
                            ContributionError::Invalid("Image reader is unavailable".into())
                        })?
                        .read(media)
                        .await
                        .map_err(|error| ContributionError::Invalid(error.to_string()))?;
                }
                AgentMessageContent::File { .. }
                | AgentMessageContent::Text { .. }
                | AgentMessageContent::Reference { .. } => {}
            }
        }
        Ok(())
    }
}

/// Validated immutable input retained through draft or durable publication.
#[derive(Clone, Debug)]
pub struct PreparedCommandInput {
    output: CommandMessage,
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
                let mut output = CommandMessage {
                    message,
                    delivery: MessageDelivery::NextStep,
                };
                // A draft always writes next_step, including when the durable intent is shorter.
                let encoded_len = output
                    .encoded_len()
                    .map_err(|error| crate::ContributionError::Invalid(error.to_string()))?;
                output.delivery = input.delivery;
                Ok(Self {
                    output,
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
        &self.output.message
    }
    /// Returns immutable scheduling intent.
    pub const fn delivery(&self) -> MessageDelivery {
        self.output.delivery
    }
    /// Freezes durable intent; drafts hold all inputs for the first accepted Step.
    pub fn publication(&self, draft: bool) -> rsi_agent_session_protocol::CommandMessage {
        self.clone().into_publication(draft)
    }

    /// Moves the callback's original content into its final publication.
    pub fn into_publication(mut self, draft: bool) -> CommandMessage {
        if draft {
            self.output.delivery = MessageDelivery::NextStep;
        }
        self.output
    }
}

pub(crate) fn initial_commit(
    baseline: &crate::DomainBaseline,
    inputs: &[PreparedCommandInput],
    selection: &rsi_agent_session_protocol::GenerationSelection,
    start_draft: bool,
) -> ContributionResult<rsi_agent_session_protocol::DomainStateCommit> {
    let mut messages: Vec<_> = inputs.iter().map(|input| input.publication(true)).collect();
    let request_id = if start_draft {
        let last = messages.last_mut().expect("prepared first command task");
        last.delivery = MessageDelivery::NextTurn;
        let AgentMessageSource::Command { source } = &last.message.source else {
            unreachable!()
        };
        Some(source.request_id.clone())
    } else {
        None
    };
    rsi_agent_session_protocol::DomainStateCommit::new_with_command_inputs(
        request_id,
        rsi_agent_session_protocol::DomainMutationSource::Generation {
            selection: selection.clone(),
            turn_id: None,
        },
        baseline
            .commit()
            .map_or_else(Vec::new, |commit| commit.updates().to_vec()),
        messages,
    )
    .map_err(|error| crate::ContributionError::Invalid(error.to_string()))
}

/// A command stages typed replacements; the Session owner performs the commit.
#[async_trait]
pub trait SessionCommand: fmt::Debug + Send + Sync + 'static {
    /// Whether a later revision still holds the exact unsettled input from this
    /// recoverable reserve command. Called only after verifying its original receipt.
    fn retains_pending_input(
        &self,
        _snapshot: &rsi_agent_session_protocol::DomainSnapshot,
        _input: &rsi_agent_session_protocol::ContinuationInput,
    ) -> bool {
        false
    }

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
    /// Also permits cold claim of the exact pending input of a committed reserve.
    RecoverableReserve,
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
