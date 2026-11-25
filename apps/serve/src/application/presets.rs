use super::{
    BoundSessions,
    session_fragments::Identity,
    session_history::{fragment, resource_key},
};
use rsi_agent_session_protocol::{AgentPresetId, SessionId};
use rsi_api_protocol::{ApiError, ApiOutput, CallOrigin, RetainedBytes};
use rsi_configuration_api::agent_presets::AgentPresetsOperation;
use rsi_extension::{Error, Result};
use rsi_session_protocol::SessionError;
use rsi_settings_protocol::{SettingsError, SettingsVersion};
use serde::Deserialize;
use serde_json::{Value, json};

/// A replacement process announces its own connection before it activates plugins.
#[derive(Debug)]
pub(super) struct Source(
    pub(super) tokio::sync::watch::Receiver<std::sync::Weak<rsi_extension::Connection>>,
);

#[async_trait::async_trait]
impl rsi_agent_composition::ApplicationPresetSource for Source {
    async fn resolve(
        &self,
        id: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<Option<rsi_host::Profile>> {
        use rsi_agent_composition_protocol::AgentCompositionError;
        let unavailable = |reason: &str| AgentCompositionError::Unavailable {
            preset_id: id.clone(),
            reason: reason.into(),
        };
        let mut peers = self.0.clone();
        let connection = loop {
            if let Some(connection) = peers.borrow_and_update().upgrade()
                && !connection.disconnect_token().is_cancelled()
            {
                break connection;
            }
            peers
                .changed()
                .await
                .map_err(|_| unavailable("The application preset source has stopped"))?;
        };
        let reply = connection
            .request(
                "application.preset.resolve",
                json!({"id":id}),
                connection.disconnect_token(),
            )
            .await
            .map_err(|_| unavailable("The application preset source is unavailable"))?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Declaration {
            id: AgentPresetId,
            broken: Option<String>,
        }
        let Some(result): Option<Declaration> = serde_json::from_value(reply)
            .map_err(|_| unavailable("Invalid application preset reply"))?
        else {
            return Ok(None);
        };
        if result.id != *id {
            return Err(unavailable("The application returned a different preset"));
        }
        if let Some(reason) = result.broken {
            return Err(unavailable(&reason));
        }
        Ok(Some(rsi_host::Profile::new([
            rsi_host::ProfileEntry::new("context", "rsi.agent.context.default", Value::Null),
            rsi_host::ProfileEntry::new(
                "model-selection",
                "rsi.agent.model-selection",
                Value::Null,
            ),
            rsi_host::ProfileEntry::new("permission", "rsi.agent.permission", Value::Null),
            rsi_host::ProfileEntry::new("goal", "rsi.agent.goal", json!({"domain_only":true})),
        ])))
    }
}

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
    DefaultRead,
    DefaultWrite {
        id: Option<AgentPresetId>,
        expected: Option<SettingsVersion>,
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

impl BoundSessions {
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
            Operation::DefaultRead => return self.preset_default(origin, None).await,
            Operation::DefaultWrite { id, expected } => {
                return self.preset_default(origin, Some((id, expected))).await;
            }
            Operation::Document { id, reader, offset } => match AgentPresetId::new(id) {
                Ok(id) => (Some(id), reader, offset),
                Err(_) => return fragment(r#"{"code":"not_found"}"#, offset, 0),
            },
            Operation::Select { id, session_id } => {
                let handle = self.interactive_handle(&input.request, &session_id).await?;
                let changed = match handle.select_agent_preset(id.clone()).await {
                    Ok((_, changed)) => changed,
                    Err(SessionError::NotFound(_)) => return Ok(json!({"code":"locked"})),
                    Err(error) => return Err(Error::Remote(error.to_string())),
                };
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

    async fn preset_default(
        &self,
        origin: CallOrigin,
        write: Option<(Option<AgentPresetId>, Option<SettingsVersion>)>,
    ) -> Result<Value> {
        let grant = self
            .context
            .lookup_local::<rsi_configuration_access::ConfigurationAccessContract>()
            .ok_or_else(|| remote(ApiError::Unavailable))?;
        let _lease = grant.admit(&origin).map_err(remote)?;
        let defaults = self
            .context
            .lookup_local::<rsi::AgentPresetDefaultsContract>()
            .ok_or_else(|| remote(ApiError::Unavailable))?;
        let result = match write {
            Some((id, expected)) => defaults
                .set_choice(expected.as_ref(), id.as_ref())
                .await
                .map(|version| (version, id)),
            None => defaults.choice().await,
        };
        match result {
            Ok((version, selected)) => Ok(json!({
                "id": selected.as_ref().map_or(rsi::DEFAULT_AGENT_PRESET_ID,AgentPresetId::as_str),
                "selected": selected,
                "version": version,
            })),
            Err(SettingsError::Conflict { .. }) => Ok(json!({"code":"conflict"})),
            Err(SettingsError::StaleRegistration(_)) => Ok(json!({"code":"stale_registration"})),
            Err(error) => Err(Error::Remote(error.to_string())),
        }
    }
}

fn remote(error: ApiError) -> Error {
    Error::Remote(error.to_string())
}
