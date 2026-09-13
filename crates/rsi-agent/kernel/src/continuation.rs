//! Live Host continuation admission over ordinary command and mailbox transactions.

use super::*;
use rsi_agent_composition_protocol::{ContinuationCommand, ContributionKind};
use rsi_agent_session_protocol::MessageDelivery;
use rsi_agent_session_protocol::{
    ContinuationInput, ContinuationProvenance, ContinuationSource, DomainMutationSource,
    DomainRevision, DomainStateUpdate, SessionCommandInvocation,
};
use rsi_agent_turn_protocol::{
    ContinuationBinding, ContinuationLease, DomainMutationReceipt, SessionContinuations,
};

#[derive(Clone, Debug)]
pub(super) enum CommandAuthorization {
    Ordinary,
    Continuation {
        lease: ContinuationLease,
        reservation: Option<ContinuationInput>,
    },
}

impl CommandAuthorization {
    pub(super) fn source(&self, invocation: SessionCommandInvocation) -> DomainMutationSource {
        match self {
            Self::Ordinary => DomainMutationSource::Command { invocation },
            Self::Continuation { lease, reservation } => DomainMutationSource::Continuation {
                invocation,
                domain: lease.binding().domain.clone(),
                owner: lease.binding().owner.clone(),
                reservation: reservation.clone(),
            },
        }
    }

    pub(super) fn validate(
        &self,
        kernel: &AgentKernel,
        session: &PreparedResumeSession,
        invocation: &SessionCommandInvocation,
    ) -> TurnResult<()> {
        let (header, composition) = kernel.inner.resume_issuer.inspect(session)?;
        let command = composition
            .contributions()
            .entries()
            .iter()
            .find_map(|entry| match entry.kind() {
                ContributionKind::Command(command) if entry.id() == &invocation.command => {
                    Some(command)
                }
                _ => None,
            })
            .ok_or_else(|| {
                TurnError::Invalid("command is unavailable in this generation".into())
            })?;
        match self {
            Self::Ordinary if !command.is_continuation_only() => Ok(()),
            Self::Continuation { lease, reservation } => {
                kernel.validate_continuation(lease, header, composition, reservation.is_some())?;
                match (command.continuation_kind(), reservation) {
                    (Some(ContinuationCommand::Reserve), Some(input))
                        if input.owner == lease.binding().owner =>
                    {
                        input
                            .validate()
                            .map_err(|error| TurnError::Invalid(error.to_string()))
                    }
                    (Some(ContinuationCommand::Settle), None) => Ok(()),
                    _ => Err(TurnError::Invalid(
                        "internal command reservation contract differs".into(),
                    )),
                }
            }
            Self::Ordinary => Err(TurnError::Invalid(
                "internal command requires continuation dispatch".into(),
            )),
        }
    }

