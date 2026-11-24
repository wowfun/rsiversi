use super::{
    Sessions,
    session_fragments::Identity,
    session_history::{fragment, resource_key},
};
use rsi_agent_session_protocol::{AgentPresetId, SessionId};
use rsi_api_protocol::{ApiError, ApiOutput, CallOrigin, RetainedBytes};
use rsi_configuration_api::agent_presets::AgentPresetsOperation;
use rsi_extension::{Error, Result};
use rsi_session_protocol::{SelectDraftPreset, SessionError};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request: String,
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List {
        reader: String,
        offset: usize,
    },
    Resolve {
        id: Option<String>,
    },
    Document {
        id: String,
        reader: String,
        offset: usize,
    },
    Select {
        id: AgentPresetId,
        session_id: SessionId,
    },
    PageClose {
        reader: String,
    },
}

impl Sessions {
    pub(super) async fn presets(&self, payload: Value) -> Result<Value> {
        let input: Request = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let origin = self
            .routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let (id, reader, offset) = match input.operation {
            Operation::List { reader, offset } => (None, reader, offset),
            Operation::Resolve { id } => {
                let id = match id.map(AgentPresetId::new).transpose() {
                    Ok(id) => id,
                    Err(_) => return Ok(Value::Null),
                };
                let bytes = self
                    .preset_reply(AgentPresetsOperation::Resolve, origin, json!({"id":id}))
                    .await?;
                return serde_json::from_slice(bytes.as_bytes()).map_err(|_| Error::Protocol);
            }
            Operation::Document { id, reader, offset } => match AgentPresetId::new(id) {
                Ok(id) => (Some(id), reader, offset),
                Err(_) => return fragment(r#"{"code":"not_found"}"#, offset, 0),
            },
            Operation::Select { id, session_id } => {
                let handle = self.interactive_handle(&input.request, &session_id).await?;
                let draft = match handle.draft_snapshot().await {
                    Ok(draft) => draft,
                    Err(SessionError::NotFound(_)) => return Ok(json!({"code":"locked"})),
                    Err(error) => return Err(Error::Remote(error.to_string())),
                };
                let changed = draft.header.agent_preset_id() != &id;
                if changed {
                    match handle
                        .select_preset(SelectDraftPreset {
                            preset_id: id.clone(),
                            expected_revision: draft.revision,
                        })
                        .await
                    {
                        Ok(_) => {}
                        Err(SessionError::NotFound(_)) => return Ok(json!({"code":"locked"})),
                        Err(error) => return Err(Error::Remote(error.to_string())),
                    }
                }
                self.routes
                    .caller(&input.request)
                    .map_err(|_| Error::Cancelled)?;
                return Ok(json!({"selected":id,"changed":changed}));
            }
            Operation::PageClose { reader } => {
                self.routes
                    .release_resource(&input.request, &resource_key("presets", &reader)?);
                return Ok(Value::Null);
            }
        };
        self.history_pages
            .read(
                &self.routes,
                &input.request,
                &reader,
                offset,
                Identity::Presets(id.clone()),
                async {
                    let (operation, body) = match id {
                        Some(id) => (AgentPresetsOperation::Document, json!({"id":id})),
                        None => (AgentPresetsOperation::List, json!({})),
                    };
                    let result = self.preset_reply(operation, origin, body).await?;
                    let json =
                        std::str::from_utf8(result.as_bytes()).map_err(|_| Error::Protocol)?;
                    Ok((0, json.to_owned()))
                },
            )
            .await
    }

    async fn preset_reply(
        &self,
        operation: AgentPresetsOperation,
        origin: CallOrigin,
        body: Value,
    ) -> Result<RetainedBytes> {
        let spec = operation.spec();
        let invocation = self.dispatch.admit(&spec.call(), origin).map_err(remote)?;
        let bytes = self
            .dispatch
            .input_budget(spec.class)
            .encode(&body, spec.maximum_request_bytes)
            .map_err(remote)?;
        match invocation.invoke(bytes).await {
            Ok(ApiOutput::Reply(message)) if message.binary.is_none() => Ok(message.json),
            Err(ApiError::Domain(outcome)) => Ok(outcome),
            Err(error) => Err(remote(error)),
            Ok(_) => Err(Error::Protocol),
        }
    }
}

fn remote(error: ApiError) -> Error {
    Error::Remote(error.to_string())
}
