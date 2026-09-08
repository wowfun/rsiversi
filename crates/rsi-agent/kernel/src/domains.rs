//! Claim-owned mixed domain commits. Plugins never select provenance or a free lane.

use super::*;
use rsi_agent_session_protocol::{
    DomainMutationSource, DomainStateCommit, DomainStateUpdate, MAXIMUM_SESSION_DOMAINS,
};
use rsi_agent_turn_protocol::{DomainMutation, DomainMutationReceipt};

struct RetainedBytes {
    inner: Arc<KernelInner>,
    bytes: usize,
}

impl RetainedBytes {
    fn acquire(inner: &Arc<KernelInner>, bytes: usize) -> TurnResult<Self> {
        if bytes > MAXIMUM_PENDING_FACT_BYTES {
            return Err(TurnError::Invalid(
                "mixed domain request exceeds its byte bound".into(),
            ));
        }
        reserve_atomic_capacity(
            &inner.process_pending_bytes,
            bytes,
            inner.limits.maximum_process_pending_fact_bytes,
        )
        .map_err(turn_kernel_error)?;
        Ok(Self {
            inner: Arc::clone(inner),
            bytes,
        })
    }
}

impl Drop for RetainedBytes {
    fn drop(&mut self) {
        self.inner
            .process_pending_bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
        self.inner.process_pending_changed.notify_waiters();
    }
}

struct Candidate {
    caller: AgentCallerAuthority,
    original_fact_seq: u64,
    original_control_seq: u64,
    turn: TurnControl,
    facts: Vec<Arc<SessionFact>>,
    control: AgentControlRecord,
}

