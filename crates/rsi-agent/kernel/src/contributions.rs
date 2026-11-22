//! Safe-boundary snapshot capture; callbacks receive no Kernel mutation authority.

use super::*;
use rsi_agent_composition_protocol::{
    ContributionContext, ContributionError, ContributionFactPage, ContributionFactReader,
    ContributionHorizon, ContributionResult,
};
use rsi_agent_session_protocol::{DomainStateView, SettledToolContext, StepId, ToolOrigin};

#[derive(Debug)]
struct CapturedFacts {
    kernel: AgentKernel,
    claim: TurnClaim,
    through: u64,
    cancellation: CancellationToken,
}

struct StepSnapshot {
    _admission: SubmissionAdmissionLease,
    through: u64,
    step_id: StepId,
    pending_input_range: Option<std::ops::RangeInclusive<u64>>,
}

#[async_trait]
impl ContributionFactReader for CapturedFacts {
    async fn read(&self, after_seq: u64, limit: usize) -> ContributionResult<ContributionFactPage> {
        if self.cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        if after_seq > self.through || limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(ContributionError::Invalid(
                "captured Fact read is out of bounds".into(),
            ));
        }
        let page = self
            .kernel
            .read_facts(&self.claim, after_seq, limit)
            .await
            .map_err(|_| ContributionError::Closed)?;
        if self.cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        Ok(ContributionFactPage {
            facts: page
                .facts
                .into_iter()
                .take_while(|fact| fact.seq() <= self.through)
                .collect(),
            through_seq: page.through_seq.min(self.through),
        })
    }
}

impl AgentKernel {
    pub(super) async fn capture_contribution_context(
        &self,
        claim: &TurnClaim,
        pending: Vec<SettledToolContext>,
        cancellation: CancellationToken,
        origin: Option<&ToolOrigin>,
    ) -> TurnResult<ContributionContext> {
        let snapshot = self
            .open_step_snapshot(claim, pending, &cancellation, origin)
            .await?;
        let page = self
            .read_step_domains(claim, snapshot.through, None)
            .await?;
        self.finish_step_snapshot(claim, &cancellation, origin)?;
        Ok(ContributionContext {
            header: Arc::new(claim.header().clone()),
            execution: claim.execution().cloned(),
            turn_id: claim.turn_id().clone(),
            accepted_fact_seq: claim.accepted_seq(),
            step_id: snapshot.step_id,
            pending_input_range: snapshot.pending_input_range,
            horizon: ContributionHorizon {
                fact_seq: snapshot.through,
                control_seq: page.durable_control_seq,
            },
            domains: page.states.into_iter().map(domain_view).collect(),
            facts: Arc::new(CapturedFacts {
                kernel: self.clone(),
                claim: claim.clone(),
                through: snapshot.through,
                cancellation,
            }),
        })
    }

    pub(super) async fn capture_request_step(
        &self,
        claim: &TurnClaim,
        domain: Option<&str>,
        pending: Vec<SettledToolContext>,
        cancellation: CancellationToken,
    ) -> TurnResult<(StepId, Option<DomainStateView>)> {
        let snapshot = self
            .open_step_snapshot(claim, pending, &cancellation, None)
            .await?;
        let selection = match domain {
            Some(id) => self
                .read_step_domains(claim, snapshot.through, Some(id))
                .await?
                .states
                .pop()
                .map(domain_view),
            None => None,
        };
        self.finish_step_snapshot(claim, &cancellation, None)?;
        Ok((snapshot.step_id, selection))
    }

    async fn open_step_snapshot(
        &self,
        claim: &TurnClaim,
        pending: Vec<SettledToolContext>,
        cancellation: &CancellationToken,
        origin: Option<&ToolOrigin>,
    ) -> TurnResult<StepSnapshot> {
        if cancellation.is_cancelled() {
            return Err(TurnError::Invalid(
                "contribution capture was cancelled".into(),
            ));
        }
        if origin.is_none() {
            self.ensure_contribution_step(claim, pending).await?;
        }
        let admission = self
            .inner
            .submission_admission
            .acquire(claim.session_id())
            .await?;
        let snapshot = {
            let state = lock_state(&self.inner);
            let turn = self.validate_contribution_claim(&state, claim, origin)?;
            let step_id = turn.current_step.clone().ok_or_else(|| {
                TurnError::Invalid("contribution capture requires an open Step".into())
            })?;
            let mut inputs = turn.pending_inputs.keys();
            let pending_input_range = inputs
                .next()
                .map(|first| *first..=*inputs.next_back().unwrap_or(first));
            StepSnapshot {
                _admission: admission,
                through: state.sessions[claim.session_id()]
                    .live_seq()
                    .map_err(turn_kernel_error)?,
                step_id,
                pending_input_range,
            }
        };
        self.flush(claim, snapshot.through).await?;
        Ok(snapshot)
    }

