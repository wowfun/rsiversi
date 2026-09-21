use crate::{LanguageContract, LanguageService, Output, Query};
use async_trait::async_trait;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutputDeclaration,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolTimeoutPolicy, TypedToolOutput,
};
use serde_json::{Value, json};
use std::sync::Arc;
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::InvalidInput(e.to_string())
}
/// Output declaration shared by model calls and independent addon tests.
pub fn output_declaration() -> rsi_tools_protocol::Result<ToolOutputDeclaration> {
    let position = json!({"type":"object","required":["line","character"],"properties":{"line":{"type":"integer"},"character":{"type":"integer"}},"additionalProperties":false});
    let range = json!({"type":["object","null"],"properties":{"start":position,"end":position},"additionalProperties":false});
    let query = json!({"type":"object","required":["operation","path","line","column"],"properties":{"operation":{"enum":["definition","references","implementation","hover"]},"path":{"type":"string"},"line":{"type":"integer"},"column":{"type":"integer"}},"additionalProperties":false});
    ToolOutputDeclaration::new(
        "rsi.lsp.query",
        1,
        json!({"type":"object","required":["query","result"],"properties":{"query":query,"result":{"type":"object","required":["kind"],"properties":{"kind":{"enum":["locations","hover"]},"locations":{"type":"array","items":{"type":"object","required":["path","range"],"properties":{"path":{"type":"string"},"range":range},"additionalProperties":false}},"text":{"type":"string"},"range":range},"additionalProperties":false}},"additionalProperties":false}),
    )
}
fn query_schema() -> Value {
    json!({"type":"object","required":["operation","path","line","column"],"properties":{"operation":{"enum":["definition","references","implementation","hover"]},"path":{"type":"string","minLength":1,"maxLength":4096},"line":{"type":"integer","minimum":1,"maximum":1_048_577},"column":{"type":"integer","minimum":1,"maximum":1_048_577}},"additionalProperties":false})
}
/// Agent-only tool contribution; workspace authority comes from the actual caller.
#[derive(Debug, Clone, Default)]
pub struct LanguageToolsFactory;
#[async_trait]
impl PluginFactory for LanguageToolsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("language Tool config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<LanguageContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let output =
            TypedToolOutput::new(output_declaration().map_err(meta)?, |output: &Output| {
                vec![ToolContent::Text {
                    text: serde_json::to_string(output).expect("closed language result"),
                }]
            });
        let definition=ToolDefinition::new("lsp_query","Read definitions, references including declarations, implementations, or hover. path is relative to this Session workspace; line and column are one-based Unicode-scalar coordinates. Returned ranges use zero-based UTF-16. Queries synchronize current files. No edits, commands, server installation or outside-workspace locations are supported.",query_schema()).map_err(meta)?;
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: Some(output.declaration().clone()),
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 35_000 },
                executor: Arc::new(Executor {
                    owner: plan.local::<LanguageContract>()?,
                    output,
                }),
            }])
            .map_err(meta)?;
        plan.defer(
            "release language Tool",
            Box::new(move || Box::pin(async move { lease.retire().map_err(|e| e.to_string()) })),
        )
    }
}
struct Executor {
    owner: Arc<LanguageService>,
    output: TypedToolOutput<Output>,
}
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageTool").finish_non_exhaustive()
    }
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
                ToolError::InvalidInput("Language query requires native Agent authority".into())
            })?;
        if serde_json::to_vec(&arguments)
            .map_err(|_| ToolError::InvalidInput("Invalid language query".into()))?
            .len()
            > 8192
        {
            return Err(ToolError::InvalidInput(
                "Language query exceeds bound".into(),
            ));
        }
        let query: Query = serde_json::from_value(arguments)
            .map_err(|_| ToolError::InvalidInput("Invalid language query".into()))?;
        match self
            .owner
            .query(
                caller.header().canonical_cwd().into(),
                query,
                execution.cancellation.clone(),
            )
            .await
        {
            Ok(output) => self.output.result(&output),
            Err(error) => ToolResult::new(
                json!({"error":error.to_string()}),
                vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                true,
            ),
        }
    }
}
