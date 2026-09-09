use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Capability, CapabilityCall, ConfigValue, ContractVersion, Message, MetaError,
    PluginFactory, PreparedActivation, Requirement,
};
use rsi_tools_protocol::portable::{self, OsValue, ProcessPlan, Request, Response, Scheduling};
use rsi_tools_protocol::{
    MAXIMUM_REGISTERED_TOOLS, MAXIMUM_TOOL_IDENTIFIER_BYTES, MAXIMUM_TOOL_TIMEOUT_MS, ToolCall,
    ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract, ToolRegistration, ToolResult,
    ToolScheduling, ToolTimeoutPolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    service: String,
}

/// Ordinary contributor importing one explicit Portable supply into a Local Tool stage.
#[derive(Clone, Debug, Default)]
pub struct PortableToolsFactory;

#[async_trait]
impl PluginFactory for PortableToolsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(desired.clone())
            .map_err(|_| MetaError::InvalidInput("invalid Portable Tools configuration".into()))?;
        if config.service.is_empty() || config.service.len() > MAXIMUM_TOOL_IDENTIFIER_BYTES {
            return Err(MetaError::InvalidInput(
                "invalid Portable Tools service key".into(),
            ));
        }
        let requirement = Requirement::new(
            config.service.clone(),
            portable::CONTRACT,
            ContractVersion(portable::VERSION),
        );
        let bytes = std::mem::size_of::<Config>() + config.service.capacity();
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring(requirement)
                .requiring_local::<ToolRegistrarContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let capability = plan
            .inject(&config.service)
            .ok_or_else(|| MetaError::ServiceUnavailable {
                service: config.service.into(),
            })?
            .clone();
        let message = capability
            .invoke(Message::new(
                portable::encode(&Request::Describe {}).map_err(activation_error)?,
            ))
            .await?;
        if !message.capabilities().is_empty() {
            return Err(activation_error(protocol_error()));
        }
        let Response::Description { tools } =
            portable::decode(message.as_bytes()).map_err(activation_error)?
        else {
            return Err(activation_error(protocol_error()));
        };
        if tools.is_empty() || tools.len() > MAXIMUM_REGISTERED_TOOLS {
            return Err(activation_error(protocol_error()));
        }
        let mut registrations = Vec::with_capacity(tools.len());
        for tool in tools {
            if tool.timeout_ms == 0 || tool.timeout_ms > MAXIMUM_TOOL_TIMEOUT_MS {
                return Err(activation_error(protocol_error()));
            }
            let scheduling = match tool.scheduling {
                Scheduling::Exclusive => ToolScheduling::Exclusive,
                Scheduling::ExclusiveFinal => ToolScheduling::ExclusiveFinal,
                Scheduling::ParallelSafe => ToolScheduling::ParallelSafe,
            };
            let name = tool.definition.name().to_owned();
            registrations.push(ToolRegistration {
                definition: tool.definition.with_scheduling(scheduling),
                timeout: ToolTimeoutPolicy::Execution {
                    timeout_ms: tool.timeout_ms,
                },
                executor: Arc::new(Executor {
                    name,
                    capability: capability.clone(),
                }),
            });
        }
        let lease = plan
            .local::<ToolRegistrarContract>()?
            .register_batch(registrations)
            .map_err(activation_error)?;
        plan.defer(
            "withdraw Portable Tool batch",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

fn activation_error(_: ToolError) -> MetaError {
    MetaError::Activation("Portable Tool description or registration failed".into())
}
fn protocol_error() -> ToolError {
    ToolError::Execution("invalid Portable Tool exchange".into())
}
fn call_error(error: &MetaError) -> ToolError {
    if matches!(error, MetaError::Cancelled) {
        ToolError::Cancelled
    } else {
        ToolError::Execution("Portable Tool call failed; external effects may be unresolved".into())
    }
}

#[derive(Debug)]
struct Executor {
    name: String,
    capability: Capability,
}
#[async_trait]
impl ToolExecutor for Executor {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        if execution.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let request = portable::encode(&Request::Execute {
            call: ToolCall {
                id: execution.call_id.clone(),
                name: self.name.clone(),
                arguments,
            },
            policy: execution.policy().clone(),
        })?;
        let mut call = self.capability.open().map_err(|error| call_error(&error))?;
        let result = exchange(&mut call, request, &execution).await;
        if result.is_err() {
            call.cancel();
            // Observe the driver's terminal before relinquishing Tool body ownership.
            while matches!(call.recv().await, Ok(Some(_))) {}
        }
        result
    }
}

async fn exchange(
    call: &mut CapabilityCall,
    request: Vec<u8>,
    execution: &ToolExecution,
) -> rsi_tools_protocol::Result<ToolResult> {
    call.send(Message::new(request))
        .await
        .map_err(|error| call_error(&error))?;
    let mut result = None;
    let mut confine_requests = 0;
    while let Some(message) = receive(call, &execution.cancellation).await? {
        if result.is_some() || !message.capabilities().is_empty() {
            return Err(protocol_error());
        }
        match portable::decode(message.as_bytes())? {
            Response::Result { result: value } => {
                if !value.enforcement.is_empty() {
                    return Err(protocol_error());
                }
                result = Some(value);
                call.finish();
            }
            Response::Confine { program, arguments } => {
                if confine_requests >= portable::MAXIMUM_CONFINE_REQUESTS {
                    return Err(protocol_error());
                }
                confine_requests += 1;
                if execution.cancellation.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                let confined = execution.confine(program, arguments).await?;
                let plan = ProcessPlan {
                    program: OsValue::capture(confined.program.as_os_str())?,
                    arguments: confined
                        .arguments
                        .iter()
                        .map(|value| OsValue::capture(value))
                        .collect::<rsi_tools_protocol::Result<_>>()?,
                    cwd: OsValue::capture(confined.cwd.as_os_str())?,
                };
                call.send(Message::new(portable::encode(&Request::Confined { plan })?))
                    .await
                    .map_err(|error| call_error(&error))?;
            }
            Response::Description { .. } => return Err(protocol_error()),
        }
    }
    result.ok_or_else(protocol_error)
}

async fn receive(
    call: &mut CapabilityCall,
    cancellation: &CancellationToken,
) -> rsi_tools_protocol::Result<Option<Message>> {
    tokio::select! {
        biased;
        message = call.recv() => message.map_err(|error| call_error(&error)),
        () = cancellation.cancelled() => {
            call.cancel();
            call.recv().await.map_err(|error| call_error(&error))
        }
    }
}
