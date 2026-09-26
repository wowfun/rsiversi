use crate::{ProgramRpc, ProgramRuntime, ProgramRuntimeContract};
use async_trait::async_trait;
use rsi_agent_turn_protocol::ProgramToolCalls;
use rsi_jobs::{Jobs, JobsContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolProgramRole, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolTimeoutPolicy,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Adds the explicitly enabled foreground program coordinator to an Agent catalog.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProgramToolsFactory;
#[async_trait]
impl PluginFactory for ProgramToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "program Tool configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null)
            .requiring_local::<ProgramRuntimeContract>()
            .requiring_local::<JobsContract>()
            .requiring_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let definition = ToolDefinition::new("run_code", "Execute an explicit JavaScript async function body. Use await tools.call(name, arguments) for eligible tools; tools.definitions contains their frozen schemas. Return curated JSON (maximum 256 KiB). Native Node runs with shell-equivalent Sandbox authority. Maximum execution is 600 seconds.", json!({"type":"object","properties":{"script":{"type":"string","maxLength":rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES}},"required":["script"],"additionalProperties":false})).map_err(meta)?.with_program_role(ToolProgramRole::Coordinator);
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register(ToolRegistration {
                definition,
                output: None,
                timeout: ToolTimeoutPolicy::Execution {
                    timeout_ms: 600_000,
                },
                executor: Arc::new(Tool {
                    runtime: plan.local::<ProgramRuntimeContract>()?,
                    jobs: plan.local::<JobsContract>()?,
                }),
            })
            .map_err(meta)?;
        let workflow = crate::workflow::register(&plan)?;
        let controls = crate::control::register(&plan)?;
        plan.defer(
            "withdraw program Tools",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    drop(workflow);
                    drop(controls);
                    Ok(())
                })
            }),
        )
    }
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[derive(Debug)]
struct Tool {
    runtime: Arc<ProgramRuntime>,
    jobs: Arc<dyn Jobs>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    script: String,
}
#[derive(Debug)]
struct ToolRpc(Arc<ProgramToolCalls>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    arguments: Value,
}
#[async_trait]
impl ProgramRpc for ToolRpc {
    fn definitions(&self) -> Value {
        serde_json::to_value(self.0.0.definitions()).expect("validated finite Tool definitions")
    }
    async fn call(
        &self,
        method: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        if method != "tool" {
            return Err("workflow methods are unavailable in run_code".into());
        }
        let call: Call = serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let result = self
            .0
            .0
            .call(call.name, call.arguments, cancellation)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(result).map_err(|error| error.to_string())
    }
}
#[async_trait]
impl ToolExecutor for Tool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: Arguments = serde_json::from_value(arguments)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        let calls = execution.extension::<ProgramToolCalls>().ok_or_else(|| {
            ToolError::Execution("program coordinator authority is absent".into())
        })?;
        let scope = execution
            .job_scope()
            .ok_or_else(|| ToolError::Execution("program Jobs authority is absent".into()))?;
        let mut program = self
            .runtime
            .prepare(
                arguments.script,
                &execution,
                scope,
                Arc::new(ToolRpc(calls)),
            )
            .await
            .map_err(ToolError::Execution)?;
        program.start().map_err(ToolError::Execution)?;
        let result = tokio::select! {
            biased;
            () = execution.cancellation.cancelled() => { program.cancel(); let _ = program.result().await; Err("program cancelled".into()) },
            () = tokio::time::sleep(Duration::from_mins(10)) => { program.cancel(); let _ = program.result().await; Err("program deadline exceeded".into()) },
            result = program.result() => result,
        };
        self.jobs
            .wait(scope, program.job_id(), 0, 0)
            .await
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        match result {
            Ok(value) => ToolResult::new(json!({"value": value}), vec![], false),
            Err(error) => ToolResult::new(json!({"error": error}), vec![], true),
        }
    }
}
