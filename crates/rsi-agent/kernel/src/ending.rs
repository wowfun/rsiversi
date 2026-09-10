//! Bounded Kernel-owned Step/budget/Turn endings, independent of business-work capacity.

use super::*;

pub(super) fn ending_facts(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    original: &TurnControl,
    base_seq: u64,
    proposed: &TurnOutcome,
) -> TurnResult<Vec<SessionFact>> {
    let terminal = canonicalize_terminal(
        SessionFactBody::TurnTerminal {
            turn_id: claim.turn_id().clone(),
            outcome: proposed.clone(),
        },
        original.cancel_requested,
    );
    let SessionFactBody::TurnTerminal { outcome, .. } = &terminal else {
        unreachable!("terminal body")
    };
    let mut bodies = Vec::with_capacity(3);
    if let Some(step_id) = &original.current_step {
        bodies.push(SessionFactBody::StepEnded {
            turn_id: claim.turn_id().clone(),
            step_id: step_id.clone(),
            outcome: if matches!(outcome, TurnOutcome::Completed) {
                StepOutcome::Completed
            } else {
                StepOutcome::Stopped {
                    reason: bounded_diagnostic(&format!("Turn ended with {outcome:?}")),
                }
            },
        });
    }
    if let TurnOutcome::BudgetExceeded {
        dimension,
        consumed,
        limit,
    } = outcome
        && original.budget_exhausted.is_none()
    {
        bodies.push(SessionFactBody::BudgetExhausted {
            turn_id: claim.turn_id().clone(),
            dimension: *dimension,
            consumed: *consumed,
            limit: *limit,
        });
    }
    bodies.push(terminal);
    let staged = execution::stage_execution_facts(kernel, claim, original, base_seq, bodies)?;
    enforce_turn_budget(
        claim.header().settings().turn_budget(),
        original,
        &staged.facts,
        kernel.inner.clock.now_ms().max(1),
    )?;
    Ok(staged.facts)
}

impl AgentKernel {
    pub(super) async fn reconcile_failed_ending(
        &self,
        claim: &TurnClaim,
        terminal: &SessionFact,
        lease: &mutation::AgentMutationLease,
        error: StoreError,
    ) -> TurnResult<()> {
        match read_turn_boundary_bounded(&self.inner, claim.session_id(), claim.turn_id()).await {
            Ok(boundary) if boundary.terminal() == Some(terminal) => Ok(()),
            Ok(boundary) if boundary.terminal().is_none() => Err(turn_store_error(error)),
            _ => {
                let error = turn_store_error(error);
                lease.fail_session(&error);
                Err(error)
            }
        }
    }

    pub(super) async fn finish_direct_claim(
        &self,
        claim: &TurnClaim,
        outcome: &TurnOutcome,
    ) -> TurnResult<Arc<SessionFact>> {
        let drain = self.drain_agent_mutations(claim).await?;
        let admission = self
            .inner
            .submission_admission
            .acquire(claim.session_id())
            .await?;
        let live = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            state.sessions[claim.session_id()]
                .live_seq()
                .map_err(turn_kernel_error)?
        };
        self.flush(claim, live).await?;
        let original = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, claim)?;
            if let Some(error) = &state.sessions[claim.session_id()].permanent_flush_error {
                return Err(TurnError::Flush(error.clone()));
            }
            clone_turn_control(turn)
        };
        let facts = ending_facts(self, claim, &original, live, outcome)?
            .into_iter()
            .map(Arc::new)
            .collect::<Vec<_>>();
        let terminal = facts.last().expect("ending has one terminal").clone();
        let controls = read_controls_bounded(&self.inner, claim.session_id(), 0, 1)
            .await
            .map_err(turn_store_error)?;
        let lease = self.retain_terminal_mutation(claim)?;
        let kernel = self.clone();
        let claim = claim.clone();
        self.owned_commit(async move {
            let _admission = admission;
            drain.admit();
            let result = kernel
                .inner
                .commit_agent(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: claim.session_id().clone(),
                        expected_fact_seq: live,
                        expected_control_seq: controls.durable_seq,
                        header: None,
                        facts,
                        controls: vec![],
                    }],
                    required_active_activations: vec![],
                    quiescent_descendants_of: None,
                })
                .await;
            if let Err(error) = result {
                kernel
                    .reconcile_failed_ending(&claim, &terminal, &lease, error)
                    .await?;
            }
            kernel.install_committed_terminal(&claim, live, terminal.seq())?;
            kernel.inner.session_changes.committed(claim.session_id());
            Ok(terminal)
        })
        .await
    }
}
