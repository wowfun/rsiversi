use super::{Arc, HistoryContract, ProductHistorySearch, Reply, Request};
use async_trait::async_trait;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutputDeclaration,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolTimeoutPolicy, TypedToolOutput,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// Explicit history tools use the same owner and the caller's immutable workspace.
#[derive(Clone, Debug, Default)]
pub struct HistoryToolsFactory;
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
#[async_trait]
impl PluginFactory for HistoryToolsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("history Tool config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<HistoryContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let declaration=ToolOutputDeclaration::new("rsi.history",1,json!({"type":"object","required":["kind"],"properties":{"kind":{"type":"string","enum":["coverage","hits","original","frozen"]},"coverage":{"type":"object"},"hits":{"type":"array"},"next":{},"hit":{"type":"object"},"offset":{"type":"integer"},"next_offset":{"type":"integer"},"has_more":{"type":"boolean"},"text":{"type":"string"},"reference":{"type":"object"}},"additionalProperties":false})).map_err(meta)?;
        let output = TypedToolOutput::new(declaration, |reply: &Reply| {
            vec![ToolContent::Text {
                text: serde_json::to_string(reply).expect("bounded history reply"),
            }]
        });
        let definition=ToolDefinition::new("history_search","Search explicit conversation text in the caller's workspace. advance indexes one bounded batch; search reports coverage and candidate hits; read rereads a hit's original; freeze saves an exact original range as reference data for this Session. Sources exclude reasoning and provider requests. Copy a hit unchanged from search to read/freeze. Do not claim unindexed history has no matches.",json!({"type":"object","required":["conversation","request"],"properties":{"conversation":{"type":"object","required":["kind","id"],"properties":{"kind":{"enum":["native","external"]},"id":{"type":"string","maxLength":256}},"additionalProperties":false},"request":{"type":"object","required":["operation"],"properties":{"operation":{"enum":["advance","search","read","freeze","rebuild"]},"query":{"type":"string","maxLength":256},"after":{},"hit":{"type":"object"},"offset":{"type":"integer","minimum":0,"maximum":1_048_576},"start":{"type":"integer","minimum":0,"maximum":1_048_576},"end":{"type":"integer","minimum":1,"maximum":1_048_576}},"additionalProperties":false}},"additionalProperties":false})).map_err(meta)?;
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: Some(output.declaration().clone()),
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
                executor: Arc::new(Executor {
                    owner: plan.local::<HistoryContract>()?,
                    output,
                }),
            }])
            .map_err(meta)?;
        plan.defer(
            "release history Tool",
            Box::new(move || Box::pin(async move { lease.retire().map_err(|e| e.to_string()) })),
        )
    }
}
struct Executor {
    owner: Arc<ProductHistorySearch>,
    output: TypedToolOutput<Reply>,
}
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryTool").finish_non_exhaustive()
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    conversation: rsi_history_api::ConversationIdentity,
    request: Value,
}
#[async_trait]
impl ToolExecutor for Executor {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| {
                ToolError::InvalidInput("history requires a native Agent caller".into())
            })?;
        if serde_json::to_vec(&arguments)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?
            .len()
            > 16384
        {
            return Err(ToolError::InvalidInput(
                "history request exceeds 16 KiB".into(),
            ));
        }
        let mut input: Input = serde_json::from_value(arguments)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let object = input
            .request
            .as_object_mut()
            .ok_or_else(|| ToolError::InvalidInput("history request must be an object".into()))?;
        if object.contains_key("scope") || object.contains_key("target") {
            return Err(ToolError::InvalidInput(
                "history scope and target come from the actual caller".into(),
            ));
        }
        object.insert("scope".into(),json!({"workspace":hex::encode(Sha256::digest(caller.header().canonical_cwd().as_bytes())),"conversation":input.conversation}));
        if object.get("operation").and_then(Value::as_str) == Some("freeze") {
            object.insert("target".into(), json!(caller.session_id()));
        }
        let request: Request = serde_json::from_value(input.request)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        match self
            .owner
            .call(request, execution.cancellation.clone())
            .await
        {
            Ok(mut reply) => {
                // Keep worst-case escaped original text inside the Tool output's 256 KiB bound.
                if let Reply::Original {
                    hit,
                    offset,
                    next_offset,
                    has_more,
                    text,
                } = &mut reply
                {
                    let end = text.floor_char_boundary(text.len().min(32 * 1024));
                    text.truncate(end);
                    *next_offset = *offset + end;
                    *has_more = *next_offset < hit.original.end;
                }
                self.output.result(&reply)
            }
            Err(error) => ToolResult::new(
                json!({"error":"history_unavailable"}),
                vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                true,
            ),
        }
    }
}
