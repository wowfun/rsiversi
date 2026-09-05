//! Thin model-facing adapters over the durable Agent control plane.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    ForkTurnSelection, MAXIMUM_AGENT_IDENTIFIER_BYTES, MessageId, SessionId,
};
use rsi_agent_turn_protocol::{
    AgentCallerAuthority, AgentListScope, AgentNodeState, AgentWaitResult, MessageState,
    SendAgentMessage, SpawnAgentRequest, TurnError, TurnService, TurnServiceContract,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolExecution, ToolExecutor, ToolLaneParkingAuthority,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolScheduling,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_WAIT_MS: u64 = 30_000;
const MAXIMUM_WAIT_MS: u64 = 570_000;

/// Ordinary contribution factory for the six native Agent control Tools.
#[derive(Clone, Debug, Default)]
pub struct AgentToolsFactory;

#[async_trait]
impl PluginFactory for AgentToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Agent Tools configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(Value::Null, (), 0)
            .requiring_local::<ToolRegistrarContract>()
            .requiring_local::<TurnServiceContract>())
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let registrar = plan.local::<ToolRegistrarContract>()?;
        let turns = plan.local::<TurnServiceContract>()?;
        let registrations =
            registrations(&turns).map_err(|error| MetaError::Activation(error.to_string()))?;
        let lease = registrar
            .register_batch(registrations)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release Agent control Tool contribution lease",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum NativeTool {
    Spawn,
    Send,
    Followup,
    Wait,
    Interrupt,
    List,
}

#[derive(Debug)]
struct NativeExecutor {
    kind: NativeTool,
    turns: Arc<dyn TurnService>,
}

fn registrations(
    turns: &Arc<dyn TurnService>,
) -> rsi_tools_protocol::Result<Vec<ToolRegistration>> {
    let specs = [
        (
            NativeTool::Spawn,
            "spawn_agent",
            "Create a durable continuable child with the parent's balanced completed-turn context. Omit fork_turns to inherit all completed turns.",
            json!({
                "type":"object",
                "properties":{
                    "task_name":{"type":"string","minLength":1,"maxLength":MAXIMUM_AGENT_IDENTIFIER_BYTES},
                    "message":{"type":"string","minLength":1},
                    "fork_turns":{"type":"string","minLength":1,"maxLength":20}
                },
                "required":["task_name","message"],
                "additionalProperties":false
            }),
            60_000,
        ),
        (
            NativeTool::Send,
            "send_message",
            "Durably send input to an adjacent Agent. It steers a running target at its next Step, but does not wake an idle target.",
            message_schema(),
            30_000,
        ),
        (
            NativeTool::Followup,
            "followup_task",
            "Durably queue a waking next Turn for an adjacent Agent, even when its current Turn is still running.",
            message_schema(),
            30_000,
        ),
        (
            NativeTool::Wait,
            "wait_agent",
            "Wait for a descendant state change after this call begins. Returns immediately when no descendant can make progress.",
            json!({
                "type":"object",
                "properties":{"timeout_ms":{"type":"integer","minimum":1,"maximum":MAXIMUM_WAIT_MS}},
                "additionalProperties":false
            }),
            600_000,
        ),
        (
            NativeTool::Interrupt,
            "interrupt_agent",
            "Request cancellation of one descendant's current Turn only. Its inbox, descendants, and future continuability remain intact.",
            target_schema(),
            30_000,
        ),
        (
            NativeTool::List,
            "list_agents",
            "List durable continuable children with running, ready, or idle state. Use scope=descendants for stable pre-order traversal.",
            json!({
                "type":"object",
                "properties":{"scope":{"type":"string","enum":["children","descendants"]}},
                "additionalProperties":false
            }),
            30_000,
        ),
    ];
    specs
        .into_iter()
        .map(|(kind, name, description, parameters, timeout_ms)| {
            let scheduling = if matches!(kind, NativeTool::Wait) {
                ToolScheduling::ExclusiveFinal
            } else {
                ToolScheduling::Exclusive
            };
            Ok(ToolRegistration {
                definition: ToolDefinition::new(name, description, parameters)?
                    .with_scheduling(scheduling),
                timeout_ms,
                executor: Arc::new(NativeExecutor {
                    kind,
                    turns: Arc::clone(turns),
                }),
            })
        })
        .collect()
}

