//! Ordered callbacks run outside framework locks and submit one validated stage.

use super::*;
use crate::execution_support::turn_failure;
use rsi_agent_composition_protocol::{
    ContributionBatch, ContributionContext, ContributionKind, ContributionResult,
    ContributionStage, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::{ContributionId, DomainRequestId, ToolRejection};

const STAGE_DEADLINE: Duration = Duration::from_secs(30);

struct StageRun<'a> {
    context: ContributionContext,
    stage: ContributionStage,
    deadline: tokio::time::Instant,
    cancellation: &'a CancellationToken,
    stop: &'a CancellationToken,
    token: CancellationToken,
    _guard: tokio_util::sync::DropGuard,
}

impl<'a> StageRun<'a> {
    async fn capture(
        turns: &dyn TurnExecution,
        claim: &TurnClaim,
        stage: ContributionStage,
        cancellation: &'a CancellationToken,
        stop: &'a CancellationToken,
    ) -> std::result::Result<Self, DriveFailure> {
        let deadline = tokio::time::Instant::now() + STAGE_DEADLINE;
        let token = cancellation.child_token();
        let guard = token.clone().drop_guard();
        let context = tokio::select! {
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
            () = cancellation.cancelled() => return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = tokio::time::timeout_at(deadline, turns.contribution_context(claim, token.clone())) => {
                result.map_err(|_| failed("contribution.timeout", format!("framework/{stage:?}: snapshot deadline")))?
                    .map_err(turn_failure)?
            }
        };
        Ok(Self {
            context,
            stage,
            deadline,
            cancellation,
            stop,
            token,
            _guard: guard,
        })
    }

    async fn before_step(
        turns: &dyn TurnExecution,
        claim: &TurnClaim,
        explicit: Option<&rsi_agent_session_protocol::ModelSelection>,
        has_callbacks: bool,
        cancellation: &'a CancellationToken,
        stop: &'a CancellationToken,
    ) -> std::result::Result<(rsi_agent_session_protocol::ModelSelection, Option<Self>), DriveFailure>
    {
        let run = if explicit.is_none() || has_callbacks {
            Some(
                Self::capture(
                    turns,
                    claim,
                    ContributionStage::BeforeStep,
                    cancellation,
                    stop,
                )
                .await?,
            )
        } else {
            None
        };
        let domains = run
            .as_ref()
            .map_or(&[][..], |run| run.context.domains.as_ref());
        let selection =
            rsi_agent_model_selection::resolve_selection(claim.header(), domains, explicit)
                .map_err(|error| failed("model.selection", error.to_string()))?;
        Ok((selection, run))
    }

    async fn callback<T>(
        &self,
        producer: &ContributionId,
        future: impl Future<Output = ContributionResult<T>>,
    ) -> std::result::Result<T, DriveFailure> {
        let label = format!("{producer}/{:?}", self.stage);
        tokio::select! {
            () = self.stop.cancelled() => Err(DriveFailure::Stopped),
            () = self.cancellation.cancelled() => Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = tokio::time::timeout_at(self.deadline, AssertUnwindSafe(future).catch_unwind()) => {
                result.map_err(|_| failed("contribution.timeout", format!("{label}: stage deadline")))?
                    .map_err(|_| failed("contribution.panic", format!("{label}: callback panicked")))?
                    .map_err(|error| failed("contribution.failed", format!("{label}: {}", bounded(&error.to_string()))))
            }
        }
    }
}

