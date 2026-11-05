//! Generation-pinned effect-free commands with canonical receipt reconciliation.

use super::continuation::CommandAuthorization;
use super::*;
use rsi_agent_composition_protocol::{
    CommandOutcomeOwner, ContributionKind, SessionCommandContext, SessionCommandRegistration,
    ValidatedDomainProposal,
};
use rsi_agent_composition_protocol::{Publication, PublicationReceipt};
use rsi_agent_session_protocol::SessionCommandsView;
use rsi_agent_session_protocol::{
    CommandRevision, DomainMutationSource, DomainRequestId, DomainStateCommit, DomainStateUpdate,
    DomainStateView, SessionCommandInvocation,
};
use rsi_agent_store_protocol::MailboxRead;
use rsi_agent_turn_protocol::{DomainMutationReceipt, SessionCommands};

type CommandKey = (SessionId, DomainRequestId);
type CommandResult = TurnResult<DomainMutationReceipt>;

#[derive(Default)]
pub(super) struct CommandRequests(Mutex<BTreeMap<CommandKey, PendingCommand>>);

impl CommandRequests {
    pub(super) fn contains_any(&self, ids: &[SessionId]) -> bool {
        self.0
            .lock()
            .expect("command requests poisoned")
            .keys()
            .any(|(id, _)| ids.binary_search(id).is_ok())
    }
}

struct PendingCommand {
    digest: String,
    receiver: watch::Receiver<Option<CommandResult>>,
}

struct CommandGuard {
    kernel: AgentKernel,
    key: CommandKey,
    sender: watch::Sender<Option<CommandResult>>,
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        if self.sender.borrow().is_none() {
            self.sender.send_replace(Some(Err(TurnError::Invariant(
                "command task ended without a result".into(),
            ))));
        }
        self.kernel
            .inner
            .commands
            .0
            .lock()
            .expect("command requests poisoned")
            .remove(&self.key);
    }
}

#[async_trait]
impl SessionCommands for AgentKernel {
    async fn list(&self, session: PreparedResumeSession) -> TurnResult<SessionCommandsView> {
        let (header, composition) = self.inner.resume_issuer.inspect(&session)?;
        let page = observation::read_domain_states_bounded(&self.inner, header.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        SessionCommandsView::new(
            CommandRevision::Durable {
                control_seq: page.durable_control_seq,
            },
            composition
                .contributions()
                .entries()
                .iter()
                .filter_map(|entry| match entry.kind() {
                    ContributionKind::Command(command) if !command.is_continuation_only() => {
                        Some(command.descriptor().clone())
                    }
                    _ => None,
                })
                .collect(),
        )
        .map_err(|error| TurnError::Invariant(error.to_string()))
    }

    async fn execute(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
    ) -> CommandResult {
        self.execute_command_authorized(session, invocation, CommandAuthorization::Ordinary)
            .await
    }

    async fn query(
        &self,
        session_id: &SessionId,
        request_id: &DomainRequestId,
    ) -> TurnResult<Option<DomainMutationReceipt>> {
        let result = self.domain_request(session_id, request_id).await?;
        if result.as_ref().is_some_and(|receipt| {
            !matches!(
                receipt.commit().source(),
                DomainMutationSource::Command { .. }
            )
        }) {
            return Err(TurnError::DomainRequestConflict {
                request_id: request_id.to_string(),
            });
        }
        Ok(result)
    }
}

struct CommandCandidate {
    session: PreparedResumeSession,
    invocation: SessionCommandInvocation,
    proposals: Vec<ValidatedDomainProposal>,
    inputs: Vec<rsi_agent_session_protocol::CommandMessage>,
    authorization: CommandAuthorization,
}

struct PreparedCommandCommit {
    candidate: CommandCandidate,
    _admission: SubmissionAdmissionLease,
    append: AtomicSessionAppend,
    receipt: DomainMutationReceipt,
    execution: Option<execution_admission::Reservation>,
    command_execution: Vec<Option<execution_admission::Reservation>>,
    publications: Vec<Publication>,
}

impl AgentKernel {
    pub(super) async fn execute_command_authorized(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        authorization: CommandAuthorization,
    ) -> CommandResult {
        let (header, _) = self.inner.resume_issuer.inspect(&session)?;
        authorization.validate(self, &session, &invocation)?;
        let key = (header.session_id().clone(), invocation.request_id.clone());
        let source = authorization.source(invocation.clone());
        let digest = source_digest(&source)?;
        let mut receiver = {
            // This is admission only; no user callback or Store I/O runs under either lock.
            let state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            let mut pending = self
                .inner
                .commands
                .0
                .lock()
                .expect("command requests poisoned");
            if let Some(existing) = pending.get(&key) {
                if existing.digest != digest {
                    return Err(request_conflict(&invocation));
                }
                existing.receiver.clone()
            } else {
                if pending.len() >= 64 {
                    return Err(TurnError::Capacity);
                }
                // Retirement holds this same key before inspecting Kernel state.
                // Publish the pending command while both admission locks are held.
                let submission = self
                    .inner
                    .submission_admission
                    .register(header.session_id());
                let _registration = submission
                    .mutex
                    .clone()
                    .try_lock_owned()
                    .map_err(|_| TurnError::Capacity)?;
                let (sender, receiver) = watch::channel(None);
                pending.insert(
                    key.clone(),
                    PendingCommand {
                        digest,
                        receiver: receiver.clone(),
                    },
                );
                let guard = CommandGuard {
                    kernel: self.clone(),
                    key,
                    sender,
                };
                let kernel = self.clone();
                self.inner.tasks.spawn(async move {
                    let result = std::panic::AssertUnwindSafe(kernel.execute_session_command(
                        session,
                        invocation,
                        authorization,
                    ))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        Err(TurnError::Invariant("command callback panicked".into()))
                    });
                    guard.sender.send_replace(Some(result));
                    drop(guard);
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| TurnError::Invariant("command result owner disappeared".into()))?;
        }
    }
}

impl AgentKernel {
    async fn execute_session_command(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        authorization: CommandAuthorization,
    ) -> CommandResult {
        let cancellation = self.inner.submission_admission.closed.child_token();
        let _cancel = cancellation.clone().drop_guard();
        let deadline = Instant::now() + Duration::from_secs(30);
        let outcome = CommandOutcomeOwner::default();
        let candidate = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(TurnError::ShuttingDown),
            result = tokio::time::timeout_at(deadline, self.prepare_session_command(session, invocation, authorization, cancellation.clone(), &outcome)) => {
                result.map_err(|_| TurnError::Invalid("command preparation exceeded its deadline".into()))??
            }
        };
        match candidate {
            Ok(receipt) => Ok(receipt),
            Err(candidate) => self.commit_session_command(candidate, &outcome).await,
        }
    }

