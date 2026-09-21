use super::*;
use async_trait::async_trait;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling, ToolTimeoutPolicy,
};
use serde_json::{Value, json};

/// Ordinary explicit read Tool; model arguments cannot select a CAS digest.
#[derive(Clone, Debug, Default)]
pub struct ReferenceToolsFactory;
#[async_trait]
impl PluginFactory for ReferenceToolsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Reference Tools configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ReferencesContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let definition = ToolDefinition::new("reference_read","Read one immutable conversation-reference page using the recorded Session, Fact and content index shown in its preview. Only references recorded in this Session or its inherited direct-parent interval are readable.",json!({
            "type":"object","properties":{
                "recorded_session_id":{"type":"string","minLength":1,"maxLength":MAXIMUM_AGENT_IDENTIFIER_BYTES},
                "fact_seq":{"type":"string","pattern":"^[1-9][0-9]{0,19}$"},
                "content_index":{"type":"integer","minimum":0,"maximum":MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS-1},
                "offset":{"type":"integer","minimum":0,"maximum":MAXIMUM_REFERENCE_TEXT_BYTES},
                "maximum":{"type":"integer","minimum":4,"maximum":MAXIMUM_REFERENCE_PAGE_BYTES}
            },"required":["recorded_session_id","fact_seq","content_index","maximum"],"additionalProperties":false
        })).map_err(|error|MetaError::Activation(error.to_string()))?.with_scheduling(ToolScheduling::ParallelSafe);
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                output: None,
                definition,
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
                executor: Arc::new(Read(plan.local::<ReferencesContract>()?)),
            }])
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release reference Tool",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}
#[derive(Debug)]
struct Read(Arc<References>);
#[async_trait]
impl ToolExecutor for Read {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let request: ReferenceReadRequest = serde_json::from_value(arguments)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        request
            .validate()
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| {
                ToolError::InvalidInput("reference_read requires Agent caller authority".into())
            })?;
        match self
            .0
            .read_recorded(
                caller.header().clone(),
                request,
                execution.cancellation.clone(),
            )
            .await
        {
            Ok(page) => ToolResult::new(
                json!({"reference":page.reference,"offset":page.offset,"next_offset":page.next_offset,"has_more":page.has_more}),
                vec![ToolContent::Text { text: page.text }],
                false,
            ),
            Err(ReferenceError::Cancelled) => Err(ToolError::Cancelled),
            Err(error) => ToolResult::new(
                json!({"error":"reference_unavailable"}),
                vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                true,
            ),
        }
    }
}
