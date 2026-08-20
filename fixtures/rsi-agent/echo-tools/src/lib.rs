#[path = "../../observer_support.rs"]
mod observer_support;
#[path = "../../provider_support.rs"]
mod provider_support;

use std::collections::BTreeSet;

use provider_support::{ProviderIo, ProviderIoError};
use rsi_agent_protocol::{
    TOOLS_SERVICE_KEY, ToolDefinition, ToolResult, ToolsBody, ToolsCatalogResponse, ToolsEnvelope,
    ToolsInvokeRequest, ToolsInvokeResponse, WireError, canonicalize_json,
};
use rsi_meta_plugin::Lane;
use rsi_meta_plugin::sdk::{Host, Plugin};
use serde_json::{Value, json};

const OBSERVER_SERVICE: &str = "fixture.rsi-agent.tools-observer";

struct EchoTools {
    io: ProviderIo,
    cataloged_streams: BTreeSet<String>,
    invoke_count: u8,
}

impl EchoTools {
    fn respond(&mut self, stream_id: &str, payload: &[u8]) -> Result<(), ProviderIoError> {
        let envelope = ToolsEnvelope::decode(payload)
            .map_err(|_| ProviderIoError::protocol("invalid tools envelope"))?;
        let request_id = envelope.request_id.clone();
        let response = match envelope.body {
            ToolsBody::CatalogRequest {} if self.cataloged_streams.insert(stream_id.to_owned()) => {
                ToolsEnvelope::catalog_response(
                    request_id,
                    ToolsCatalogResponse {
                        tools: vec![echo_definition()],
                    },
                )
            }
            ToolsBody::CatalogRequest {} => ToolsEnvelope::error(
                request_id,
                WireError {
                    code: "unexpected_extra_catalog".to_owned(),
                    message: "echo fixture permits exactly one catalog request".to_owned(),
                },
            ),
            ToolsBody::InvokeRequest(request)
                if self.cataloged_streams.contains(stream_id) && self.invoke_count == 0 =>
            {
                self.invoke_count += 1;
                let call_id = request.call_id.clone();
                ToolsEnvelope::invoke_response(
                    request_id,
                    ToolsInvokeResponse {
                        call_id,
                        result: invoke_echo(&request),
                    },
                )
            }
            ToolsBody::InvokeRequest(_) if !self.cataloged_streams.contains(stream_id) => {
                ToolsEnvelope::error(
                    request_id,
                    WireError {
                        code: "catalog_required".to_owned(),
                        message: "echo catalog must be captured before invocation".to_owned(),
                    },
                )
            }
            ToolsBody::InvokeRequest(_) => ToolsEnvelope::error(
                request_id,
                WireError {
                    code: "unexpected_extra_invoke".to_owned(),
                    message: "echo fixture permits exactly one invocation".to_owned(),
                },
            ),
            _ => ToolsEnvelope::error(
                request_id,
                WireError {
                    code: "unexpected_kind".to_owned(),
                    message: "echo fixture accepts only catalog_request and invoke_request"
                        .to_owned(),
                },
            ),
        };
        let bytes = response
            .encode()
            .map_err(|_| ProviderIoError::protocol("encode tools response"))?;
        self.io.send_data(stream_id, bytes)
    }

    fn observe(&mut self, stream_id: &str, payload: &[u8]) -> Result<(), ProviderIoError> {
        if payload != observer_support::QUERY {
            return Err(ProviderIoError::protocol("invalid observer query"));
        }
        let observation = self.io.observation();
        self.io.send_data(
            stream_id,
            observer_support::snapshot(
                observation.open_attempts,
                observation.accepted_opens,
                observation.data_frames,
                observation.max_concurrent_streams,
            ),
        )
    }
}

fn echo_definition() -> ToolDefinition {
    ToolDefinition {
        name: "echo".to_owned(),
        description: "Return the supplied text without side effects.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["text"],
            "properties": {"text": {"type": "string"}},
            "additionalProperties": false,
        }),
    }
}

fn invoke_echo(request: &ToolsInvokeRequest) -> ToolResult {
    if request.name != "echo" {
        return ToolResult::Error {
            code: "unknown_tool".to_owned(),
            message: format!("tool {:?} is not available", request.name),
        };
    }
    let Ok(arguments) = serde_json::from_str::<Value>(&request.arguments) else {
        return ToolResult::Error {
            code: "invalid_arguments".to_owned(),
            message: "echo arguments must be valid JSON".to_owned(),
        };
    };
    let Some(object) = arguments.as_object() else {
        return ToolResult::Error {
            code: "invalid_arguments".to_owned(),
            message: "echo arguments must be an object".to_owned(),
        };
    };
    if object.len() != 1 || !object.contains_key("text") {
        return ToolResult::Error {
            code: "invalid_arguments".to_owned(),
            message: "echo arguments must contain only text".to_owned(),
        };
    }
    let Some(text) = object.get("text").and_then(Value::as_str) else {
        return ToolResult::Error {
            code: "invalid_arguments".to_owned(),
            message: "echo text must be a string".to_owned(),
        };
    };
    let value = canonicalize_json(&json!({"text": text}))
        .expect("bounded fixture text always canonicalizes");
    ToolResult::Ok { value }
}

impl Plugin for EchoTools {
    type Error = ProviderIoError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            io: ProviderIo::new(host, TOOLS_SERVICE_KEY, OBSERVER_SERVICE),
            cataloged_streams: BTreeSet::new(),
            invoke_count: 0,
        })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        if let Some(inbound) = self.io.receive(lane, payload)? {
            if inbound.service == OBSERVER_SERVICE {
                self.observe(&inbound.stream_id, &inbound.payload)?;
            } else {
                self.respond(&inbound.stream_id, &inbound.payload)?;
            }
        }
        Ok(())
    }
}

rsi_meta_plugin::export_plugin!(EchoTools);