    pub(super) fn validate_updates(&self, updates: &[DomainStateUpdate]) -> TurnResult<()> {
        if let Self::Continuation { lease, .. } = self
            && (updates.len() != 1 || updates[0].snapshot().identity() != &lease.binding().domain)
        {
            return Err(TurnError::Invalid(
                "continuation command must replace only its bound domain".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl SessionContinuations for AgentKernel {
    async fn arm(
        &self,
        session: SubmitSession,
        binding: ContinuationBinding,
    ) -> TurnResult<ContinuationLease> {
        self.create_continuation_lease(session, binding, true).await
    }

    async fn retain_for_settlement(
        &self,
        session: SubmitSession,
        binding: ContinuationBinding,
    ) -> TurnResult<ContinuationLease> {
        self.create_continuation_lease(session, binding, false)
            .await
    }

    async fn execute(
        &self,
        lease: &ContinuationLease,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        reservation: Option<ContinuationInput>,
    ) -> TurnResult<DomainMutationReceipt> {
        let result = self
            .execute_command_authorized(
                session,
                invocation,
                CommandAuthorization::Continuation {
                    lease: lease.clone(),
                    reservation,
                },
            )
            .await;
        match result {
            Ok(receipt) => {
                let update = receipt.commit().updates().first().ok_or_else(|| {
                    TurnError::Invariant("continuation receipt has no domain update".into())
                })?;
                self.inner
                    .continuation_issuer
                    .set_revision(lease, update.revision())?;
                Ok(receipt)
            }
            Err(error) => {
                lease.revoke();
                Err(error)
            }
        }
    }

    async fn query(
        &self,
        lease: &ContinuationLease,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> TurnResult<Option<DomainMutationReceipt>> {
        self.inner.continuation_issuer.inspect(lease)?;
        let receipt = self.domain_request(lease.session_id(), request_id).await?;
        if receipt.as_ref().is_some_and(|receipt| !matches!(receipt.commit().source(), DomainMutationSource::Continuation { domain, owner, .. }
            if domain == &lease.binding().domain && owner == &lease.binding().owner)) {
            return Err(TurnError::DomainRequestConflict { request_id: request_id.to_string() });
        }
        Ok(receipt)
    }

    async fn submit(
        &self,
        lease: &ContinuationLease,
        session: SubmitSession,
        input: ContinuationInput,
        provenance: ContinuationProvenance,
    ) -> TurnResult<MessageReceipt> {
        input
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        provenance
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let admission = self
            .inner
            .submission_admission
            .acquire(session.session_id())
            .await?;
        let (header, composition) = self.continuation_session(&session)?;
        self.validate_continuation(lease, header, composition, true)?;
        if input.owner != lease.binding().owner {
            return Err(TurnError::ContinuationDisarmed);
        }
        let reserved_revision = self
            .verify_reservation(lease, &session, &input, &provenance)
            .await?;
        let source = ContinuationSource {
            domain: lease.binding().domain.clone(),
            owner: input.owner.clone(),
            round: input.round,
            reserved_revision,
            provenance,
            text_sha256: input.text_sha256(),
        };
        self.inner
            .continuation_issuer
            .guard_source(lease, source.clone())?;
        let request = SubmitMessage {
            session,
            delivery: MessageDelivery::NextTurn,
            message: AgentMessage {
                message_id: input.message_id,
                source: AgentMessageSource::Continuation { source },
                content: vec![AgentMessageContent::Text { text: input.text }],
                options: MessageOptions::default(),
            },
        };
        let result = self.submit_message_admitted(request, None, admission).await;
        if result.is_err() {
            lease.revoke();
        }
        result
    }

    async fn discard_if_pending(
        &self,
        lease: &ContinuationLease,
        message_id: &MessageId,
    ) -> TurnResult<MessageReceipt> {
        let (header, composition) = self.inner.continuation_issuer.inspect(lease)?;
        self.validate_continuation(lease, header, composition, false)?;
        let _admission = self
            .inner
            .submission_admission
            .acquire(lease.session_id())
            .await?;
        self.fence_pending_terminal(lease.session_id()).await?;
        let scan = scan_durable_messages(&self.inner, lease.session_id(), Some(message_id)).await?;
        let entry = scan
            .selected
            .as_ref()
            .ok_or_else(|| TurnError::MessageNotFound {
                session: lease.session_id().to_string(),
                message: message_id.to_string(),
            })?;
        if !matches!(&entry.message.source, AgentMessageSource::Continuation { source }
            if source.domain == lease.binding().domain && source.owner == lease.binding().owner)
        {
            return Err(TurnError::Invalid(
                "pending-only discard selected another continuation owner".into(),
            ));
        }
        self.discard_continuation_admitted(
            lease.session_id(),
            &scan,
            entry,
            MessageDiscardReason::ContinuationDisarmed,
        )
        .await
    }
}

impl AgentKernel {
    async fn create_continuation_lease(
        &self,
        session: SubmitSession,
        binding: ContinuationBinding,
        armed: bool,
    ) -> TurnResult<ContinuationLease> {
        if !lock_state(&self.inner).accepting {
            return Err(TurnError::ShuttingDown);
        }
        let _admission = self
            .inner
            .submission_admission
            .acquire(session.session_id())
            .await?;
        let (header, composition) = self.continuation_session(&session)?;
        if let Some(input) = &binding.initial_input {
            input
                .validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
            if input.owner != binding.owner || input.round != 1 || binding.revision.get() != 0 {
                return Err(TurnError::Invalid(
                    "draft continuation allocation binding differs".into(),
                ));
            }
        }
        let states = self.continuation_binding_states(&session, &binding).await?;
        let snapshot = states
            .iter()
            .find(|snapshot| snapshot.identity() == &binding.domain)
            .ok_or(TurnError::ContinuationDisarmed)?;
        if snapshot
            .sha256()
            .map_err(|error| TurnError::Invalid(error.to_string()))?
            != binding.snapshot_sha256
        {
            return Err(TurnError::ContinuationDisarmed);
        }
        composition
            .domains()
            .validate_complete_states(&states)
            .map_err(|error| turn_composition_error(error.into()))?;
        // Shutdown closes admission under this same lock before revoking the
        // registry. No asynchronous binding read may publish a lease afterwards.
        let state = lock_state(&self.inner);
        if !state.accepting {
            return Err(TurnError::ShuttingDown);
        }
        let mut leases = self
            .inner
            .continuations
            .lock()
            .expect("continuation registry poisoned");
        leases.retain(|_, entry| entry.upgrade().is_some());
        if leases
            .get(header.session_id())
            .and_then(rsi_agent_turn_protocol::WeakContinuationLease::upgrade)
            .is_some()
        {
            return Err(TurnError::Invalid(
                "Session already has a retained continuation owner".into(),
            ));
        }
        if leases.len() >= 64 {
            return Err(TurnError::Capacity);
        }
        let lease =
            self.inner
                .continuation_issuer
                .issue(header.clone(), composition.clone(), binding);
        if !armed {
            lease.revoke();
        }
        leases.insert(header.session_id().clone(), lease.downgrade());
        Ok(lease)
    }
}

impl AgentKernel {
    async fn continuation_binding_states(
        &self,
        session: &SubmitSession,
        binding: &ContinuationBinding,
    ) -> TurnResult<Vec<rsi_agent_session_protocol::DomainSnapshot>> {
        match session {
            SubmitSession::Fresh(prepared) => {
                if binding.revision.get() != 0 || binding.initial_input.is_none() {
                    return Err(TurnError::Invalid(
                        "draft continuation requires its frozen first allocation".into(),
                    ));
                }
                Ok(prepared.baseline().initial_states())
            }
            SubmitSession::Resume(_) => {
                let page = observation::read_domain_states_bounded(
                    &self.inner,
                    session.session_id(),
                    None,
                )
                .await
                .map_err(turn_store_error)?;
                if !page.states.iter().any(|state| {
                    state.snapshot.identity() == &binding.domain
                        && state.head.revision == binding.revision
                }) {
                    return Err(TurnError::ContinuationDisarmed);
                }
                Ok(page
                    .states
                    .into_iter()
                    .map(|state| state.snapshot)
                    .collect())
            }
        }
    }

    fn continuation_session<'a>(
        &self,
        session: &'a SubmitSession,
    ) -> TurnResult<(&'a SessionHeader, &'a AgentCompositionPin)> {
        match session {
            SubmitSession::Fresh(prepared) => Ok((prepared.header(), prepared.composition())),
            SubmitSession::Resume(prepared) => self.inner.resume_issuer.inspect(prepared),
        }
    }

    fn validate_continuation(
        &self,
        lease: &ContinuationLease,
        header: &SessionHeader,
        composition: &AgentCompositionPin,
        armed: bool,
    ) -> TurnResult<()> {
        let (expected_header, expected_composition) =
            self.inner.continuation_issuer.inspect(lease)?;
        if (armed && !lease.is_armed())
            || expected_header != header
            || !expected_composition.same_generation(composition)
        {
            return Err(TurnError::ContinuationDisarmed);
        }
        let current = self
            .inner
            .continuations
            .lock()
            .expect("continuation registry poisoned")
            .get(header.session_id())
            .and_then(rsi_agent_turn_protocol::WeakContinuationLease::upgrade);
        if !current.is_some_and(|current| current.same_lease(lease)) {
            return Err(TurnError::ContinuationDisarmed);
        }
        Ok(())
    }

    async fn verify_reservation(
        &self,
        lease: &ContinuationLease,
        session: &SubmitSession,
        input: &ContinuationInput,
        provenance: &ContinuationProvenance,
    ) -> TurnResult<DomainRevision> {
        match provenance {
            ContinuationProvenance::Baseline { snapshot_sha256 } => {
                let SubmitSession::Fresh(prepared) = session else {
                    return Err(TurnError::Invalid(
                        "baseline input admission requires its frozen draft".into(),
                    ));
                };
                let snapshot = prepared
                    .baseline()
                    .initial_states()
                    .into_iter()
                    .find(|snapshot| snapshot.identity() == &lease.binding().domain)
                    .ok_or(TurnError::ContinuationDisarmed)?;
                if lease.binding().initial_input.as_ref() != Some(input)
                    || &snapshot
                        .sha256()
                        .map_err(|error| TurnError::Invalid(error.to_string()))?
                        != snapshot_sha256
                    || snapshot_sha256 != &lease.binding().snapshot_sha256
                {
                    return Err(TurnError::ContinuationDisarmed);
                }
                self.inner
                    .continuation_issuer
                    .set_revision(lease, DomainRevision::new(1))?;
                Ok(DomainRevision::new(1))
            }
            ContinuationProvenance::Command { request_id } => {
                let receipt = self
                    .domain_request(session.session_id(), request_id)
                    .await?
                    .ok_or(TurnError::ContinuationDisarmed)?;
                if !matches!(receipt.commit().source(), DomainMutationSource::Continuation { domain, owner, reservation: Some(reserved), .. }
                    if domain == &lease.binding().domain && owner == &input.owner && reserved == input)
                {
                    return Err(TurnError::ContinuationDisarmed);
                }
                let [update] = receipt.commit().updates() else {
                    return Err(TurnError::ContinuationDisarmed);
                };
                if update.snapshot().identity() != &lease.binding().domain {
                    return Err(TurnError::ContinuationDisarmed);
                }
                let page = observation::read_domain_states_bounded(
                    &self.inner,
                    session.session_id(),
                    None,
                )
                .await
                .map_err(turn_store_error)?;
                if !page.states.iter().any(|state| {
                    state.snapshot.identity() == &lease.binding().domain
                        && state.head.revision == lease.revision()
                }) {
                    return Err(TurnError::ContinuationDisarmed);
                }
                Ok(update.revision())
            }
        }
    }

    pub(super) async fn continuation_claim_allowed(
        &self,
        session: &SessionId,
        source: &ContinuationSource,
        prepared: Option<&PreparedResumeSession>,
    ) -> TurnResult<bool> {
        let lease = self
            .inner
            .continuations
            .lock()
            .expect("continuation registry poisoned")
            .get(session)
            .and_then(rsi_agent_turn_protocol::WeakContinuationLease::upgrade);
        let Some(lease) = lease.filter(ContinuationLease::is_armed) else {
            return Ok(false);
        };
        if let Some(prepared) = prepared {
            let (header, composition) = self.inner.resume_issuer.inspect(prepared)?;
            if self
                .validate_continuation(&lease, header, composition, true)
                .is_err()
            {
                return Ok(false);
            }
        }
        let page = observation::read_domain_states_bounded(&self.inner, session, None)
            .await
            .map_err(turn_store_error)?;
        Ok(page.states.iter().any(|state| {
            state.snapshot.identity() == &source.domain && lease.guards(source, state.head.revision)
        }))
    }

    pub(super) async fn discard_continuation_admitted(
        &self,
        session: &SessionId,
        scan: &DurableMessageScan,
        entry: &DurableMessageEntry,
        reason: MessageDiscardReason,
    ) -> TurnResult<MessageReceipt> {
        if entry.state != MessageState::Pending {
            return Ok(message_receipt(session, scan.durable_fact_seq, entry));
        }
        let seq = scan
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        let control = AgentControlRecord::new(
            seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::MessageDiscarded {
                message_id: entry.message.message_id.clone(),
                reason,
            },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        self.commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: scan.durable_fact_seq,
                expected_control_seq: scan.durable_control_seq,
                header: None,
                facts: Vec::new(),
                controls: vec![control],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await?
        .map_err(turn_store_error)?;
        self.inner.session_changes.committed(session);
        Ok(MessageReceipt {
            session_id: session.clone(),
            message_id: entry.message.message_id.clone(),
            accepted_control_seq: entry.accepted_control_seq,
            observed_fact_seq: scan.durable_fact_seq,
            state: MessageState::Discarded {
                reason,
                control_seq: seq,
            },
        })
    }

    pub(super) async fn discard_stale_continuation(
        &self,
        session: &SessionId,
        message: &MessageId,
    ) -> TurnResult<bool> {
        let _admission = self.inner.submission_admission.acquire(session).await?;
        self.fence_pending_terminal(session).await?;
        let scan = scan_durable_messages(&self.inner, session, Some(message)).await?;
        let Some(entry) = &scan.selected else {
            return Ok(false);
        };
        if let AgentMessageSource::Continuation { source } = &entry.message.source
            && entry.state == MessageState::Pending
            && !self
                .continuation_claim_allowed(session, source, None)
                .await?
        {
            self.discard_continuation_admitted(
                session,
                &scan,
                entry,
                MessageDiscardReason::ContinuationDisarmed,
            )
            .await?;
            return Ok(true);
        }
        Ok(false)
    }
}