fn message_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "target":{"type":"string","minLength":1,"maxLength":MAXIMUM_AGENT_IDENTIFIER_BYTES},
            "message":{"type":"string","minLength":1}
        },
        "required":["target","message"],
        "additionalProperties":false
    })
}

fn target_schema() -> Value {
    json!({
        "type":"object",
        "properties":{"target":{"type":"string","minLength":1,"maxLength":MAXIMUM_AGENT_IDENTIFIER_BYTES}},
        "required":["target"],
        "additionalProperties":false
    })
}

impl NativeExecutor {
    async fn spawn(
        &self,
        arguments: Value,
        execution: &ToolExecution,
        caller: &AgentCallerAuthority,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: SpawnArguments = parse(arguments)?;
        let fork_turns = arguments
            .fork_turns
            .as_deref()
            .map(ForkTurnSelection::parse)
            .transpose()
            .map_err(|error| rsi_tools_protocol::ToolError::InvalidInput(error.to_string()))?
            .unwrap_or(ForkTurnSelection::All);
        match self
            .turns
            .spawn_agent(SpawnAgentRequest {
                caller: caller.clone(),
                child_session_id: deterministic_session(
                    "agent",
                    caller.session_id().as_str(),
                    caller.turn_id().as_str(),
                    &execution.call_id,
                ),
                task_name: arguments.task_name,
                message_id: deterministic_message(
                    "spawn-message",
                    caller.session_id().as_str(),
                    caller.turn_id().as_str(),
                    &execution.call_id,
                ),
                message: arguments.message,
                fork_turns,
            })
            .await
        {
            Ok(child) => tool_ok(
                json!({
                    "session_id":child.session_id.as_str(),
                    "message_id":child.message.message_id.as_str(),
                    "path":child.path.segments(),
                }),
                format!("started subagent {}", child.session_id.as_str()),
            ),
            Err(error) => tool_error("spawn_failed", error.to_string()),
        }
    }

    async fn send(
        &self,
        arguments: Value,
        execution: &ToolExecution,
        caller: &AgentCallerAuthority,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: MessageArguments = parse(arguments)?;
        let target = match SessionId::new(arguments.target) {
            Ok(target) => target,
            Err(error) => return tool_error("invalid_target", error.to_string()),
        };
        match self
            .turns
            .send_agent_message(SendAgentMessage {
                caller: caller.clone(),
                target_session_id: target,
                message_id: deterministic_message(
                    "agent-message",
                    caller.session_id().as_str(),
                    caller.turn_id().as_str(),
                    &execution.call_id,
                ),
                message: arguments.message,
                start_new_turn: matches!(self.kind, NativeTool::Followup),
            })
            .await
        {
            Ok(receipt) => tool_ok(
                json!({
                    "message_id":receipt.message_id.as_str(),
                    "state":message_state_name(&receipt.state),
                }),
                format!("message {} durably accepted", receipt.message_id.as_str()),
            ),
            Err(error) => tool_error("send_failed", error.to_string()),
        }
    }

    async fn wait(
        &self,
        arguments: Value,
        execution: &ToolExecution,
        caller: &AgentCallerAuthority,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: WaitArguments = parse(arguments)?;
        let timeout_ms = match validate_wait_timeout(arguments.timeout_ms) {
            Ok(timeout_ms) => timeout_ms,
            Err(message) => {
                return tool_error("invalid_timeout", message);
            }
        };
        let Some(parking) = execution.extension::<ToolLaneParkingAuthority>() else {
            return tool_error(
                "missing_lane_authority",
                "wait_agent requires executor lane-parking authority",
            );
        };
        let parked = match parking.park().await {
            Ok(parked) => parked,
            Err(error) => return tool_error("park_failed", error.to_string()),
        };
        let result = self
            .turns
            .wait_agent(
                caller,
                Duration::from_millis(timeout_ms),
                execution.cancellation.clone(),
            )
            .await;
        parked.resume(execution.cancellation.clone()).await?;
        map_wait_result(result)
    }

