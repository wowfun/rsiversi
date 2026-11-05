//! Safe-boundary snapshot capture; callbacks receive no Kernel mutation authority.

use super::*;
use rsi_agent_composition_protocol::{
    ContributionContext, ContributionError, ContributionFactPage, ContributionFactReader,
    ContributionHorizon, ContributionResult,
};
use rsi_agent_session_protocol::{DomainStateView, SettledToolContext, StepId};

#[derive(Debug)]
struct CapturedFacts {
    kernel: AgentKernel,
    claim: TurnClaim,
    through: u64,
    cancellation: CancellationToken,
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
        program: Option<&EffectId>,
    ) -> TurnResult<ContributionContext> {
        let (_admission, live, step_id) = self
            .open_step_snapshot(claim, pending, &cancellation, program)
            .await?;
        let page = self.read_step_domains(claim, live, None).await?;
        self.finish_step_snapshot(claim, &cancellation, program)?;
        Ok(ContributionContext {
            header: Arc::new(claim.header().clone()),
            execution: claim.execution().cloned(),
            turn_id: claim.turn_id().clone(),
            accepted_fact_seq: claim.accepted_seq(),
            step_id,
            horizon: ContributionHorizon {
                fact_seq: live,
                control_seq: page.durable_control_seq,
            },
            domains: page.states.into_iter().map(domain_view).collect(),
            facts: Arc::new(CapturedFacts {
                kernel: self.clone(),
                claim: claim.clone(),
                through: live,
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
        let (_admission, live, step_id) = self
            .open_step_snapshot(claim, pending, &cancellation, None)
            .await?;
        let selection = match domain {
            Some(id) => self
                .read_step_domains(claim, live, Some(id))
                .await?
                .states
                .pop()
                .map(domain_view),
            None => None,
        };
        self.finish_step_snapshot(claim, &cancellation, None)?;
        Ok((step_id, selection))
    }

    async fn open_step_snapshot(
        &self,
        claim: &TurnClaim,
        pending: Vec<SettledToolContext>,
        cancellation: &CancellationToken,
        program: Option<&EffectId>,
    ) -> TurnResult<(SubmissionAdmissionLease, u64, StepId)> {
        if cancellation.is_cancelled() {
            return Err(TurnError::Invalid(
                "contribution capture was cancelled".into(),
            ));
        }
        if program.is_none() {
            self.ensure_contribution_step(claim, pending).await?;
        }
        let admission = self
            .inner
            .submission_admission
            .acquire(claim.session_id())
            .await?;
        let (live, step_id) = {
            let state = lock_state(&self.inner);
            let turn = self.validate_contribution_claim(&state, claim, program)?;
            let step_id = turn.current_step.clone().ok_or_else(|| {
                TurnError::Invalid("contribution capture requires an open Step".into())
            })?;
            (
                state.sessions[claim.session_id()]
                    .live_seq()
                    .map_err(turn_kernel_error)?,
                step_id,
            )
        };
        self.flush(claim, live).await?;
        Ok((admission, live, step_id))
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
        program: Option<&EffectId>,
    ) -> TurnResult<()> {
        self.validate_contribution_claim(&lock_state(&self.inner), claim, program)?;
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
        program: Option<&EffectId>,
    ) -> TurnResult<&'a TurnControl> {
        let turn = self.validate_claim(state, claim)?;
        if turn.cancel_requested || turn.terminal.is_some() {
            return Err(TurnError::Invalid(
                "contribution capture follows cancellation or the Turn ending boundary".into(),
            ));
        }
        if let Some(parent) = program {
            if turn.effects.len() != 1
                || !matches!(
                    turn.effects.get(parent),
                    Some(ActiveEffect::Tool {
                        started: true,
                        program_role: rsi_tools_protocol::ToolProgramRole::Coordinator,
                        origin: rsi_agent_session_protocol::ToolOrigin::Model { .. },
                        ..
                    })
                )
            {
                return Err(TurnError::Invalid(
                    "program policy snapshot requires its sole started coordinator".into(),
                ));
            }
        } else {
            turn_state::ensure_no_active_effect(turn)?;
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
