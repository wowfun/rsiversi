use super::{Arc, Contract, Manager, Principal, Reply, wire};
use async_trait::async_trait;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutputDeclaration,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolRunPolicy, TypedToolOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) struct Factory;
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("Host Profile Tool requires null configuration"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<Contract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let target = json!({"type":"object","required":["root","profile","leaf"],"additionalProperties":false,"properties":{"root":{"type":"string"},"profile":{"type":"string"},"leaf":{"type":"string"}}});
        let change = json!({"anyOf":[
            {"type":"object","required":["kind","enabled"],"additionalProperties":false,"properties":{"kind":{"const":"enabled"},"enabled":{"type":"boolean"}}},
            {"type":"object","required":["kind","value"],"additionalProperties":false,"properties":{"kind":{"const":"configuration"},"value":{"description":"Complete literal JSON replacement, at most 64 KiB."}}}
        ]});
        let definition = ToolDefinition::new("host_profile", "Review single-leaf changes in an existing writable Host Profile. Every call requires Full access or approval. catalog is redacted. preview computes a review digest without retaining resources; commit supplies that digest, target and complete change again. Results are not retained. After reply loss inspect current source and runtime state before further work; this does not prove the original outcome. Saving source is separate from runtime application. No operation grants permission.", json!({
            "type":"object", "required":["operation"], "additionalProperties":false,
            "properties":{
                "operation":{"type":"string","enum":["catalog","preview","commit"]},
                "query":{"type":"object","properties":{"profile":{"type":["string","null"]},"after":{"type":["string","null"]}},"additionalProperties":false},
                "request":{"anyOf":[
                    {"type":"object","required":["target","change"],"additionalProperties":false,"properties":{"target":target,"change":change}},
                    {"type":"object","required":["host_epoch","target","change","digest"],"additionalProperties":false,"properties":{"host_epoch":{"type":"string"},"target":target,"change":change,"digest":{"type":"string"}}}
                ]}
            }
        })).map_err(meta)?.requiring_full_access();
        let output = TypedToolOutput::new(ToolOutputDeclaration::new("rsi.profile-leaves", 2, json!({
            "type":"object","required":["operation","data"],"properties":{"operation":{"type":"string"},"data":{}},"additionalProperties":false
        })).map_err(meta)?, |value: &Output| vec![ToolContent::Text { text: serde_json::to_string(value).expect("bounded Profile metadata") }]);
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: Some(output.declaration().clone()),
                run_policy: ToolRunPolicy::execution(std::time::Duration::from_mins(1)),
                executor: Arc::new(Executor {
                    owner: plan.local::<Contract>()?,
                    output,
                }),
            }])
            .map_err(meta)?;
        plan.defer(
            "release Host Profile Tool",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Catalog {
        #[serde(default)]
        query: wire::CatalogRequest,
    },
    Preview {
        request: wire::PreviewRequest,
    },
    Commit {
        request: wire::Commit,
    },
}
#[derive(Serialize)]
struct Output {
    operation: &'static str,
    data: Value,
}
struct Executor {
    owner: Arc<Manager>,
    output: TypedToolOutput<Output>,
}
fn admission_error(error: rsi_api_protocol::ApiError) -> ToolError {
    match error {
        rsi_api_protocol::ApiError::Capacity => ToolError::Capacity,
        rsi_api_protocol::ApiError::ShuttingDown => ToolError::ShuttingDown,
        error => ToolError::Execution(format!("Host Profile operation was not admitted: {error}")),
    }
}
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProfileExecutor")
            .finish_non_exhaustive()
    }
}
fn encode<T: Serialize>(result: Reply<T>) -> Reply<Value> {
    match result {
        Ok(Ok(value)) => Ok(Ok(
            serde_json::to_value(value).map_err(|_| rsi_api_protocol::ApiError::Unavailable)?
        )),
        Ok(Err(error)) => Ok(Err(error)),
        Err(rsi_api_protocol::ApiError::Unauthorized) => Ok(Err(wire::Failure::Unauthorized)),
        Err(error) => Err(error),
    }
}
impl Manager {
    async fn tool(&self, principal: Principal, input: Input) -> Reply<Output> {
        let (operation, data) = match input {
            Input::Catalog { query } => ("catalog", encode(self.catalog(principal, query).await)),
            Input::Preview { request } => {
                ("preview", encode(self.preview(principal, request).await))
            }
            Input::Commit { request } => ("commit", encode(self.commit(principal, request).await)),
        };
        Ok(data?.map(|data| Output { operation, data }))
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
                ToolError::InvalidInput(
                    "Host Profile management requires a live native caller".into(),
                )
            })?;
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let principal = Principal::Agent(caller.session_id().clone());
        let input = serde_json::from_value(arguments)
            .map_err(|_| ToolError::InvalidInput("Invalid Host Profile request".into()))?;
        let result = match self.owner.run(&principal.clone(), move |owner| {
            Box::pin(async move { owner.tool(principal, input).await })
        }) {
            Ok(work) => work.await,
            Err(error) => return Err(admission_error(error)),
        };
        match result {
            Ok(Ok(output)) => self.output.result(&output),
            Ok(Err(error)) => ToolResult::new(json!({"failure":error}), vec![ToolContent::Text { text: format!("Host Profile operation rejected: {error:?}") }], true),
            Err(rsi_api_protocol::ApiError::OutcomeUnknown) => ToolResult::new(json!({"outcome":"unknown"}), vec![ToolContent::Text { text: "Host Profile outcome unknown. Inspect current source and runtime state before further work; do not blindly repeat the write.".into() }], true),
            Err(error) => Err(admission_error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definite_admission_refusals_do_not_claim_an_unknown_write() {
        use rsi_api_protocol::ApiError;
        assert_eq!(admission_error(ApiError::Capacity), ToolError::Capacity);
        assert_eq!(
            admission_error(ApiError::ShuttingDown),
            ToolError::ShuttingDown
        );
        let error = admission_error(ApiError::Unavailable).to_string();
        assert!(error.contains("not admitted"));
        assert!(!error.contains("ticket"));
    }
}