    async fn read_step_domains(
        &self,
        claim: &TurnClaim,
        live: u64,
        domain: Option<&str>,
    ) -> TurnResult<rsi_agent_store_protocol::StoreDomainStatePage> {
        let page = observation::read_domain_state_page_bounded(
            &self.inner,
            claim.session_id(),
            None,
            domain,
        )
        .await
        .map_err(turn_store_error)?;
        if page.durable_fact_seq != live {
            return Err(TurnError::Invariant(
                "contribution capture lost its flushed prefix".into(),
            ));
        }
        Ok(page)
    }

    fn finish_step_snapshot(
        &self,
        claim: &TurnClaim,
        cancellation: &CancellationToken,
        origin: Option<&ToolOrigin>,
    ) -> TurnResult<()> {
        self.validate_contribution_claim(&lock_state(&self.inner), claim, origin)?;
        if cancellation.is_cancelled() {
            return Err(TurnError::Invalid(
                "contribution capture was cancelled".into(),
            ));
        }
        Ok(())
    }

    async fn ensure_contribution_step(
        &self,
        claim: &TurnClaim,
        pending: Vec<SettledToolContext>,
    ) -> TurnResult<()> {
        let mut bodies = Vec::new();
        {
            let state = lock_state(&self.inner);
            let turn = self.validate_contribution_claim(&state, claim, None)?;
            let new_step;
            let step = if let Some(step) = &turn.current_step {
                if pending.is_empty() {
                    return Ok(());
                }
                step
            } else {
                let next = state.sessions[claim.session_id()]
                    .live_seq()
                    .map_err(turn_kernel_error)?
                    .checked_add(1)
                    .ok_or_else(|| TurnError::Invariant("Step sequence exhausted".into()))?;
                new_step = StepId::new(format!("step-context-{next}"))
                    .map_err(|error| TurnError::Invalid(error.to_string()))?;
                bodies.push(SessionFactBody::StepStarted {
                    turn_id: claim.turn_id().clone(),
                    step_id: new_step.clone(),
                });
                &new_step
            };
            bodies.extend(SettledToolContext::claim(pending, claim.turn_id(), step));
        }
        loop {
            match self.publish(claim, bodies).await? {
                PublishAttempt::Published(_) => return Ok(()),
                PublishAttempt::FlushRequired { unpublished } => {
                    let live = lock_state(&self.inner)
                        .sessions
                        .get(claim.session_id())
                        .ok_or(TurnError::StaleClaim)?
                        .live_seq()
                        .map_err(turn_kernel_error)?;
                    self.flush(claim, live).await?;
                    bodies = unpublished;
                }
            }
        }
    }

    fn validate_contribution_claim<'a>(
        &self,
        state: &'a KernelState,
        claim: &TurnClaim,
        origin: Option<&ToolOrigin>,
    ) -> TurnResult<&'a TurnControl> {
        let turn = self.validate_claim(state, claim)?;
        if turn.cancel_requested || turn.terminal.is_some() {
            return Err(TurnError::Invalid(
                "contribution capture follows cancellation or the Turn ending boundary".into(),
            ));
        }
        match origin {
            Some(ToolOrigin::Model { effect_id }) => {
                if !turn
                    .tool_source
                    .as_ref()
                    .is_some_and(|source| source.completed_from(effect_id))
                    || !turn_state::only_parallel_model_tools(turn, effect_id)
                {
                    return Err(TurnError::Invalid(
                        "tool policy snapshot requires its completed model and compatible siblings"
                            .into(),
                    ));
                }
            }
            Some(ToolOrigin::Program {
                parent_effect_id, ..
            }) => {
                if turn.effects.len() != 1
                    || !matches!(
                        turn.effects.get(parent_effect_id),
                        Some(ActiveEffect::Tool {
                            started: true,
                            program_role: rsi_tools_protocol::ToolProgramRole::Coordinator,
                            origin: ToolOrigin::Model { .. },
                            ..
                        })
                    )
                {
                    return Err(TurnError::Invalid(
                        "program policy snapshot requires its sole started coordinator".into(),
                    ));
                }
            }
            None => turn_state::ensure_no_active_effect(turn)?,
        }
        Ok(turn)
    }
}

fn domain_view(state: rsi_agent_store_protocol::StoreDomainState) -> DomainStateView {
    DomainStateView {
        revision: state.head.revision,
        snapshot: state.snapshot,
    }
}
