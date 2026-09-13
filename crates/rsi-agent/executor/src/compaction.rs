//! Bounded orchestration over the Context-owned pure summary protocol.

use super::{
    AgentCompositionPin, CancellationToken, DriveFailure, Driver, ModelAttempt, ModelContextState,
    ModelRef, TurnClaim, ai_failure, failed, fatal,
};
use rsi_agent_session_protocol::{CompactionTrigger, ModelPurpose};

impl Driver {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_model_attempt(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        model: &ModelRef,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        self.sync_fold(claim, fold).await?;
        let profile = self
            .language
            .describe(model)
            .map_err(|error| ai_failure(&error))?;
        if retry_attempt == 0 {
            self.compact(claim, fold, model, &profile, None, cancellation, stop)
                .await?;
        }
        let request = fold
            .build(composition.tools().definitions())
            .map_err(|error| failed("context.limit", error.to_string()))?;
        let result = self
            .run_model_effect(
                claim,
                request,
                ModelPurpose::Conversation,
                fold,
                model,
                retry_attempt,
                cancellation,
                stop,
            )
            .await?;
        if !matches!(result, ModelAttempt::ContextLimit) {
            return Ok(result);
        }
        self.compact(
            claim,
            fold,
            model,
            &profile,
            Some(CompactionTrigger::ProviderContextLimit),
            cancellation,
            stop,
        )
        .await?;
        let request = fold
            .build(composition.tools().definitions())
            .map_err(|error| failed("context.limit", error.to_string()))?;
        // This is the single ordinary resubmission. It cannot reopen the ordinary
        // retry series after capacity recovery, including on undispatched errors.
        self.run_model_effect(
            claim,
            request,
            ModelPurpose::Conversation,
            fold,
            model,
            u8::MAX,
            cancellation,
            stop,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn compact(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
        model: &ModelRef,
        profile: &rsi_ai_protocol::LanguageProfile,
        force: Option<CompactionTrigger>,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut trigger = force;
        for attempt in 0..2 {
            let planned = fold
                .plan_compaction(model, profile, trigger.clone(), attempt > 0)
                .map_err(|error| {
                    let code = if matches!(error, rsi_agent_context::ContextError::TooLarge) {
                        "context.limit"
                    } else {
                        "context.compaction_failed"
                    };
                    failed(code, error.to_string())
                })?;
            let Some(planned) = planned else {
                return if trigger.is_some() {
                    Err(failed(
                        "context.limit",
                        "selected builder cannot compact this input",
                    ))
                } else {
                    Ok(())
                };
            };
            trigger = Some(planned.plan.trigger.clone());
            match self
                .run_model_effect(
                    claim,
                    planned.request,
                    ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                    fold,
                    model,
                    u8::MAX,
                    cancellation,
                    stop,
                )
                .await?
            {
                ModelAttempt::Output(_) => {
                    // A steering message accepted during the summary must enter
                    // before the resumed ordinary request is prepared.
                    self.turns
                        .enter_pending_step_messages(claim)
                        .await
                        .map_err(fatal)?;
                    self.sync_fold(claim, fold).await?;
                    return Ok(());
                }
                ModelAttempt::ContextLimit if attempt == 0 => {}
                ModelAttempt::ContextLimit | ModelAttempt::Retry => {
                    return Err(failed(
                        "context.limit",
                        "bounded summary input still exceeds provider capacity",
                    ));
                }
            }
        }
        Err(failed(
            "context.compaction_failed",
            "summary pressure event exhausted its attempt bound",
        ))
    }
}
