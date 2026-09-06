use super::*;
use rsi_agent_turn_protocol::{TurnExecution, TurnExecutionContract};
use rsi_tools_protocol::{ToolError, ToolTimeoutPolicy};
use rsi_user_questions_protocol::{
    Question, QuestionError, QuestionRequest, UserQuestions, UserQuestionsContract,
    validate_questions,
};

/// Separate root-only synchronous question contribution.
#[derive(Clone, Debug, Default)]
pub struct QuestionToolsFactory;

#[async_trait]
impl PluginFactory for QuestionToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "question Tool configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(Value::Null, (), 0)
            .requiring_local::<ToolRegistrarContract>()
            .requiring_local::<TurnExecutionContract>()
            .requiring_local::<UserQuestionsContract>())
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let definition = ToolDefinition::new("ask_user",
            "Ask the user one to three questions and wait for answers. Suggested options are optional; free text is always accepted. Only the root Agent may ask; child Agents should message their parent.",
            json!({"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":3,"items":{
                "type":"object","properties":{"id":{"type":"string","minLength":1,"maxLength":256},"prompt":{"type":"string","minLength":1},
                "options":{"type":"array","maxItems":8,"items":{"type":"string","minLength":1}}},"required":["id","prompt"],"additionalProperties":false
            }}},"required":["questions"],"additionalProperties":false}))
            .map_err(|error| MetaError::Activation(error.to_string()))?.with_scheduling(ToolScheduling::ExclusiveFinal);
        let executor = Arc::new(AskUser {
            turns: plan.local::<TurnExecutionContract>()?,
            questions: plan.local::<UserQuestionsContract>()?,
        });
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                timeout: ToolTimeoutPolicy::HumanInteraction,
                executor,
            }])
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release question Tool contribution",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}

#[derive(Debug)]
struct AskUser {
    turns: Arc<dyn TurnExecution>,
    questions: Arc<dyn UserQuestions>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    questions: Vec<Question>,
}

#[async_trait]
impl ToolExecutor for AskUser {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| {
                ToolError::InvalidInput("ask_user requires Agent caller authority".into())
            })?;
        if caller.header().fork_origin().is_some() {
            return tool_error(
                "root_only",
                "Only the root Agent can ask the user. Send this question to your parent.",
            );
        }
        let arguments: Arguments = serde_json::from_value(arguments)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        validate_questions(&arguments.questions).map_err(question_error)?;
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|error| ToolError::Execution(error.to_string()))?;
        let request = QuestionRequest {
            id: format!("question-{:032x}", u128::from_le_bytes(entropy)),
            session_id: caller.session_id().to_string(),
            turn_id: caller.turn_id().to_string(),
            questions: arguments.questions,
        };
        request.validate().map_err(question_error)?;
        let parking = execution
            .extension::<ToolLaneParkingAuthority>()
            .ok_or_else(|| {
                ToolError::InvalidInput("ask_user requires exclusive-final admission".into())
            })?;
        let waiting = self
            .turns
            .park_human_wait(caller.claim(), (*parking).clone())
            .await
            .map_err(wait_error)?;
        let answer = self
            .questions
            .ask(request, execution.cancellation.clone())
            .await;
        waiting
            .resume(execution.cancellation.clone())
            .await
            .map_err(wait_error)?;
        let answer = answer.map_err(question_error)?;
        let value = serde_json::to_value(answer)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        ToolResult::new(
            value.clone(),
            vec![ToolContent::Text {
                text: value.to_string(),
            }],
            false,
        )
    }
}

fn question_error(error: QuestionError) -> ToolError {
    match error {
        QuestionError::Cancelled => ToolError::Cancelled,
        error @ QuestionError::Invalid(_) => ToolError::InvalidInput(error.to_string()),
        error @ (QuestionError::Capacity | QuestionError::Conflict) => {
            ToolError::Execution(error.to_string())
        }
    }
}
fn wait_error(error: TurnError) -> ToolError {
    match error {
        TurnError::Cancelled => ToolError::Cancelled,
        TurnError::ShuttingDown => ToolError::ShuttingDown,
        other => ToolError::Execution(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_capacity_and_identity_faults_are_execution_failures() {
        for error in [QuestionError::Capacity, QuestionError::Conflict] {
            assert!(matches!(question_error(error), ToolError::Execution(_)));
        }
        assert!(matches!(
            question_error(QuestionError::Invalid("bad input".into())),
            ToolError::InvalidInput(_)
        ));
        assert_eq!(
            question_error(QuestionError::Cancelled),
            ToolError::Cancelled
        );
    }
}