impl Driver {
    pub(super) async fn before_step(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        explicit: Option<&rsi_agent_session_protocol::ModelSelection>,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<rsi_agent_session_protocol::ModelSelection, DriveFailure> {
        let has_callbacks = composition
            .contributions()
            .entries()
            .iter()
            .any(|entry| entry.stage() == ContributionStage::BeforeStep);
        let (selection, run) = StageRun::before_step(
            self.turns.as_ref(),
            claim,
            explicit,
            has_callbacks,
            cancellation,
            stop,
        )
        .await?;
        if let Some(run) = run {
            self.run_captured_contributions(claim, composition, fold, run, &[])
                .await?;
        }
        Ok(selection)
    }

    #[allow(clippy::too_many_arguments)] // One stage shares the exact claim, fold and two cancellation owners.
    pub(super) async fn run_contributions(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        stage: ContributionStage,
        settled: &[Arc<SessionFact>],
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let entries = composition.contributions().entries();
        if !entries.iter().any(|entry| entry.stage() == stage) {
            return Ok(());
        }
        let run = StageRun::capture(self.turns.as_ref(), claim, stage, cancellation, stop).await?;
        self.run_captured_contributions(claim, composition, fold, run, settled)
            .await
    }

    async fn run_captured_contributions(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        run: StageRun<'_>,
        settled: &[Arc<SessionFact>],
    ) -> std::result::Result<(), DriveFailure> {
        let stage = run.stage;
        let entries = composition.contributions().entries();
        let mut batch = ContributionBatch::default();
        for entry in entries.iter().filter(|entry| entry.stage() == stage) {
            let output = run
                .callback(entry.id(), async {
                    match entry.kind() {
                        ContributionKind::Context(callback) => {
                            callback.contribute(&run.context, run.token.clone()).await
                        }
                        ContributionKind::PostTool(callback) => {
                            callback
                                .contribute(&run.context, settled, run.token.clone())
                                .await
                        }
                        ContributionKind::ToolPolicy(_)
                        | ContributionKind::ToolSettlement(_)
                        | ContributionKind::Command(_)
                        | ContributionKind::Projection(_)
                        | ContributionKind::ResourceRead(_) => {
                            unreachable!("non-execution callbacks use their own dispatch")
                        }
                    }
                })
                .await?;
            batch
                .append(entry.id(), claim.turn_id(), &run.context.step_id, output)
                .map_err(|error| {
                    failed(
                        "contribution.output",
                        format!("{}/{stage:?}: {error}", entry.id()),
                    )
                })?;
        }
        let (facts, proposals) = batch.into_parts();
        // Close captured readers before entering the mutation boundary. Never rerun callbacks
        // in response to a commit acknowledgement failure.
        drop(run);
        if !proposals.is_empty() {
            let request_id =
                DomainRequestId::new(next_effect_id().map_err(fatal)?.as_str()).map_err(fatal)?;
            let result = self
                .turns
                .commit_domains(
                    claim,
                    rsi_agent_turn_protocol::DomainMutation {
                        request_id,
                        proposals,
                        facts,
                    },
                )
                .await;
            match result {
                Err(TurnError::DomainRevisionConflict { .. })
                    if stage == ContributionStage::AfterTools => {}
                result => {
                    result.map_err(turn_failure)?;
                }
            }
        } else if !facts.is_empty() {
            let entered = self.publish_apply(claim, fold, facts).await?;
            self.flush_last(claim, &entered).await?;
        }
        // Capturing may have opened a direct Turn's first Step even for an empty output.
        self.sync_fold(claim, fold).await
    }

    pub(super) async fn tool_policy_decision(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        mut request: ToolPolicyRequest<'_>,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(bool, Option<ToolRejection>), DriveFailure> {
        let stage = ContributionStage::ToolPolicy;
        let entries = composition.contributions().entries();
        if !entries.iter().any(|entry| entry.stage() == stage) {
            return Ok((request.require_approval, None));
        }
        let run = StageRun::capture(self.turns.as_ref(), claim, stage, cancellation, stop).await?;
        for entry in entries {
            let ContributionKind::ToolPolicy(callback) = entry.kind() else {
                continue;
            };
            let decision = run
                .callback(entry.id(), async {
                    callback
                        .decide(&run.context, &request, run.token.clone())
                        .await
                })
                .await?;
            match decision {
                ToolPolicyDecision::Abstain => {}
                ToolPolicyDecision::RequireApproval => request.require_approval = true,
                ToolPolicyDecision::Deny { reason } => {
                    let rejection = ToolRejection::PolicyDenied {
                        contribution_id: entry.id().clone(),
                        reason,
                    };
                    rejection.validate().map_err(|error| {
                        failed(
                            "contribution.output",
                            format!("{}/{stage:?}: {error}", entry.id()),
                        )
                    })?;
                    return Ok((request.require_approval, Some(rejection)));
                }
            }
        }
        Ok((request.require_approval, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn step_capture_is_skipped_for_overrides_and_bounded_when_required() {
        let (claim, _) = crate::tests::claim();
        let explicit =
            rsi_agent_session_protocol::ModelSelection::baseline(claim.header().settings());
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let turns = crate::tests::pending_context(tokens.clone());
        let cancellation = CancellationToken::new();
        let stop = CancellationToken::new();
        let (selection, run) = StageRun::before_step(
            turns.as_ref(),
            &claim,
            Some(&explicit),
            false,
            &cancellation,
            &stop,
        )
        .await
        .unwrap();
        assert_eq!(selection, explicit);
        assert!(run.is_none());
        assert!(tokens.lock().unwrap().is_empty());
        for mode in 0..3 {
            let cancellation = CancellationToken::new();
            let stop = CancellationToken::new();
            let mut pending = Box::pin(StageRun::before_step(
                turns.as_ref(),
                &claim,
                (mode != 0).then_some(&explicit),
                mode != 0,
                &cancellation,
                &stop,
            ));
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
            assert_eq!(tokens.lock().unwrap().len(), mode + 1);
            match mode {
                0 => tokio::time::advance(Duration::from_secs(31)).await,
                1 => cancellation.cancel(),
                _ => stop.cancel(),
            }
            let result = pending.await;
            assert!(match (mode, result) {
                (0, Err(DriveFailure::Turn(TurnOutcome::Failed { code, .. }))) =>
                    code == "contribution.timeout",
                (1, Err(DriveFailure::Turn(TurnOutcome::Cancelled)))
                | (2, Err(DriveFailure::Stopped)) => true,
                _ => false,
            });
            assert!(tokens.lock().unwrap().last().unwrap().is_cancelled());
        }
    }
}