    async fn interrupt(
        &self,
        arguments: Value,
        caller: &AgentCallerAuthority,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: TargetArguments = parse(arguments)?;
        let target = match SessionId::new(arguments.target) {
            Ok(target) => target,
            Err(error) => return tool_error("invalid_target", error.to_string()),
        };
        match self.turns.interrupt_agent(caller, &target).await {
            Ok(result) => tool_ok(
                json!({
                    "accepted":result.accepted,
                    "already_terminal":result.already_terminal,
                }),
                format!("interrupt requested for agent {}", target.as_str()),
            ),
            Err(error) => tool_error("interrupt_failed", error.to_string()),
        }
    }

    async fn list(
        &self,
        arguments: Value,
        caller: &AgentCallerAuthority,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: ListArguments = parse(arguments)?;
        let scope = match arguments.scope.as_deref().unwrap_or("children") {
            "children" => AgentListScope::Children,
            "descendants" => AgentListScope::Descendants,
            other => return tool_error("invalid_scope", format!("unknown scope `{other}`")),
        };
        match self.turns.list_agents(caller, scope).await {
            Ok(nodes) => {
                let value = nodes
                    .iter()
                    .map(|node| {
                        json!({
                            "session_id":node.session_id.as_str(),
                            "parent_session_id":node.parent_session_id.as_str(),
                            "task_name":node.task_name,
                            "path":node.path.segments(),
                            "state":agent_node_state_name(node.state),
                        })
                    })
                    .collect::<Vec<_>>();
                tool_ok(
                    json!({"agents":value}),
                    serde_json::to_string_pretty(&value)
                        .unwrap_or_else(|_| "Agent roster serialization failed".into()),
                )
            }
            Err(error) => tool_error("list_failed", error.to_string()),
        }
    }
}

fn validate_wait_timeout(timeout_ms: Option<u64>) -> Result<u64, String> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_MS);
    if timeout_ms == 0 || timeout_ms > MAXIMUM_WAIT_MS {
        return Err(format!("timeout_ms must be within 1..={MAXIMUM_WAIT_MS}"));
    }
    Ok(timeout_ms)
}

fn map_wait_result(
    result: rsi_agent_turn_protocol::Result<AgentWaitResult>,
) -> rsi_tools_protocol::Result<ToolResult> {
    match result {
        Ok(result) => {
            let status = match result {
                AgentWaitResult::Changed => "changed",
                AgentWaitResult::TimedOut => "timed_out",
                AgentWaitResult::NoProgress => "no_progress",
            };
            tool_ok(json!({"status":status}), status.replace('_', " "))
        }
        Err(TurnError::Cancelled) => Err(rsi_tools_protocol::ToolError::Cancelled),
        Err(error) => tool_error("wait_failed", error.to_string()),
    }
}

const fn message_state_name(state: &MessageState) -> &'static str {
    match state {
        MessageState::Pending => "pending",
        MessageState::Claimed { .. } => "claimed",
        MessageState::Discarded { .. } => "discarded",
    }
}

const fn agent_node_state_name(state: AgentNodeState) -> &'static str {
    match state {
        AgentNodeState::Running => "running",
        AgentNodeState::Ready => "ready",
        AgentNodeState::Idle => "idle",
    }
}

