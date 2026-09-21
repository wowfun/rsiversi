use super::{Arc, Contract, Manager, Principal, Reply, wire};
use async_trait::async_trait;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutputDeclaration,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolTimeoutPolicy, TypedToolOutput,
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
        let definition = ToolDefinition::new("host_profile", "Review single-leaf changes in an existing writable Host Profile. Each change needs the operator's explicit grant for this Session, exact Profile, leaf and operation. catalog is redacted. preview returns a review digest and ticket; commit saves that exact proposal; receipt reconciles it after reply loss. Never repeat an unknown write as a fresh proposal. No operation grants permission or edits a resident Session.", json!({
            "type":"object", "required":["operation"], "additionalProperties":false,
            "properties":{
                "operation":{"type":"string","enum":["catalog","preview","previews","commit","receipt","receipts","discard"]},
                "query":{"type":"object","properties":{"profile":{"type":["string","null"]},"after":{"type":["string","null"]}},"additionalProperties":false},
                "request":{"description":"For preview: target from catalog and change; for commit: host_epoch, ticket and digest copied from the exact prepared preview.","anyOf":[
                    {"type":"object","required":["target","change"],"additionalProperties":false,"properties":{
                        "target":{"type":"object","required":["root","profile","leaf"],"additionalProperties":false,"properties":{"root":{"type":"string"},"profile":{"type":"string"},"leaf":{"type":"string"}}},
                        "change":{"anyOf":[
                            {"type":"object","required":["kind","enabled"],"additionalProperties":false,"properties":{"kind":{"const":"enabled"},"enabled":{"type":"boolean"}}},
                            {"type":"object","required":["kind","value"],"additionalProperties":false,"properties":{"kind":{"const":"configuration"},"value":{"description":"Complete literal JSON replacement, at most 64 KiB; never a partial merge."}}}
                        ]}
                    }},
                    {"type":"object","required":["host_epoch","ticket","digest"],"additionalProperties":false,"properties":{"host_epoch":{"type":"string"},"ticket":{"type":"string"},"digest":{"type":"string"}}}
                ]},
                "ticket":{"description":"For receipt or discard: exact host_epoch and ticket returned by preview.","type":"object","required":["host_epoch","ticket"],"additionalProperties":false,"properties":{"host_epoch":{"type":"string"},"ticket":{"type":"string"}}}
            }
        })).map_err(meta)?;
        let output = TypedToolOutput::new(ToolOutputDeclaration::new("rsi.profile-leaves", 1, json!({
            "type":"object","required":["operation","data"],"properties":{"operation":{"type":"string"},"data":{}},"additionalProperties":false
        })).map_err(meta)?, |value: &Output| vec![ToolContent::Text { text: serde_json::to_string(value).expect("bounded Profile metadata") }]);
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: Some(output.declaration().clone()),
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 60_000 },
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
    Previews,
    Commit {
        request: wire::Commit,
    },
    Receipt {
        ticket: wire::Ticket,
    },
    Receipts,
    Discard {
        ticket: wire::Ticket,
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
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProfileExecutor")
            .finish_non_exhaustive()
    }
}
fn encode<T: Serialize>(result: Reply<T>) -> Reply<Value> {
    match result? {
        Ok(value) => Ok(Ok(
            serde_json::to_value(value).map_err(|_| rsi_api_protocol::ApiError::Unavailable)?
        )),
        Err(error) => Ok(Err(error)),
    }
}
impl Manager {
    async fn tool(&self, principal: Principal, input: Input) -> Reply<Output> {
        let (operation, data) = match input {
            Input::Catalog { query } => ("catalog", encode(self.catalog(principal, query).await)),
            Input::Preview { request } => {
                ("preview", encode(self.preview(principal, request).await))
            }
            Input::Previews => ("previews", encode(Ok(Ok(self.previews(&principal))))),
            Input::Commit { request } => ("commit", encode(self.commit(principal, request).await)),
            Input::Receipt { ticket } => ("receipt", encode(self.receipt(&principal, &ticket))),
            Input::Receipts => ("receipts", encode(Ok(Ok(self.receipts(&principal))))),
            Input::Discard { ticket } => ("discard", encode(self.discard(&principal, &ticket))),
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
        let result = match self
            .owner
            .run(move |owner| Box::pin(async move { owner.tool(principal, input).await }))
        {
            Ok(work) => work.await,
            Err(error) => Err(error),
        };
        match result {
            Ok(Ok(output)) => self.output.result(&output),
            Ok(Err(error)) => ToolResult::new(json!({"failure":error}), vec![ToolContent::Text { text: format!("Host Profile operation rejected: {error:?}") }], true),
            Err(_) => ToolResult::new(json!({"outcome":"unknown"}), vec![ToolContent::Text { text: "Host Profile outcome unavailable. Query the original ticket; do not create a replacement write.".into() }], true),
        }
    }
}
