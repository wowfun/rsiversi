use super::Sessions;
use rsi_ai_protocol::ModelRef;
use rsi_extension::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request: String,
    after: Option<ModelRef>,
}
impl Sessions {
    pub(super) async fn select_model(&self, payload: Value) -> Result<Value> {
        use rsi_agent_session_protocol::{CommandArguments, SessionCommandInvocation};
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
            request_id: rsi_agent_session_protocol::DomainRequestId,
            selection: rsi_agent_session_protocol::ModelSelection,
        }
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        let catalog = handle
            .commands()
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
        let command = catalog
            .commands()
            .iter()
            .find(|command| command.id().as_str() == rsi_agent_model_selection::COMMAND)
            .ok_or_else(|| Error::Remote("Session model selection unavailable".into()))?;
        let arguments =
            CommandArguments::new(json!(input.selection)).map_err(|_| Error::Protocol)?;
        let receipt = handle
            .execute_command(SessionCommandInvocation {
                command: command.id().clone(),
                request_id: input.request_id,
                expected_revision: catalog.revision(),
                arguments,
            })
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
        Ok(json!({"selected":input.selection,"receipt":receipt}))
    }

    pub(super) async fn model_catalog(&self, payload: Value) -> Result<Value> {
        let input: Request = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Remote("request has retired".into()))?;
        let models = self
            .models
            .as_ref()
            .ok_or_else(|| Error::Remote("Language model catalog unavailable".into()))?;
        let defaults = self
            .defaults
            .as_ref()
            .ok_or_else(|| Error::Remote("Agent defaults unavailable".into()))?
            .current()
            .map_err(|error| Error::Remote(error.to_string()))?;
        let page = models
            .list_models(input.after.as_ref(), 16)
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
        let next = page.has_more.then(|| page.models.last().cloned()).flatten();
        let mut rows = Vec::with_capacity(page.models.len());
        for route in page.models {
            let entry = match models.describe_model(&route).await {
                Ok(description) => {
                    let efforts = description.profile().reasoning_efforts();
                    json!({"route":route,"efforts":efforts.supported(),"default_effort":efforts.default_effort()})
                }
                Err(error) => json!({"route":route,"failure":error.to_string()}),
            };
            rows.push(entry);
        }
        Ok(
            json!({"models":rows,"next":next,"default":defaults.default_model(),"effort":defaults.default_reasoning_effort()}),
        )
    }
}