#[async_trait]
impl ToolExecutor for NativeExecutor {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let Some(caller) = execution.extension::<AgentCallerAuthority>() else {
            return tool_error(
                "missing_agent_caller",
                "Agent control Tool requires a live Agent caller",
            );
        };
        match self.kind {
            NativeTool::Spawn => self.spawn(arguments, &execution, caller.as_ref()).await,
            NativeTool::Send | NativeTool::Followup => {
                self.send(arguments, &execution, caller.as_ref()).await
            }
            NativeTool::Wait => self.wait(arguments, &execution, caller.as_ref()).await,
            NativeTool::Interrupt => self.interrupt(arguments, caller.as_ref()).await,
            NativeTool::List => self.list(arguments, caller.as_ref()).await,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArguments {
    task_name: String,
    message: String,
    fork_turns: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArguments {
    target: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArguments {
    target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArguments {
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    scope: Option<String>,
}

fn parse<T: DeserializeOwned>(value: Value) -> rsi_tools_protocol::Result<T> {
    serde_json::from_value(value)
        .map_err(|error| rsi_tools_protocol::ToolError::InvalidInput(error.to_string()))
}

fn deterministic_suffix(
    domain: &str,
    caller_session_id: &str,
    caller_turn_id: &str,
    call_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"rsi-agent-tool-id-v2\0");
    for field in [domain, caller_session_id, caller_turn_id, call_id] {
        digest.update(field.len().to_le_bytes());
        digest.update(field.as_bytes());
    }
    let encoded = format!("{:x}", digest.finalize());
    encoded[..24].to_owned()
}

fn deterministic_session(
    domain: &str,
    caller_session_id: &str,
    caller_turn_id: &str,
    call_id: &str,
) -> SessionId {
    SessionId::new(format!(
        "agent-{}",
        deterministic_suffix(domain, caller_session_id, caller_turn_id, call_id)
    ))
    .expect("SHA-derived Agent session identity is valid")
}

fn deterministic_message(
    domain: &str,
    caller_session_id: &str,
    caller_turn_id: &str,
    call_id: &str,
) -> MessageId {
    MessageId::new(format!(
        "message-{}",
        deterministic_suffix(domain, caller_session_id, caller_turn_id, call_id)
    ))
    .expect("SHA-derived Agent message identity is valid")
}

fn tool_ok(value: Value, text: String) -> rsi_tools_protocol::Result<ToolResult> {
    ToolResult::new(value, vec![ToolContent::Text { text }], false)
}

fn tool_error(code: &str, message: impl Into<String>) -> rsi_tools_protocol::Result<ToolResult> {
    let message = message.into();
    ToolResult::new(
        json!({"error":{"code":code,"message":message}}),
        vec![ToolContent::Text {
            text: format!("Error: {message}"),
        }],
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_identities_are_scoped_to_the_exact_caller_turn() {
        let first = deterministic_session("agent", "session-a", "turn-a", "call-1");
        assert_eq!(
            first,
            deterministic_session("agent", "session-a", "turn-a", "call-1")
        );
        assert_ne!(
            first,
            deterministic_session("agent", "session-b", "turn-a", "call-1")
        );
        assert_ne!(
            first,
            deterministic_session("agent", "session-a", "turn-b", "call-1")
        );
        assert_ne!(
            deterministic_message("agent-message", "session-a", "turn-a", "call-1"),
            deterministic_message("agent-message", "session-b", "turn-a", "call-1")
        );
    }

    #[test]
    fn model_schemas_accept_every_protocol_valid_agent_identifier() {
        let maximum = rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES;
        assert_eq!(
            message_schema()["properties"]["target"]["maxLength"],
            maximum
        );
        assert_eq!(
            target_schema()["properties"]["target"]["maxLength"],
            maximum
        );
    }

    #[test]
    fn wait_timeout_and_fork_selection_reject_invalid_model_values() {
        assert_eq!(validate_wait_timeout(None).unwrap(), DEFAULT_WAIT_MS);
        assert_eq!(
            validate_wait_timeout(Some(MAXIMUM_WAIT_MS)).unwrap(),
            MAXIMUM_WAIT_MS
        );
        assert!(validate_wait_timeout(Some(0)).is_err());
        assert!(validate_wait_timeout(Some(MAXIMUM_WAIT_MS + 1)).is_err());
        assert_eq!(
            ForkTurnSelection::parse("7").unwrap(),
            ForkTurnSelection::Last(7)
        );
        assert!(ForkTurnSelection::parse("0").is_err());
        assert!(SessionId::new("contains whitespace").is_err());
    }

    #[test]
    fn kernel_wait_cancellation_stays_a_tool_cancellation() {
        assert_eq!(
            map_wait_result(Err(TurnError::Cancelled)).unwrap_err(),
            rsi_tools_protocol::ToolError::Cancelled
        );
    }
}