impl AgentKernel {
    #[allow(clippy::too_many_lines)] // One preparation holds Session admission across the exact prefix, request identity, CAS and budget checks.
    pub(super) async fn commit_turn_domains(
        &self,
        claim: &TurnClaim,
        mut request: DomainMutation,
    ) -> TurnResult<DomainMutationReceipt> {
        if request.proposals.is_empty()
            || request.proposals.len() > MAXIMUM_SESSION_DOMAINS
            || request.facts.len() > MAXIMUM_STORE_BATCH_FACTS
            || request.facts.iter().any(|body| {
                matches!(
                    body,
                    SessionFactBody::BudgetExhausted { .. } | SessionFactBody::TurnTerminal { .. }
                )
            })
        {
            return Err(TurnError::Invalid(
                "domain mutation requires bounded business work and cannot use the ending channel"
                    .into(),
            ));
        }
        let caller = self.agent_caller(claim)?;
        let admission = self
            .inner
            .submission_admission
            .acquire(claim.session_id())
            .await?;
        let composition = self.composition(claim)?;
        request.proposals.sort_by(|left, right| {
            left.snapshot()
                .identity()
                .id()
                .cmp(right.snapshot().identity().id())
        });
        let mut updates = Vec::with_capacity(request.proposals.len());
        for proposal in &request.proposals {
            composition
                .domains()
                .validate_proposal(proposal)
                .map_err(|error| turn_composition_error(error.into()))?;
            updates.push(
                DomainStateUpdate::new(proposal.expected_revision(), proposal.snapshot().clone())
                    .map_err(|error| TurnError::Invalid(error.to_string()))?,
            );
        }
        let live = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            if let Some(error) = &state.sessions[claim.session_id()].permanent_flush_error {
                return Err(TurnError::Flush(error.clone()));
            }
            state.sessions[claim.session_id()]
                .live_seq()
                .map_err(turn_kernel_error)?
        };
        self.flush(claim, live).await?;
        let page = observation::read_domain_states_bounded(&self.inner, claim.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        if page.durable_fact_seq != live {
            return Err(TurnError::Invariant(
                "domain mutation lost its flushed Fact prefix".into(),
            ));
        }
        let now = self.inner.clock.now_ms().max(1);
        let facts = request
            .facts
            .into_iter()
            .enumerate()
            .map(|(index, body)| {
                let seq = live
                    .checked_add(index as u64 + 1)
                    .ok_or_else(|| TurnError::Invariant("Fact sequence exhausted".into()))?;
                SessionFact::new(seq, now, body)
                    .map_err(|error| TurnError::Invalid(error.to_string()))
            })
            .collect::<TurnResult<Vec<_>>>()?;
        let commit = DomainStateCommit::new(
            Some(request.request_id.clone()),
            DomainMutationSource::Turn {
                turn_id: claim.turn_id().clone(),
            },
            updates,
        )
        .and_then(|commit| commit.with_facts(&facts))
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        // Retry identity precedes current revision, Step, effect and budget checks.
        if let Some(existing) = self
            .domain_request(claim.session_id(), &request.request_id)
            .await?
        {
            if existing.commit().request_sha256() != commit.request_sha256() {
                return Err(TurnError::DomainRequestConflict {
                    request_id: request.request_id.to_string(),
                });
            }
            self.validate_agent_caller(&caller)?;
            return Ok(existing);
        }
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
            now,
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let original = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, claim)?;
            if turn.terminal.is_some() || turn.budget_exhausted.is_some() {
                return Err(TurnError::Invalid(
                    "domain work follows the Turn ending boundary".into(),
                ));
            }
            let session = &state.sessions[claim.session_id()];
            for fact in &facts {
                validate_durable_intent_fence(session, fact.body())?;
            }
            clone_turn_control(turn)
        };
        let mut staged = clone_turn_control(&original);
        for fact in &facts {
            apply_executor_body(&mut staged, fact.body())?;
        }
        staged.budget_usage = turn_state::enforce_domain_budget(
            claim.header().settings().turn_budget(),
            &original,
            &facts,
            &control,
            now,
        )?;
        let bytes = facts
            .iter()
            .try_fold(control.encoded_len(), |bytes, fact| {
                bytes.checked_add(fact.encoded_len())
            })
            .ok_or(TurnError::Capacity)?;
        let lease = self.admit_agent_mutation(&caller, &CancellationToken::new())?;
        let candidate = Candidate {
            caller,
            original_fact_seq: live,
            original_control_seq: page.durable_control_seq,
            turn: staged,
            facts: facts.into_iter().map(Arc::new).collect(),
            control,
        };
        let kernel = self.clone();
        self.owned_commit(async move {
            let _admission = admission;
            // Quiescence waits for this tracked task before resetting the shared byte pool.
            let _retained = RetainedBytes::acquire(&kernel.inner, bytes)?;
            kernel.commit_domain_candidate(candidate, lease).await
        })
        .await
    }

    async fn commit_domain_candidate(
        &self,
        candidate: Candidate,
        lease: mutation::AgentMutationLease,
    ) -> TurnResult<DomainMutationReceipt> {
        lease.validate(self, &candidate.caller)?;
        let receipt = DomainMutationReceipt::new(
            candidate.caller.session_id().clone(),
            candidate.control.clone(),
        )?;
        let result = self
            .inner
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: candidate.caller.session_id().clone(),
                    expected_fact_seq: candidate.original_fact_seq,
                    expected_control_seq: candidate.original_control_seq,
                    header: None,
                    facts: candidate.facts.clone(),
                    controls: vec![candidate.control.clone()],
                }],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await;
        if let Err(error) = result {
            let request_id = receipt
                .commit()
                .request_id()
                .expect("domain mutation has a request");
            match self.domain_request(receipt.session_id(), request_id).await {
                Ok(Some(stored)) if stored == receipt => {}
                Ok(Some(_)) => {
                    return Err(TurnError::DomainRequestConflict {
                        request_id: request_id.to_string(),
                    });
                }
                Ok(None) => return Err(turn_store_error(error)),
                Err(_) => {
                    let unknown = TurnError::DomainOutcomeUnknown {
                        request_id: request_id.to_string(),
                    };
                    lease.fail_session(&unknown);
                    return Err(unknown);
                }
            }
        }
        if let Err(error) = self.install_domain_candidate(candidate, &lease) {
            lease.fail_session(&error);
            return Err(error);
        }
        Ok(receipt)
    }

    fn install_domain_candidate(
        &self,
        candidate: Candidate,
        lease: &mutation::AgentMutationLease,
    ) -> TurnResult<()> {
        lease.validate(self, &candidate.caller)?;
        let mut state = lock_state(&self.inner);
        let session = state
            .sessions
            .get_mut(candidate.caller.session_id())
            .ok_or(TurnError::StaleClaim)?;
        if session.durable_seq != candidate.original_fact_seq || !session.pending.is_empty() {
            return Err(TurnError::Invariant(
                "domain commit changed its resident predecessor".into(),
            ));
        }
        let turn = session
            .turns
            .get_mut(candidate.caller.turn_id())
            .ok_or(TurnError::StaleClaim)?;
        *turn = candidate.turn;
        session.durable_seq = candidate
            .facts
            .last()
            .map_or(candidate.original_fact_seq, |fact| fact.seq());
        session.flush_status.send_replace(FlushStatus {
            durable_seq: session.durable_seq,
            permanent_error: session.permanent_flush_error.clone(),
        });
        publish_live_watermarks(session);
        self.inner
            .session_changes
            .committed(candidate.caller.session_id());
        Ok(())
    }
}