    async fn prepare_session_command(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        authorization: CommandAuthorization,
        cancellation: CancellationToken,
        outcome: &CommandOutcomeOwner,
    ) -> TurnResult<std::result::Result<DomainMutationReceipt, PreparedCommandCommit>> {
        let (header, composition) = self.inner.resume_issuer.inspect(&session)?;
        if let Some(receipt) = self
            .domain_request(header.session_id(), &invocation.request_id)
            .await?
        {
            return matching_receipt(receipt, &authorization.source(invocation.clone())).map(Ok);
        }
        if let CommandAuthorization::Continuation {
            lease,
            reservation: Some(_),
        } = &authorization
        {
            self.inner.continuation_issuer.request_round(lease)?;
        }
        let callback: SessionCommandRegistration = composition
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
                TurnError::Invalid(format!(
                    "command {} is unavailable in this generation",
                    invocation.command
                ))
            })?;
        let page = observation::read_domain_states_bounded(&self.inner, header.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        if matches!(
            authorization,
            CommandAuthorization::Continuation {
                reservation: Some(_),
                ..
            }
        ) && check_revision(&invocation, page.durable_control_seq).is_err()
        {
            return Err(TurnError::SessionBusy);
        }
        check_revision(&invocation, page.durable_control_seq)?;
        composition
            .domains()
            .validate_complete_states(
                &page
                    .states
                    .iter()
                    .map(|state| state.snapshot.clone())
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| turn_composition_error(error.into()))?;
        let context = SessionCommandContext {
            outcome: Some(outcome.view()),
            history: Some(rsi_agent_composition_protocol::CapturedFactHistory {
                through_seq: page.durable_fact_seq,
                facts: captured_history::capture(
                    self,
                    header.session_id(),
                    page.durable_fact_seq,
                    cancellation.clone(),
                ),
            }),
            request_id: invocation.request_id.clone(),
            continuation_input: match &authorization {
                CommandAuthorization::Continuation { reservation, .. } => reservation.clone(),
                CommandAuthorization::Ordinary => None,
            },
            header: Arc::new(header.clone()),
            revision: invocation.expected_revision,
            domains: page
                .states
                .into_iter()
                .map(|state| DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                })
                .collect(),
        };
        let output = callback
            .callback()
            .execute(&context, &invocation.arguments, cancellation)
            .await
            .map_err(|error| {
                TurnError::Invalid(format!(
                    "command {}: {}",
                    invocation.command,
                    bounded_diagnostic(&error.to_string())
                ))
            })?;
        self.stage_command_output(session, invocation, authorization, output, outcome)
            .await
    }

    async fn stage_command_output(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        authorization: CommandAuthorization,
        mut output: rsi_agent_composition_protocol::SessionCommandOutput,
        outcome: &CommandOutcomeOwner,
    ) -> TurnResult<std::result::Result<DomainMutationReceipt, PreparedCommandCommit>> {
        if output.publications.len() > rsi_agent_composition_protocol::MAXIMUM_AGENT_CONTRIBUTIONS {
            return Err(TurnError::Invalid("command publication capacity".into()));
        }
        let inputs = match rsi_agent_composition_protocol::PreparedCommandInput::prepare(
            &invocation,
            output.inputs,
        ) {
            Ok(inputs) => inputs
                .into_iter()
                .map(|input| input.into_publication(false))
                .collect(),
            Err(error) => {
                let _ = notify_publications(&mut output.publications, None).await;
                return Err(TurnError::Invalid(error.to_string()));
            }
        };
        let result = self
            .stage_session_command(CommandCandidate {
                session,
                invocation,
                proposals: output.domains,
                inputs,
                authorization,
            })
            .await;
        match result {
            Ok(Err(mut prepared)) => {
                prepared.publications = output.publications;
                Ok(Err(prepared))
            }
            Ok(Ok(receipt)) => {
                record_outcome(outcome, &receipt);
                notify_publications(&mut output.publications, Some(&receipt)).await?;
                Ok(Ok(receipt))
            }
            Err(error) => {
                let _ = notify_publications(&mut output.publications, None).await;
                Err(error)
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Keep the frozen candidate, idle admission and atomic control suffix together."
    )]
    async fn stage_session_command(
        &self,
        mut candidate: CommandCandidate,
    ) -> TurnResult<std::result::Result<DomainMutationReceipt, PreparedCommandCommit>> {
        if (candidate.proposals.is_empty() && candidate.inputs.is_empty())
            || candidate.proposals.len() > rsi_agent_session_protocol::MAXIMUM_SESSION_DOMAINS
        {
            return Err(TurnError::Invalid(
                "command requires inputs or at most 64 typed domain replacements".into(),
            ));
        }
        candidate
            .authorization
            .validate(self, &candidate.session, &candidate.invocation)?;
        let (header, composition) = self.inner.resume_issuer.inspect(&candidate.session)?;
        let session_id = header.session_id().clone();
        let admission = self.inner.submission_admission.acquire(&session_id).await?;
        candidate
            .authorization
            .validate(self, &candidate.session, &candidate.invocation)?;
        self.fence_pending_terminal(&session_id).await?;
        if let Some(receipt) = self
            .domain_request(&session_id, &candidate.invocation.request_id)
            .await?
        {
            return matching_receipt(
                receipt,
                &candidate.authorization.source(candidate.invocation.clone()),
            )
            .map(Ok);
        }
        let page = observation::read_domain_states_bounded(&self.inner, &session_id, None)
            .await
            .map_err(turn_store_error)?;
        if let CommandAuthorization::Continuation {
            lease,
            reservation: Some(_),
        } = &candidate.authorization
            && (!self.automatic_turn_selected(lease)
                || !self.automatic_session_idle(&session_id).await?
                || check_revision(&candidate.invocation, page.durable_control_seq).is_err())
        {
            return Err(TurnError::SessionBusy);
        }
        check_revision(&candidate.invocation, page.durable_control_seq)?;
        candidate.proposals.sort_by(|a, b| {
            a.snapshot()
                .identity()
                .id()
                .cmp(b.snapshot().identity().id())
        });
        let updates = candidate
            .proposals
            .iter()
            .map(|proposal| {
                composition
                    .domains()
                    .validate_proposal(proposal)
                    .map_err(|error| turn_composition_error(error.into()))?;
                DomainStateUpdate::new(proposal.expected_revision(), proposal.snapshot().clone())
                    .map_err(|error| TurnError::Invalid(error.to_string()))
            })
            .collect::<TurnResult<Vec<_>>>()?;
        candidate.authorization.validate_updates(&updates)?;
        let commit = DomainStateCommit::new_with_command_inputs(
            Some(candidate.invocation.request_id.clone()),
            candidate.authorization.source(candidate.invocation.clone()),
            updates,
            candidate.inputs.clone(),
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let seq = page
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        rsi_agent_store_protocol::domain_heads_after(
            &page
                .states
                .iter()
                .map(|state| state.head.clone())
                .collect::<Vec<_>>(),
            seq,
            &commit,
        )
        .map_err(turn_store_error)?;
        let control = AgentControlRecord::new(
            seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let receipt = DomainMutationReceipt::new(session_id.clone(), control.clone())?;
        let mut execution = None;
        let mut controls = vec![control];
        let mut command_execution = Vec::new();
        if !candidate.inputs.is_empty() {
            let scan = scan_durable_messages(&self.inner, &session_id, MailboxRead::All).await?;
            if scan
                .pending
                .iter()
                .filter(|entry| matches!(entry.message.source, AgentMessageSource::Command { .. }))
                .count()
                .saturating_add(candidate.inputs.len())
                > rsi_agent_session_protocol::MAXIMUM_COMMAND_INPUTS
            {
                return Err(TurnError::Capacity);
            }
            let reserved = self
                .inner
                .store
                .completion_reservation_count(&session_id)
                .await
                .map_err(turn_store_error)?;
            let active = {
                let state = lock_state(&self.inner);
                state.sessions.get(&session_id).and_then(|session| {
                    session.turns.iter().find_map(|(id, turn)| {
                        (turn.activation_id.is_some()
                            && turn.terminal.is_none()
                            && !turn.cancel_requested)
                            .then(|| id.clone())
                    })
                })
            };
            let wakes = active.is_none()
                && candidate.inputs.iter().any(|input| {
                    input.delivery == rsi_agent_session_protocol::MessageDelivery::Steer
                });
            let superseded = scan
                .pending
                .iter()
                .filter(|entry| {
                    wakes
                        && matches!(
                            entry.message.source,
                            AgentMessageSource::Continuation { .. }
                        )
                })
                .map(|entry| entry.message.message_id.clone())
                .collect::<Vec<_>>();
            if scan
                .pending_count
                .saturating_sub(superseded.len())
                .saturating_add(reserved)
                .saturating_add(candidate.inputs.len())
                > MAXIMUM_PENDING_AGENT_MESSAGES
            {
                return Err(TurnError::Capacity);
            }
            for input in &candidate.inputs {
                let bound_turn_id = (input.delivery
                    == rsi_agent_session_protocol::MessageDelivery::Steer)
                    .then(|| active.clone())
                    .flatten();
                let target = if input.delivery
                    == rsi_agent_session_protocol::MessageDelivery::NextStep
                    || bound_turn_id.is_some()
                {
                    MessageTarget::NextStep
                } else {
                    MessageTarget::NextTurn
                };
                command_execution.push(self.inner.execution_messages.reserve(
                    &session_id,
                    &input.message.message_id,
                    candidate.session.execution(),
                )?);
                controls.push(
                    AgentControlRecord::new(
                        seq.checked_add(controls.len() as u64)
                            .ok_or(TurnError::Capacity)?,
                        self.inner.clock.now_ms().max(1),
                        AgentControlRecordBody::MessageAccepted {
                            message: input.message.clone(),
                            delivery: input.delivery,
                            bound_turn_id,
                            root_session_id: agent_root_and_path(header).0,
                            target,
                            wake_required: target == MessageTarget::NextTurn,
                        },
                    )
                    .map_err(|error| TurnError::Invalid(error.to_string()))?,
                );
            }
            if scan.inbox_paused
                && candidate.inputs.iter().any(|input| {
                    input.delivery != rsi_agent_session_protocol::MessageDelivery::NextStep
                })
            {
                inbox::append_resume(&mut controls, self.inner.clock.now_ms().max(1))?;
            }
            for message_id in superseded {
                controls.push(
                    AgentControlRecord::new(
                        seq.checked_add(controls.len() as u64)
                            .ok_or(TurnError::Capacity)?,
                        self.inner.clock.now_ms().max(1),
                        AgentControlRecordBody::MessageDiscarded {
                            message_id,
                            reason: MessageDiscardReason::Superseded,
                        },
                    )
                    .map_err(|error| TurnError::Invalid(error.to_string()))?,
                );
            }
        }
        if let CommandAuthorization::Continuation {
            lease,
            reservation: Some(input),
        } = &candidate.authorization
        {
            execution = self.inner.execution_messages.reserve(
                &session_id,
                &input.message_id,
                lease.execution(),
            )?;
            let source = rsi_agent_session_protocol::ContinuationSource {
                domain: lease.binding().domain.clone(),
                owner: input.owner.clone(),
                round: input.round,
                reserved_revision: receipt.commit().updates()[0].revision(),
                provenance: rsi_agent_session_protocol::ContinuationProvenance::Command {
                    request_id: candidate.invocation.request_id.clone(),
                },
                text_sha256: input.text_sha256(),
            };
            controls.push(
                AgentControlRecord::new(
                    seq.checked_add(1)
                        .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
                    self.inner.clock.now_ms().max(1),
                    AgentControlRecordBody::MessageAccepted {
                        message: AgentMessage {
                            message_id: input.message_id.clone(),
                            source: AgentMessageSource::Continuation { source },
                            content: vec![AgentMessageContent::Text {
                                text: input.text.clone(),
                            }],
                            options: MessageOptions::default(),
                        },
                        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                        bound_turn_id: None,
                        root_session_id: agent_root_and_path(header).0,
                        target: MessageTarget::NextTurn,
                        wake_required: true,
                    },
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            );
            inbox::append_resume(&mut controls, self.inner.clock.now_ms().max(1))?;
        }
        Ok(Err(PreparedCommandCommit {
            publications: vec![],
            candidate,
            _admission: admission,
            append: AtomicSessionAppend {
                session_id,
                expected_fact_seq: page.durable_fact_seq,
                expected_control_seq: page.durable_control_seq,
                header: None,
                facts: vec![],
                controls,
            },
            receipt,
            execution,
            command_execution,
        }))
    }

    async fn commit_session_command(
        &self,
        mut prepared: PreparedCommandCommit,
        outcome: &CommandOutcomeOwner,
    ) -> CommandResult {
        let mut publications = std::mem::take(&mut prepared.publications);
        let canonical_candidate = prepared.receipt.clone();
        let result = self.commit_session_command_inner(prepared, outcome).await;
        match &result {
            Ok(receipt) => notify_publications(&mut publications, Some(receipt)).await?,
            Err(TurnError::DomainOutcomeUnknown { .. }) => {}
            Err(_)
                if matches!(
                    outcome.view().evidence().as_deref(),
                    Some(rsi_agent_composition_protocol::CommandOutcomeEvidence::Durable { .. })
                ) =>
            {
                let _ = notify_publications(&mut publications, Some(&canonical_candidate)).await;
            }
            Err(_) => {
                outcome.rejected();
                let _ = notify_publications(&mut publications, None).await;
            }
        }
        result
    }

    async fn commit_session_command_inner(
        &self,
        prepared: PreparedCommandCommit,
        outcome: &CommandOutcomeOwner,
    ) -> CommandResult {
        let PreparedCommandCommit {
            candidate,
            _admission,
            append,
            receipt,
            execution,
            command_execution,
            publications: _,
        } = prepared;
        let session_id = append.session_id.clone();
        candidate
            .authorization
            .validate(self, &candidate.session, &candidate.invocation)?;
        let _execution_admission = match &candidate.authorization {
            CommandAuthorization::Continuation {
                lease,
                reservation: Some(_),
            } => execution_admission::admit(candidate.session.header(), lease.execution())?,
            _ if !candidate.inputs.is_empty() => execution_admission::admit(
                candidate.session.header(),
                candidate.session.execution(),
            )?,
            _ => None,
        };
        outcome.begin_publication();
        let result = self
            .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                sessions: vec![append],
                required_active_activations: vec![],
                quiescent_descendants_of: matches!(
                    candidate.authorization,
                    CommandAuthorization::Continuation {
                        reservation: Some(_),
                        ..
                    }
                )
                .then(|| session_id.clone().into()),
            })
            .await;
        if matches!(
            &result,
            Ok(Err(StoreError::ValidationBusy | StoreError::ReadCapacity))
        ) {
            return Err(TurnError::Capacity);
        }
        if let Err(error) = result.and_then(|result| {
            result.map_err(|error| match error {
                StoreError::SessionNotQuiescent { .. } => TurnError::SessionBusy,
                error => turn_store_error(error),
            })
        }) {
            match self
                .domain_request(&session_id, &candidate.invocation.request_id)
                .await
            {
                Ok(Some(stored)) => {
                    if stored != receipt {
                        return Err(request_conflict(&candidate.invocation));
                    }
                }
                Ok(None) => return Err(error),
                Err(_) => {
                    return Err(TurnError::DomainOutcomeUnknown {
                        request_id: candidate.invocation.request_id.to_string(),
                    });
                }
            }
        }
        record_outcome(outcome, &receipt);
        self.complete_command_continuation(&candidate, &receipt, execution)?;
        for reservation in command_execution {
            self.inner.execution_messages.publish(reservation);
        }
        if !candidate.inputs.is_empty() {
            self.request_ready_scan();
        }
        self.inner.session_changes.committed(&session_id);
        Ok(receipt)
    }
    fn complete_command_continuation(
        &self,
        candidate: &CommandCandidate,
        receipt: &DomainMutationReceipt,
        execution: Option<execution_admission::Reservation>,
    ) -> TurnResult<()> {
        if let CommandAuthorization::Continuation {
            lease,
            reservation: Some(input),
        } = &candidate.authorization
        {
            self.inner.execution_messages.publish(execution);
            let revision = receipt.commit().updates()[0].revision();
            self.inner
                .continuation_issuer
                .set_revision(lease, revision)?;
            self.inner.continuation_issuer.guard_source(
                lease,
                rsi_agent_session_protocol::ContinuationSource {
                    domain: lease.binding().domain.clone(),
                    owner: input.owner.clone(),
                    round: input.round,
                    reserved_revision: revision,
                    provenance: rsi_agent_session_protocol::ContinuationProvenance::Command {
                        request_id: candidate.invocation.request_id.clone(),
                    },
                    text_sha256: input.text_sha256(),
                },
            )?;
            self.inner
                .continuation_issuer
                .admitted_round(lease, receipt.control_seq() + 1)?;
            self.request_ready_scan();
        }
        Ok(())
    }
}

async fn notify_publications(
    publications: &mut [Publication],
    receipt: Option<&DomainMutationReceipt>,
) -> TurnResult<()> {
    if publications.len() > rsi_agent_composition_protocol::MAXIMUM_AGENT_CONTRIBUTIONS {
        return Err(TurnError::Invalid("command publication capacity".into()));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        for publication in publications {
            match receipt {
                Some(receipt) => {
                    publication
                        .committed(&PublicationReceipt {
                            session_id: receipt.session_id(),
                            facts: &[],
                            domain_commit: Some((receipt.control_seq(), receipt.commit())),
                        })
                        .await
                }
                None => publication.rejected().await,
            }
            .map_err(|error| {
                TurnError::Invalid(format!(
                    "command publication notification failed; query its receipt: {}",
                    bounded_diagnostic(&error.to_string())
                ))
            })?;
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        TurnError::Invalid("command publication notification timed out; query its receipt".into())
    })?
}

fn record_outcome(outcome: &CommandOutcomeOwner, receipt: &DomainMutationReceipt) {
    outcome.committed(&PublicationReceipt {
        session_id: receipt.session_id(),
        facts: &[],
        domain_commit: Some((receipt.control_seq(), receipt.commit())),
    });
}

fn request_conflict(invocation: &SessionCommandInvocation) -> TurnError {
    TurnError::DomainRequestConflict {
        request_id: invocation.request_id.to_string(),
    }
}

fn source_digest(source: &DomainMutationSource) -> TurnResult<String> {
    use sha2::Digest;
    let bytes =
        serde_json::to_vec(source).map_err(|error| TurnError::Invalid(error.to_string()))?;
    Ok(format!("{:x}", sha2::Sha256::digest(bytes)))
}

fn matching_receipt(
    receipt: DomainMutationReceipt,
    source: &DomainMutationSource,
) -> CommandResult {
    if source_digest(receipt.commit().source())? != source_digest(source)? {
        let request_id = receipt
            .commit()
            .request_id()
            .ok_or_else(|| TurnError::Invariant("command receipt lacks identity".into()))?;
        return Err(TurnError::DomainRequestConflict {
            request_id: request_id.to_string(),
        });
    }
    Ok(receipt)
}

fn check_revision(invocation: &SessionCommandInvocation, control_seq: u64) -> TurnResult<()> {
    let actual = CommandRevision::Durable { control_seq };
    if invocation.expected_revision != actual {
        return Err(TurnError::CommandRevisionConflict {
            expected: invocation.expected_revision,
            actual,
        });
    }
    Ok(())
}
