use super::*;

#[derive(Default)]
pub(super) struct ClaimMutationGate {
    pub(super) closed: AtomicBool,
    pub(super) retiring: AtomicBool,
    reopen: AtomicBool,
    draining: AtomicBool,
    terminal_admitted: AtomicBool,
    waiting: AtomicBool,
    pub(super) active: AtomicUsize,
    pub(super) drained: Notify,
    pub(super) stopping: CancellationToken,
}

pub(super) struct AgentMutationLease {
    inner: Weak<KernelInner>,
    session_id: SessionId,
    turn_id: TurnId,
    gate: Arc<ClaimMutationGate>,
}

pub(super) struct WaitMutationLease(AgentMutationLease);

impl std::ops::Deref for WaitMutationLease {
    type Target = AgentMutationLease;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for WaitMutationLease {
    fn drop(&mut self) {
        self.0.gate.waiting.store(false, Ordering::Release);
    }
}

pub(super) struct TerminalMutationDrain {
    inner: Weak<KernelInner>,
    session_id: SessionId,
    turn_id: TurnId,
    gate: Arc<ClaimMutationGate>,
    admitted: bool,
}

impl TerminalMutationDrain {
    pub(super) fn admit(mut self) {
        self.gate.terminal_admitted.store(true, Ordering::Release);
        self.admitted = true;
    }
}

impl Drop for TerminalMutationDrain {
    fn drop(&mut self) {
        if self.admitted {
            self.gate.draining.store(false, Ordering::Release);
            return;
        }
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut state = lock_state(&inner);
        self.gate.draining.store(false, Ordering::Release);
        self.gate.reopen.store(true, Ordering::Release);
        if let Some(turn) = state
            .sessions
            .get_mut(&self.session_id)
            .and_then(|session| session.turns.get_mut(&self.turn_id))
        {
            reopen_drained_gate(turn, &self.gate);
        }
    }
}

fn reopen_drained_gate(turn: &mut TurnControl, gate: &Arc<ClaimMutationGate>) {
    if gate.reopen.load(Ordering::Acquire)
        && !gate.retiring.load(Ordering::Acquire)
        && !gate.terminal_admitted.load(Ordering::Acquire)
        && !gate.draining.load(Ordering::Acquire)
        && gate.active.load(Ordering::Acquire) == 0
        && turn.terminal.is_none()
        && !turn.cancellation.is_cancelled()
        && let Some(owner) = &mut turn.claim
        && Arc::ptr_eq(&owner.mutations, gate)
    {
        owner.mutations = Arc::new(ClaimMutationGate::default());
    }
}

impl AgentMutationLease {
    pub(super) fn stopping(&self) -> CancellationToken {
        self.gate.stopping.clone()
    }

    pub(super) fn fail_session(&self, error: &TurnError) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut state = lock_state(&inner);
        let Some(session) = state.sessions.get_mut(&self.session_id) else {
            return;
        };
        if !session
            .turns
            .get(&self.turn_id)
            .and_then(|turn| turn.claim.as_ref())
            .is_some_and(|owner| Arc::ptr_eq(&owner.mutations, &self.gate))
        {
            return;
        }
        session
            .permanent_flush_error
            .get_or_insert_with(|| bounded_diagnostic(&format!("retained wait failed: {error}")));
        session.flush_status.send_replace(FlushStatus {
            durable_seq: session.durable_seq,
            permanent_error: session.permanent_flush_error.clone(),
        });
        inner.claim_changed.notify_waiters();
        inner.process_pending_changed.notify_waiters();
    }

    pub(super) fn validate(
        &self,
        kernel: &SessionKernel,
        caller: &AgentCallerAuthority,
    ) -> TurnResult<()> {
        kernel.validate_issued_claim(caller.claim())?;
        let state = lock_state(&kernel.inner);
        if self.session_id != *caller.session_id()
            || self.turn_id != *caller.turn_id()
            || !state
                .sessions
                .get(&self.session_id)
                .and_then(|session| session.turns.get(&self.turn_id))
                .and_then(|turn| turn.claim.as_ref())
                .is_some_and(|owner| Arc::ptr_eq(&owner.mutations, &self.gate))
        {
            return Err(TurnError::StaleClaim);
        }
        Ok(())
    }
}

