//! Bounded orchestration over the Context-owned pure summary protocol.

use super::{
    AgentCompositionPin, CancellationToken, DriveFailure, Driver, ModelAttempt, ModelContextState,
    TurnClaim, ai_failure, context_failure, failed, failure_outcome, fatal,
};
use rsi_agent_session_protocol::{CompactionTrigger, ModelPurpose};

fn options_failure(
    error: &rsi_ai_protocol::SemanticError,
) -> rsi_agent_session_protocol::TurnOutcome {
    let code = match error.code() {
        "request.too_large" | "request.too_many_tools" | "request.tool_schemas_too_large" => {
            "context.limit"
        }
        "request.invalid_settings" | "reasoning_effort.invalid" => "model.settings",
        _ => "context.options",
    };
    failure_outcome(code, error.to_string())
}

fn admitted_options(
    budget: &rsi_agent_context::ContextBudget,
    composition: &AgentCompositionPin,
    model: &rsi_agent_session_protocol::ModelSelection,
) -> std::result::Result<
    (
        rsi_ai_protocol::LanguageRequestOptions,
        rsi_agent_context::ContextCredit,
    ),
    rsi_agent_session_protocol::TurnOutcome,
> {
    let definition_bytes = composition
        .tool_definition_weight()
        .map_err(|error| failure_outcome("context.capacity", error.to_string()))?;
    let mut credit = budget
        .reserve(
            definition_bytes
                .checked_mul(2)
                .and_then(|n| n.checked_add(2048))
                .ok_or_else(|| {
                    failure_outcome("context.capacity", "request option weight overflow")
                })?,
        )
        .map_err(|_| {
            failure_outcome(
                "context.capacity",
                "request options exceed Context capacity",
            )
        })?;
    let options = rsi_ai_protocol::LanguageRequestOptions::new(
        composition.tools().definitions(),
        rsi_ai_protocol::ToolChoice::Auto,
        Vec::new(),
        rsi_ai_protocol::ResponseFormat::Text,
        rsi_ai_protocol::LanguageSettings::default()
            .with_optional_reasoning_effort(model.reasoning_effort.clone()),
        Vec::new(),
    )
    .map_err(|error| options_failure(&error))?;
    credit.resize(options.encoded_weight()).map_err(|_| {
        failure_outcome(
            "context.capacity",
            "request options exceed Context capacity",
        )
    })?;
    Ok((options, credit))
}

impl Driver {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_model_attempt(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        model: &rsi_agent_session_protocol::ModelSelection,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        let (options, _options_credit) = admitted_options(&self.context_budget, composition, model)
            .map_err(DriveFailure::Turn)?;
        self.sync_fold(claim, fold).await?;
        let profile = self
            .language
            .describe(&model.model)
            .map_err(|error| ai_failure(&error))?
            .into_profile();
        if retry_attempt == 0 {
            self.compact(
                claim,
                fold,
                &options,
                model,
                &profile,
                None,
                cancellation,
                stop,
            )
            .await?;
        }
        let request = fold
            .build(options.clone(), &profile)
            .map_err(|error| context_failure("context.limit", &error))?;
        let result = self
            .run_model_effect(
                claim,
                request,
                ModelPurpose::Conversation,
                fold,
                model,
                &profile,
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
            &options,
            model,
            &profile,
            Some(CompactionTrigger::ProviderContextLimit),
            cancellation,
            stop,
        )
        .await?;
        let request = fold
            .build(options.clone(), &profile)
            .map_err(|error| context_failure("context.limit", &error))?;
        // This is the single ordinary resubmission. It cannot reopen the ordinary
        // retry series after capacity recovery, including on undispatched errors.
        self.run_model_effect(
            claim,
            request,
            ModelPurpose::Conversation,
            fold,
            model,
            &profile,
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
        options: &rsi_ai_protocol::LanguageRequestOptions,
        model: &rsi_agent_session_protocol::ModelSelection,
        profile: &rsi_ai_protocol::LanguageProfile,
        force: Option<CompactionTrigger>,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut trigger = force;
        for attempt in 0..2 {
            let planned_result =
                fold.plan_compaction(options, &model.model, profile, trigger.clone(), attempt > 0);
            if trigger.is_none()
                && matches!(
                    planned_result,
                    Err(rsi_agent_context::ContextError::Capacity)
                )
            {
                eprintln!("optional context compaction declined: context.capacity");
                return Ok(());
            }
            let planned = planned_result.map_err(|error| {
                let code = if matches!(error, rsi_agent_context::ContextError::Capacity) {
                    "context.capacity"
                } else if matches!(error, rsi_agent_context::ContextError::TooLarge) {
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
                    profile,
                    u8::MAX,
                    cancellation,
                    stop,
                )
                .await?
            {
                ModelAttempt::Output(_, _) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_ai_protocol::{
        HostedTool, LanguageRequestOptions, LanguageSettings, ResponseFormat, ToolChoice,
    };

    #[test]
    fn options_release_construction_headroom_but_retain_the_recovery_copy() {
        let pin = crate::tests::context_pin();
        let budget = rsi_agent_context::ContextBudget::new(8192).unwrap();
        let model = rsi_agent_session_protocol::ModelSelection {
            model: rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            reasoning_effort: None,
        };
        let (options, credit) = admitted_options(&budget, &pin, &model).unwrap();
        let weight = options.encoded_weight();
        assert_eq!(credit.bytes(), weight);
        assert_eq!(budget.used(), weight);
        let message = rsi_ai_protocol::Message::user_text("work").unwrap();
        let request_weight = weight - 2
            + serde_json::to_vec(std::slice::from_ref(&message))
                .unwrap()
                .len();
        let request_credit = budget.reserve(request_weight).unwrap();
        let request =
            rsi_ai_protocol::LanguageRequest::new_with_options(vec![message], options.clone())
                .unwrap()
                .with_retention(request_credit);
        assert_eq!(request.encoded_weight(), request_weight);
        assert_eq!(budget.used(), weight + request_weight);
        drop(request);
        assert_eq!(
            budget.used(),
            weight,
            "recovery options lost their admission"
        );
        drop(options);
        drop(credit);
        assert_eq!(budget.used(), 0);
        let held = budget.reserve(budget.maximum()).unwrap();
        assert!(
            matches!(admitted_options(&budget, &pin, &model), Err(rsi_agent_session_protocol::TurnOutcome::Failed { code, .. }) if code == "context.capacity")
        );
        assert_eq!(budget.used(), budget.maximum());
        drop(held);
    }

    #[test]
    fn options_errors_distinguish_capacity_settings_and_invalid_controls() {
        let options = |tools, hosted| {
            LanguageRequestOptions::new(
                tools,
                ToolChoice::Auto,
                hosted,
                ResponseFormat::Text,
                LanguageSettings::default(),
                vec![],
            )
        };
        let tool = rsi_tools_protocol::ToolDefinition::new(
            "read",
            "Read",
            serde_json::json!({"type":"object"}),
        )
        .unwrap();
        let too_many = options(vec![tool; rsi_ai_protocol::MAX_TOOLS + 1], vec![]).unwrap_err();
        let hosted =
            options(vec![], vec![HostedTool::WebSearch { max_uses: Some(0) }]).unwrap_err();
        let settings = LanguageSettings::default()
            .with_max_output_tokens(0)
            .unwrap_err();
        for (error, expected) in [
            (too_many, "context.limit"),
            (hosted, "context.options"),
            (settings, "model.settings"),
        ] {
            assert!(
                matches!(options_failure(&error), rsi_agent_session_protocol::TurnOutcome::Failed { code, .. } if code == expected)
            );
        }
    }
}
