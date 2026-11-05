use super::Sessions;
use rsi_agent_session_protocol::{DomainRequestId, SessionCommandInvocation, SessionId};
use rsi_extension::{Error, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List,
    Execute {
        invocation: SessionCommandInvocation,
    },
    Status {
        request_id: DomainRequestId,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    session_id: SessionId,
    operation: Operation,
}
impl Sessions {
    pub(super) async fn command(&self, payload: Value) -> Result<Value> {
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        let reply = match input.operation {
            Operation::List => serde_json::to_value(handle.commands().await.map_err(remote)?),
            Operation::Execute { invocation } => {
                serde_json::to_value(handle.execute_command(invocation).await.map_err(remote)?)
            }
            Operation::Status { request_id } => {
                serde_json::to_value(handle.command_status(&request_id).await.map_err(remote)?)
            }
        };
        reply.map_err(|_| Error::Protocol)
    }
}
fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string())
}