impl Drop for AgentMutationLease {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut state = lock_state(&inner);
        let remaining = self.gate.active.fetch_sub(1, Ordering::AcqRel) - 1;
        if remaining == 0 {
            self.gate.drained.notify_waiters();
            if self.gate.retiring.load(Ordering::Acquire) {
                let mut requeue = false;
                if let Some(turn) = state
                    .sessions
                    .get_mut(&self.session_id)
                    .and_then(|session| session.turns.get_mut(&self.turn_id))
                    && turn
                        .claim
                        .as_ref()
                        .is_some_and(|owner| Arc::ptr_eq(&owner.mutations, &self.gate))
                {
                    requeue = super::turn_state::retire_claim(turn);
                }
                if requeue {
                    enqueue(&mut state, self.session_id.clone(), self.turn_id.clone());
                }
                inner.claim_changed.notify_waiters();
            } else if let Some(turn) = state
                .sessions
                .get_mut(&self.session_id)
                .and_then(|session| session.turns.get_mut(&self.turn_id))
            {
                reopen_drained_gate(turn, &self.gate);
            }
        }
    }
}

impl SessionKernel {
    pub(super) fn admit_wait_mutation(
        &self,
        caller: &AgentCallerAuthority,
    ) -> TurnResult<WaitMutationLease> {
        let mutation = self.admit_agent_mutation(caller, &CancellationToken::new())?;
        if mutation.gate.waiting.swap(true, Ordering::AcqRel) {
            return Err(TurnError::Invalid(
                "claim already owns a retained wait".into(),
            ));
        }
        Ok(WaitMutationLease(mutation))
    }

    pub(super) fn admit_agent_mutation(
        &self,
        caller: &AgentCallerAuthority,
        cancellation: &CancellationToken,
    ) -> TurnResult<AgentMutationLease> {
        let state = lock_state(&self.inner);
        if !state.accepting {
            return Err(TurnError::ShuttingDown);
        }
        let turn = self.validate_claim(&state, caller.claim())?;
        let gate = &turn
            .claim
            .as_ref()
            .expect("validated claim owner")
            .mutations;
        if gate.closed.load(Ordering::Acquire)
            || turn.cancellation.is_cancelled()
            || cancellation.is_cancelled()
        {
            return Err(TurnError::StaleClaim);
        }
        gate.active.fetch_add(1, Ordering::AcqRel);
        Ok(AgentMutationLease {
            inner: Arc::downgrade(&self.inner),
            session_id: caller.session_id().clone(),
            turn_id: caller.turn_id().clone(),
            gate: Arc::clone(gate),
        })
    }

    pub(super) fn retain_terminal_mutation(
        &self,
        claim: &TurnClaim,
    ) -> TurnResult<AgentMutationLease> {
        let state = lock_state(&self.inner);
        let turn = self.validate_claim(&state, claim)?;
        let gate = &turn
            .claim
            .as_ref()
            .expect("validated claim owner")
            .mutations;
        if !gate.closed.load(Ordering::Acquire) || gate.active.load(Ordering::Acquire) != 0 {
            return Err(TurnError::Invariant(
                "terminal mutation gate was not drained".into(),
            ));
        }
        gate.active.fetch_add(1, Ordering::AcqRel);
        Ok(AgentMutationLease {
            inner: Arc::downgrade(&self.inner),
            session_id: claim.session_id().clone(),
            turn_id: claim.turn_id().clone(),
            gate: Arc::clone(gate),
        })
    }

