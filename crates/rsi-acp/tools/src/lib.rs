//! Model delegation over configured external endpoints and Host-owned observations.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::ConversationId,
    service::{Error, ExternalConversations, ExternalConversationsContract},
};
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolOutputDeclaration,
    ToolRegistrarContract, ToolRegistration, ToolResult, ToolTimeoutPolicy, TypedToolOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

/// Ordinary frozen Agent contribution. External lifecycle stays in the Host.
#[derive(Debug, Default)]
pub struct Factory;
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("external Agent Tool configuration must be null"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ExternalConversationsContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let output=TypedToolOutput::new(ToolOutputDeclaration::new("rsi.acp.external-agent",1,json!({"type":"object","required":["operation","conversation","data"],"properties":{"operation":{"type":"string"},"conversation":{"type":"string"},"data":{}},"additionalProperties":false})).map_err(meta)?,render);
        let definition=ToolDefinition::new(rsi_acp_protocol::EXTERNAL_AGENT_TOOL_NAME,"Delegate through an operator-configured external endpoint. start only creates a conversation; follow_up sends one prompt without retry. read returns local observations. Unknown does not mean completed; never resend an unknown prompt. Permission decisions require the human's external conversation view.",json!({"type":"object","properties":{"operation":{"type":"string","enum":["endpoints","start","follow_up","read","cancel","close"]},"endpoint":{"type":"string","maxLength":64},"conversation":{"type":"string","maxLength":128},"text":{"type":"string","maxLength":131_072},"after":{"type":"string","maxLength":19}},"required":["operation"],"additionalProperties":false})).map_err(meta)?;
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(vec![ToolRegistration {
                definition,
                output: Some(output.declaration().clone()),
                timeout: ToolTimeoutPolicy::Execution { timeout_ms: 60_000 },
                executor: Arc::new(Executor {
                    service: plan.local::<ExternalConversationsContract>()?,
                    output,
                }),
            }])
            .map_err(meta)?;
        plan.defer(
            "release external delegation Tool",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Endpoints,
    Start {
        endpoint: String,
    },
    FollowUp {
        conversation: ConversationId,
        text: String,
    },
    Read {
        conversation: ConversationId,
        #[serde(default)]
        after: Option<String>,
    },
    Cancel {
        conversation: ConversationId,
    },
    Close {
        conversation: ConversationId,
    },
}
#[derive(Serialize)]
struct Output {
    operation: &'static str,
    conversation: String,
    data: Value,
}
fn render(output: &Output) -> Vec<ToolContent> {
    vec![ToolContent::Text {
        text: serde_json::to_string(&output).expect("bounded serializable output"),
    }]
}
struct Executor {
    service: Arc<dyn ExternalConversations>,
    output: TypedToolOutput<Output>,
}
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalDelegationExecutor")
            .finish_non_exhaustive()
    }
}
fn encode(value: &impl Serialize) -> rsi_acp_protocol::service::Result<Value> {
    serde_json::to_value(value).map_err(|_| Error::Input)
}
fn prefix(caller: &AgentCallerAuthority) -> String {
    format!(
        "delegated_{}_",
        hex::encode(Sha256::digest(caller.session_id().as_str().as_bytes()))
    )
}
fn target(id: &ConversationId, prefix: &str) -> rsi_acp_protocol::service::Result<()> {
    if id.as_str().starts_with(prefix) {
        Ok(())
    } else {
        Err(Error::Stale)
    }
}
impl Executor {
    async fn run(
        &self,
        input: Input,
        execution: &ToolExecution,
        caller: &AgentCallerAuthority,
    ) -> rsi_acp_protocol::service::Result<Output> {
        let namespace = prefix(caller);
        let (operation, conversation, data) = match input {
            Input::Endpoints => (
                "endpoints",
                String::new(),
                encode(&self.service.endpoints().await?)?,
            ),
            Input::Start { endpoint } => {
                let id = ConversationId::new(format!(
                    "{namespace}{}",
                    &hex::encode(Sha256::digest(execution.call_id.as_bytes()))[..32]
                ))
                .map_err(|_| Error::Input)?;
                let snapshot = self.service.start(id.clone(), &endpoint).await?;
                ("start", id.as_str().into(), encode(&snapshot)?)
            }
            Input::FollowUp { conversation, text } => {
                target(&conversation, &namespace)?;
                (
                    "follow_up",
                    conversation.as_str().into(),
                    encode(&self.service.submit(&conversation, &text).await?)?,
                )
            }
            Input::Read {
                conversation,
                after,
            } => {
                target(&conversation, &namespace)?;
                let after = if let Some(after) = after {
                    let value = after.parse::<u64>().map_err(|_| Error::Input)?;
                    if value.to_string() != after {
                        return Err(Error::Input);
                    }
                    value
                } else {
                    0
                };
                let view = self.service.view(&conversation).await?;
                let history = self
                    .service
                    .page(&conversation, view.snapshot.epoch, after)
                    .await?;
                (
                    "read",
                    conversation.as_str().into(),
                    json!({"snapshot":view.snapshot,"connected":view.connected,"pending_permissions":view.permissions.len(),"history":history}),
                )
            }
            Input::Cancel { conversation } => {
                target(&conversation, &namespace)?;
                (
                    "cancel",
                    conversation.as_str().into(),
                    encode(&self.service.cancel(&conversation).await?)?,
                )
            }
            Input::Close { conversation } => {
                target(&conversation, &namespace)?;
                (
                    "close",
                    conversation.as_str().into(),
                    encode(&self.service.close(&conversation).await?)?,
                )
            }
        };
        Ok(Output {
            operation,
            conversation,
            data,
        })
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
                ToolError::InvalidInput("external delegation requires a live native caller".into())
            })?;
        let input = serde_json::from_value(arguments)
            .map_err(|_| ToolError::InvalidInput("invalid external delegation input".into()))?;
        let result = tokio::select! {biased;()=execution.cancellation.cancelled()=>return Err(ToolError::Cancelled),result=self.run(input,&execution,caller.as_ref())=>result};
        match result {
            Ok(value) => self.output.result(&value),
            Err(error) => ToolResult::new(
                json!({"error":error}),
                vec![ToolContent::Text {
                    text: error.to_string(),
                }],
                true,
            ),
        }
    }
}
