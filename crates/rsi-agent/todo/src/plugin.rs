use crate::{DOMAIN, TOOL, TodoList, VIEW};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionInput,
    ContributionKind, ContributionOutput, ContributionRegistrarContract, ContributionRegistration,
    ContributionResult, DomainDefinition, DomainForkPolicy, DomainHandle, DomainRegistrarContract,
    SessionProjection, SessionProjectionContext, ToolSettlement, ToolSettlementContext,
    ToolSettlementContributor,
};
use rsi_agent_session_protocol::{
    ContributionId, DomainIdentity, DomainStateView, ProjectionValue, SessionFactBody,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Agent-only domain, Tool, context and projection registration.
#[derive(Clone, Copy, Debug, Default)]
pub struct TodoFactory;
#[derive(Debug)]
struct Todo {
    state: DomainHandle<TodoList>,
}
#[derive(Debug)]
struct WriteTool;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    todos: TodoList,
}
fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
impl Todo {
    fn current<'a>(
        &self,
        domains: &'a [DomainStateView],
    ) -> ContributionResult<(&'a DomainStateView, TodoList)> {
        let view = domains
            .iter()
            .find(|view| view.snapshot.identity().id() == DOMAIN)
            .ok_or_else(|| invalid("Todo domain is absent"))?;
        Ok((view, self.state.decode(&view.snapshot).map_err(invalid)?))
    }
}
#[async_trait]
impl ToolExecutor for WriteTool {
    async fn execute(
        &self,
        arguments: serde_json::Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: Arguments = serde_json::from_value(arguments)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        ToolResult::new(
            serde_json::to_value(args).map_err(|e| ToolError::InvalidInput(e.to_string()))?,
            vec![],
            false,
        )
    }
}
impl ToolSettlementContributor for Todo {
    fn settle(&self, context: &ToolSettlementContext<'_>) -> ContributionResult<ToolSettlement> {
        let SessionFactBody::ToolIntent {
            name, arguments, ..
        } = context.intent.body()
        else {
            return Err(invalid("Todo settlement requires a ToolIntent"));
        };
        if name != TOOL || context.result.is_error {
            return Ok(ToolSettlement::default());
        }
        let args: Arguments = serde_json::from_value(arguments.clone()).map_err(invalid)?;
        let actual: Arguments =
            serde_json::from_value(context.result.value.clone()).map_err(invalid)?;
        if actual.todos != args.todos {
            return Err(invalid("Todo result disagrees with its exact ToolIntent"));
        }
        let (view, _) = self.current(context.domains)?;
        Ok(ToolSettlement {
            domains: vec![
                self.state
                    .propose(view.revision, &args.todos)
                    .map_err(invalid)?,
            ],
            conclusion: None,
        })
    }
}
#[async_trait]
impl SessionProjection for Todo {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, list) = self.current(context.domains())?;
        ProjectionValue::new(serde_json::to_value(list).map_err(invalid)?).map_err(invalid)
    }
}
#[async_trait]
impl ContextContributor for Todo {
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let (_, list) = self.current(&context.domains)?;
        if list.items().is_empty() {
            return Ok(ContributionOutput::default());
        }
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(format!(
                "Current tasks: {}",
                serde_json::to_string(&list).map_err(invalid)?
            ))],
            domains: vec![],
        })
    }
}
#[async_trait]
impl PluginFactory for TodoFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(activation("Todo configuration must be null"));
        }
        Ok(PreparedActivation::with_state(ConfigValue::Null, (), 0)
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ContributionRegistrarContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let definition = DomainDefinition::new(
            DomainIdentity::new(DOMAIN, 1).map_err(activation)?,
            &TodoList::default(),
            TodoList::validate,
        )
        .map_err(activation)?
        .with_fork_policy(DomainForkPolicy::ResetToInitial);
        let context = plan.context().registration_context()?;
        let (state, domain_lease) = definition
            .register(plan.local::<DomainRegistrarContract>()?.as_ref(), &context)
            .map_err(activation)?;
        let plugin = Arc::new(Todo { state });
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let mut leases = Vec::new();
        for (id, kind) in [
            (
                "rsi.todo.settle",
                ContributionKind::ToolSettlement(plugin.clone()),
            ),
            (
                "rsi.todo.context",
                ContributionKind::Context(plugin.clone()),
            ),
            (VIEW, ContributionKind::Projection(plugin)),
        ] {
            leases.push(
                registrar
                    .register(
                        &context,
                        ContributionRegistration::new(
                            ContributionId::new(id).map_err(activation)?,
                            20,
                            kind,
                        ),
                    )
                    .map_err(activation)?,
            );
        }
        let definition=ToolDefinition::new(TOOL,"Replace the complete current task list. Pass todos=[] to clear it. Several tasks may be in progress at once.",serde_json::json!({
            "type":"object","properties":{"todos":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"content":{"type":"string","description":"One nonblank line, at most 512 UTF-8 bytes; no control characters.","pattern":"\\S","minLength":1,"maxLength":512},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["content","status"],"additionalProperties":false}}},"required":["todos"],"additionalProperties":false
        })).map_err(activation)?.with_scheduling(ToolScheduling::Exclusive);
        let tool_lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                output: None,
                definition,
                executor: Arc::new(WriteTool),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1000 },
            }])
            .map_err(activation)?;
        plan.defer(
            "withdraw Todo",
            Box::new(move || {
                Box::pin(async move {
                    tool_lease.retire().map_err(|e| e.to_string())?;
                    drop(leases);
                    drop(domain_lease);
                    Ok(())
                })
            }),
        )
    }
}