    pub(super) async fn drain_agent_mutations(
        &self,
        claim: &TurnClaim,
    ) -> TurnResult<TerminalMutationDrain> {
        let drain = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, claim)?;
            let gate = Arc::clone(
                &turn
                    .claim
                    .as_ref()
                    .expect("validated claim owner")
                    .mutations,
            );
            if gate.draining.swap(true, Ordering::AcqRel) {
                return Err(TurnError::StaleClaim);
            }
            gate.closed.store(true, Ordering::Release);
            gate.reopen.store(false, Ordering::Release);
            gate.stopping.cancel();
            TerminalMutationDrain {
                inner: Arc::downgrade(&self.inner),
                session_id: claim.session_id().clone(),
                turn_id: claim.turn_id().clone(),
                admitted: gate.terminal_admitted.load(Ordering::Acquire),
                gate,
            }
        };
        tokio::time::timeout(DURABILITY_WAIT_TIMEOUT, async {
            loop {
                let notified = drain.gate.drained.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if drain.gate.active.load(Ordering::Acquire) == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| {
            TurnError::Flush("source mutations did not drain before terminal deadline".into())
        })?;
        Ok(drain)
    }

    pub(super) async fn owned_commit<T: Send + 'static>(
        &self,
        work: impl std::future::Future<Output = TurnResult<T>> + Send + 'static,
    ) -> TurnResult<T> {
        let task = {
            let state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            self.inner.tasks.spawn(work)
        };
        task.await.map_err(|error| {
            TurnError::Invariant(format!("admitted Kernel task failed: {error}"))
        })?
    }
}

impl SessionKernel {
    pub(super) async fn interrupt_descendant(
        &self,
        caller: &AgentCallerAuthority,
        session_id: &SessionId,
        turn_id: &TurnId,
        cancellation: &CancellationToken,
    ) -> TurnResult<CancelResult> {
        let admission = self.inner.submission_admission.acquire(session_id).await?;
        self.ensure_session_loaded(session_id).await?;
        let live_seq = {
            let state = lock_state(&self.inner);
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| TurnError::SessionNotFound(session_id.to_string()))?
                .live_seq()
                .map_err(turn_kernel_error)?
        };
        self.wait_for_durable(session_id, live_seq)
            .await
            .map_err(turn_kernel_error)?;
        let (fact, turn_cancellation) = {
            let state = lock_state(&self.inner);
            let Some(session) = state.sessions.get(session_id) else {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            };
            let Some(turn) = session.turns.get(turn_id) else {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: true,
                });
            };
            if turn.terminal.is_some() || turn.cancel_requested {
                return Ok(CancelResult {
                    accepted: false,
                    already_terminal: turn.terminal.is_some(),
                });
            }
            (
                next_fact(
                    &self.inner,
                    session,
                    SessionFactBody::CancelRequested {
                        turn_id: turn_id.clone(),
                        reason: None,
                    },
                )
                .map_err(turn_kernel_error)?,
                turn.cancellation.clone(),
            )
        };
        let expected_control_seq = control_tail(&self.inner, session_id).await?;
        let source = self.admit_agent_mutation(caller, cancellation)?;
        let kernel = self.clone();
        let session_id = session_id.clone();
        let turn_id = turn_id.clone();
        self.owned_commit(async move {
            let _admission = admission;
            let _source = source;
            kernel
                .inner
                .store
                .commit_agent(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: session_id.clone(),
                        expected_fact_seq: live_seq,
                        expected_control_seq,
                        header: None,
                        facts: vec![fact.as_ref().clone()],
                        controls: Vec::new(),
                    }],
                    required_active_activations: Vec::new(),
                    quiescent_descendants_of: None,
                })
                .await
                .map_err(turn_store_error)?;
            kernel.install_committed_interrupt(&session_id, &turn_id, live_seq, fact.seq())?;
            turn_cancellation.cancel();
            kernel.inner.claim_changed.notify_waiters();
            Ok(CancelResult {
                accepted: true,
                already_terminal: false,
            })
        })
        .await
    }
}

impl SessionKernel {
    fn install_committed_interrupt(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        live_seq: u64,
        cancel_seq: u64,
    ) -> TurnResult<()> {
        let mut state = lock_state(&self.inner);
        let session = state.sessions.get_mut(session_id).ok_or_else(|| {
            TurnError::Invariant("interrupt target disappeared during admitted commit".into())
        })?;
        if session.durable_seq != live_seq || !session.pending.is_empty() {
            return Err(TurnError::Invariant(
                "interrupt target changed during admitted commit".into(),
            ));
        }
        session
            .turns
            .get_mut(turn_id)
            .ok_or_else(|| {
                TurnError::Invariant("interrupt Turn disappeared during admitted commit".into())
            })?
            .cancel_requested = true;
        session.durable_seq = cancel_seq;
        session.flush_status.send_replace(FlushStatus {
            durable_seq: cancel_seq,
            permanent_error: session.permanent_flush_error.clone(),
        });
        publish_live_watermarks(session);
        Ok(())
    }
}
