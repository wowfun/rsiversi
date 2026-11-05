use super::Sessions;
use rsi_extension::{Error, Result};
use rsi_session_protocol::{
    SessionError, SessionHandle,
    terminal::{Operation, Reply, Request},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

impl Sessions {
    pub(super) async fn wait_terminal(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
            key: String,
        }
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        handle
            .wait_terminal_closed(&input.key)
            .await
            .map_err(|e| Error::Remote(e.to_string()))?;
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(Value::Null)
    }
    pub(super) async fn actor(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
        }
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        let header = handle.header().await.map_err(|_| Error::Cancelled)?;
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(
            json!({"id":header.session_id(),"createdAt":header.created_at_ms(),
            "cwd":(header.coordinates().location()==&rsi_execution_protocol::ExecutionLocation::Local).then(||header.canonical_cwd())}),
        )
    }
    pub(super) async fn terminal(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
            operation: Option<rsi_session_protocol::terminal::Request>,
        }
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        let value = if let Some(
            operation @ (Request::Create { .. }
            | Request::Operate {
                operation: Operation::Attach { .. },
            }),
        ) = input.operation
        {
            self.open_terminal(&input.request, handle, operation)
                .await?
        } else if let Some(operation) = input.operation {
            let reply = handle
                .terminal(operation)
                .await
                .map_err(|e| Error::Remote(e.to_string()))?;
            serde_json::to_value(reply).map_err(|_| Error::Protocol)?
        } else {
            let catalog = handle
                .terminal_catalog()
                .await
                .map_err(|e| Error::Remote(e.to_string()))?;
            serde_json::to_value(catalog).map_err(|_| Error::Protocol)?
        };
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(value)
    }
    async fn open_terminal(
        &self,
        request: &str,
        handle: Arc<dyn SessionHandle>,
        operation: Request,
    ) -> Result<Value> {
        let create = matches!(operation, Request::Create { .. });
        let opened = handle.clone();
        let held = match tokio::spawn(async move { opened.borrow_terminal(operation).await })
            .await
            .map_err(|_| Error::Disconnected)?
        {
            Ok(held) => held,
            Err(SessionError::TerminalKeyRetired) => return Ok(json!({"type":"retired"})),
            Err(error) => return Err(Error::Remote(error.to_string())),
        };
        let reply = if create {
            let id = held.receipt().terminal.id.clone();
            drop(held);
            let Reply::List(rows) = handle
                .terminal(Request::Operate {
                    operation: Operation::List,
                })
                .await
                .map_err(|e| Error::Remote(e.to_string()))?
            else {
                return Err(Error::Protocol);
            };
            let Some(terminal) = rows.into_iter().find(|row| row.id == id) else {
                return Ok(json!({"type":"retired"}));
            };
            Reply::Terminal(terminal)
        } else {
            let receipt = held.receipt().clone();
            self.routes
                .retain_resource(request, format!("pty/{}", receipt.id), Arc::new(held))
                .map_err(|_| Error::Cancelled)?;
            Reply::Attached(receipt)
        };
        serde_json::to_value(reply).map_err(|_| Error::Protocol)
    }
    pub(super) fn release_terminal(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Release {
            request: String,
            attachment: String,
        }
        let release: Release = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        self.routes
            .release_resource(&release.request, &format!("pty/{}", release.attachment));
        Ok(Value::Null)
    }
    pub(super) async fn interactive_handle(
        &self,
        request: &str,
        id: &rsi_agent_session_protocol::SessionId,
    ) -> Result<Arc<dyn rsi_session_protocol::SessionHandle>> {
        let handle = self.read_handle(request, id, None).await?;
        let header = handle
            .header()
            .await
            .map_err(|_| Error::Remote("Session header unavailable".into()))?;
        if header.fork_origin().is_some() {
            return Err(Error::Remote(
                "Child Session requires its parent command".into(),
            ));
        }
        self.routes.caller(request).map_err(|_| Error::Cancelled)?;
        Ok(handle)
    }

    pub(super) async fn cancel(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
            #[serde(default)]
            keep_inbox: bool,
            #[serde(default)]
            inbox_only: bool,
        }
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let receipt = handle
            .cancel(
                if input.inbox_only {
                    rsi_agent_turn_protocol::CancelTarget::Inbox
                } else {
                    rsi_agent_turn_protocol::CancelTarget::Session {
                        keep_inbox: input.keep_inbox,
                    }
                },
                None,
            )
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
        Ok(json!({"session_id":input.session_id,"receipt":receipt}))
    }
    pub(super) async fn submit(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
            message_id: rsi_agent_session_protocol::MessageId,
            #[serde(default)]
            origin: rsi_session_protocol::SubmissionOrigin,
            attribution: Option<rsi_agent_session_protocol::MessageAttribution>,
            delivery: rsi_agent_session_protocol::MessageDelivery,
            content: rsi_session_protocol::ValidatedSessionInput,
        }
        let input: Input =
            serde_json::from_value(payload).map_err(|e| Error::Remote(e.to_string()))?;
        let handle = self
            .interactive_handle(&input.request, &input.session_id)
            .await?;
        let receipt = handle
            .submit(rsi_session_protocol::SubmitInput {
                origin: input.origin,
                attribution: input.attribution,
                delivery: input.delivery,
                message_id: input.message_id,
                content: input.content,
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
        serde_json::to_value(receipt).map_err(|_| Error::Protocol)
    }
}
