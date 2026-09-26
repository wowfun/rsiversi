//! Live Host continuation admission over ordinary command and mailbox transactions.

use super::*;

mod initial;
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

    async fn reserve_initial(
        &self,
        lease: &ContinuationLease,
        session: PreparedFreshSession,
        invocation: SessionCommandInvocation,
        input: ContinuationInput,
    ) -> TurnResult<MessageReceipt> {
        let result = self
            .reserve_initial_continuation(lease, session, invocation, input)
            .await;
        if result
            .as_ref()
            .is_err_and(|error| !error.is_continuation_contention())
        {
            lease.revoke();
        }
        result
    }

    async fn wait_idle(
        &self,
        lease: &ContinuationLease,
        cancellation: CancellationToken,
    ) -> TurnResult<()> {
        let (header, composition) = self.inner.continuation_issuer.inspect(lease)?;
        let root = agent_root_and_path(header).0;
        loop {
            self.validate_continuation(lease, header, composition, true)?;
            // Subscribe to membership before the subtree read and to every current
            // descendant before the idle recheck. Child completion must wake a parent.
            let mut changes = vec![
                self.inner.session_changes.tree(&root),
                self.inner.session_changes.session(lease.session_id()),
            ];
            match self
                .inner
                .store
                .read_agent_subtree_snapshot(lease.session_id())
                .await
            {
                Ok(tree) => {
                    tree.validate().map_err(turn_store_error)?;
                    changes.extend(
                        tree.descendants.iter().map(|child| {
                            self.inner.session_changes.session(&child.status.session_id)
                        }),
                    );
                }
                Err(StoreError::NotFound(_)) => {}
                Err(error) => return Err(turn_store_error(error)),
            }
            let peers = self
                .continuation_peers(lease)
                .iter()
                .map(ContinuationLease::disarmed_token)
                .collect::<Vec<_>>();
            if self.automatic_turn_selected(lease)
                && self.automatic_session_idle(lease.session_id()).await?
            {
                return Ok(());
            }
            let changes = futures_util::future::select_all(
                changes.iter_mut().map(|change| Box::pin(change.changed())),
            );
            let peers = async {
                if peers.is_empty() {
                    std::future::pending::<()>().await;
                } else {
                    futures_util::future::select_all(
                        peers.iter().map(|peer| Box::pin(peer.cancelled())),
                    )
                    .await;
                }
            };
            tokio::select! {
                () = cancellation.cancelled() => return Err(TurnError::Cancelled),
                () = lease.disarmed() => return Err(TurnError::ContinuationDisarmed),
                () = self.inner.submission_admission.closed.cancelled() => return Err(TurnError::ShuttingDown),
                _ = changes => {},
                () = peers => {},
            }
        }
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
                if !error.is_continuation_contention() {
                    lease.revoke();
                }
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
            .get(&(header.session_id().clone(), binding.domain.id().to_owned()))
            .and_then(rsi_agent_turn_protocol::WeakContinuationLease::upgrade)
            .is_some()
        {
            return Err(TurnError::Invalid(
                "Session domain already has a retained continuation owner".into(),
            ));
        }
        for ((session_id, _), other) in leases.iter() {
            if session_id == header.session_id()
                && let Some(other) = other.upgrade()
            {
                let (other_header, other_composition) =
                    self.inner.continuation_issuer.inspect(&other)?;
                if other_header != header || !other_composition.same_generation(composition) {
                    return Err(TurnError::ContinuationDisarmed);
                }
            }
        }
        if leases.len() >= 128
            || leases
                .keys()
                .filter(|(_, domain)| domain == binding.domain.id())
                .count()
                >= 64
        {
            return Err(TurnError::Capacity);
        }
        let lease =
            self.inner
                .continuation_issuer
                .issue(header.clone(), composition.clone(), binding);
        if !armed {
            lease.revoke();
        }
        leases.insert(
            (
                header.session_id().clone(),
                lease.binding().domain.id().to_owned(),
            ),
            lease.downgrade(),
        );
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
                if binding.revision.get() != 0 {
                    return Err(TurnError::Invalid(
                        "draft continuation requires an unpublished domain revision".into(),
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
            .get(&(
                header.session_id().clone(),
                lease.binding().domain.id().to_owned(),
            ))
            .and_then(rsi_agent_turn_protocol::WeakContinuationLease::upgrade);
        if !current.is_some_and(|current| current.same_lease(lease)) {
            return Err(TurnError::ContinuationDisarmed);
        }
        Ok(())
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
            .get(&(session.clone(), source.domain.id().to_owned()))
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

impl AgentKernel {
    fn continuation_peers(&self, lease: &ContinuationLease) -> Vec<ContinuationLease> {
        self.inner
            .continuations
            .lock()
            .expect("continuation registry poisoned")
            .iter()
            .filter(|((session, _), _)| session == lease.session_id())
            .filter_map(|(_, weak)| weak.upgrade())
            .filter(|other| !other.same_lease(lease) && other.is_armed())
            .collect()
    }

    pub(super) fn automatic_turn_selected(&self, lease: &ContinuationLease) -> bool {
        let own = lease
            .demand()
            .map(|stamp| (stamp, lease.binding().domain.id()));
        let Some(own) = own else {
            return true;
        };
        !self.continuation_peers(lease).iter().any(|other| {
            other
                .demand()
                .is_some_and(|stamp| (stamp, other.binding().domain.id()) < own)
        })
    }

    pub(super) async fn automatic_session_idle(&self, session: &SessionId) -> TurnResult<bool> {
        if lock_state(&self.inner)
            .sessions
            .get(session)
            .is_some_and(|state| state.turns.values().any(|turn| turn.terminal.is_none()))
        {
            return Ok(false);
        }
        let tree = match self.inner.store.read_agent_subtree_snapshot(session).await {
            Ok(tree) => tree,
            Err(StoreError::NotFound(_)) => return Ok(true),
            Err(error) => return Err(turn_store_error(error)),
        };
        tree.validate().map_err(turn_store_error)?;
        if std::iter::once(&tree.session)
            .chain(tree.descendants.iter().map(|child| &child.status))
            .any(|status| {
                status.has_open_turn
                    || status.has_active_activation
                    || status.has_waking_message
                    || status.has_active_program
            })
        {
            return Ok(false);
        }
        Ok(self
            .inner
            .store
            .read_agent_mailbox_summary(session)
            .await
            .map_err(turn_store_error)?
            .pending_count
            == 0)
    }
}
