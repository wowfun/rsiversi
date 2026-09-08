//! Safe-boundary snapshot capture; callbacks receive no Kernel mutation authority.

use super::*;
use rsi_agent_composition_protocol::{
    ContributionContext, ContributionError, ContributionFactPage, ContributionFactReader,
    ContributionHorizon, ContributionResult,
};
use rsi_agent_session_protocol::{DomainStateView, StepId};

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
        cancellation: CancellationToken,
    ) -> TurnResult<ContributionContext> {
        if cancellation.is_cancelled() {
            return Err(TurnError::Invalid(
                "contribution capture was cancelled".into(),
            ));
        }
        self.ensure_contribution_step(claim).await?;
        let _admission = self
            .inner
            .submission_admission
            .acquire(claim.session_id())
            .await?;
        let (live, step_id) = {
            let state = lock_state(&self.inner);
            let turn = self.validate_contribution_claim(&state, claim)?;
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
        let page = observation::read_domain_states_bounded(&self.inner, claim.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        if page.durable_fact_seq != live {
            return Err(TurnError::Invariant(
                "contribution capture lost its flushed prefix".into(),
            ));
        }
        self.validate_contribution_claim(&lock_state(&self.inner), claim)?;
        if cancellation.is_cancelled() {
            return Err(TurnError::Invalid(
                "contribution capture was cancelled".into(),
            ));
        }
        Ok(ContributionContext {
            header: Arc::new(claim.header().clone()),
            turn_id: claim.turn_id().clone(),
            accepted_fact_seq: claim.accepted_seq(),
            step_id,
            horizon: ContributionHorizon {
                fact_seq: live,
                control_seq: page.durable_control_seq,
            },
            domains: page
                .states
                .into_iter()
                .map(|state| DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                })
                .collect(),
            facts: Arc::new(CapturedFacts {
                kernel: self.clone(),
                claim: claim.clone(),
                through: live,
                cancellation,
            }),
        })
    }

    async fn ensure_contribution_step(&self, claim: &TurnClaim) -> TurnResult<()> {
        let next = {
            let state = lock_state(&self.inner);
            let turn = self.validate_contribution_claim(&state, claim)?;
            if turn.current_step.is_some() {
                return Ok(());
            }
            state.sessions[claim.session_id()]
                .live_seq()
                .map_err(turn_kernel_error)?
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("Step sequence exhausted".into()))?
        };
        let step_id = StepId::new(format!("step-context-{next}"))
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let mut bodies = vec![SessionFactBody::StepStarted {
            turn_id: claim.turn_id().clone(),
            step_id,
        }];
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
    ) -> TurnResult<&'a TurnControl> {
        let turn = self.validate_claim(state, claim)?;
        if turn.cancel_requested || turn.terminal.is_some() || turn.budget_exhausted.is_some() {
            return Err(TurnError::Invalid(
                "contribution capture follows cancellation or the Turn ending boundary".into(),
            ));
        }
        turn_state::ensure_no_active_effect(turn)?;
        let consumed = turn
            .elapsed
            .consumed(turn.accepted_at_ms, self.inner.clock.now_ms());
        let limit = claim.header().settings().turn_budget().maximum_elapsed_ms();
        if consumed >= limit {
            return Err(TurnError::BudgetExceeded {
                dimension: BudgetDimension::Elapsed,
                consumed,
                limit,
            });
        }
        Ok(turn)
    }
}
