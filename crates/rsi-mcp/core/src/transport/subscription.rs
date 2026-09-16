use super::{Connection, State, Transport, wire};
use crate::error::{McpError, Result};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::Ordering};
use tokio::sync::oneshot;

pub(super) struct Subscription {
    id: String,
    tools: bool,
    resources: bool,
    accepted: Mutex<Option<(bool, bool)>>,
    ready: Mutex<Option<oneshot::Sender<()>>>,
}
impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl Subscription {
    fn message(&self, value: &Value) -> Result<()> {
        if value.get("method").is_none() {
            let result = wire::result(value.clone(), &self.id)?;
            if result.get("resultType").and_then(Value::as_str) != Some("complete")
                || result
                    .pointer("/_meta/io.modelcontextprotocol~1subscriptionId")
                    .and_then(Value::as_str)
                    != Some(&self.id)
            {
                return Err(McpError::Protocol);
            }
            return Err(McpError::Disconnected);
        }
        if value.get("id").is_some()
            || value
                .pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId")
                .and_then(Value::as_str)
                != Some(&self.id)
        {
            return Err(McpError::Protocol);
        }
        let mut accepted = self.accepted.lock().expect("MCP subscription poisoned");
        match value.get("method").and_then(Value::as_str) {
            Some("notifications/subscriptions/acknowledged") if accepted.is_none() => {
                let filters = value
                    .pointer("/params/notifications")
                    .and_then(Value::as_object)
                    .ok_or(McpError::Protocol)?;
                let flag = |name: &str| -> Result<bool> {
                    filters
                        .get(name)
                        .map_or(Ok(false), |value| value.as_bool().ok_or(McpError::Protocol))
                };
                let tools = flag("toolsListChanged")?;
                let resources = flag("resourcesListChanged")?;
                if (tools && !self.tools)
                    || (resources && !self.resources)
                    || filters.keys().any(|key| {
                        !matches!(key.as_str(), "toolsListChanged" | "resourcesListChanged")
                    })
                {
                    return Err(McpError::Protocol);
                }
                *accepted = Some((tools, resources));
                if let Some(ready) = self
                    .ready
                    .lock()
                    .expect("MCP subscription ready poisoned")
                    .take()
                {
                    let _ = ready.send(());
                }
                Ok(())
            }
            Some("notifications/tools/list_changed") if accepted.is_some_and(|flags| flags.0) => {
                Err(McpError::CatalogChanged)
            }
            Some("notifications/resources/list_changed")
                if accepted.is_some_and(|flags| flags.1) =>
            {
                Err(McpError::CatalogChanged)
            }
            _ => Err(McpError::Protocol),
        }
    }
}
impl State {
    pub(super) fn subscription_message(&self, value: &Value, exclusive: bool) -> Result<bool> {
        let subscription = self
            .subscription
            .lock()
            .expect("MCP subscription state poisoned")
            .clone();
        let related = exclusive
            || wire::changed(value)
            || value.get("method").and_then(Value::as_str)
                == Some("notifications/subscriptions/acknowledged")
            || value
                .pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId")
                .is_some()
            || subscription
                .as_ref()
                .is_some_and(|s| value.get("id").and_then(Value::as_str) == Some(&s.id));
        if related {
            subscription.ok_or(McpError::Protocol)?.message(value)?;
        }
        Ok(related)
    }
}
impl Connection {
    pub(crate) async fn subscribe(&self, capabilities: &Value) -> Result<()> {
        if !self.modern() {
            return Ok(());
        }
        let tools = capabilities
            .pointer("/tools/listChanged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let resources = capabilities
            .pointer("/resources/listChanged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !tools && !resources {
            return Ok(());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed).to_string();
        let (ready, wait) = oneshot::channel();
        let subscription = Arc::new(Subscription {
            id: id.clone(),
            tools,
            resources,
            accepted: Mutex::new(None),
            ready: Mutex::new(Some(ready)),
        });
        *self
            .state
            .subscription
            .lock()
            .expect("MCP subscription state poisoned") = Some(subscription);
        let mut params =
            json!({"notifications":{"toolsListChanged":tools,"resourcesListChanged":resources}});
        self.state.request_meta(&mut params)?;
        let request =
            json!({"jsonrpc":"2.0","id":id,"method":"subscriptions/listen","params":params});
        match &self.transport {
            Transport::Http(peer) => peer.subscribe(&request).await?,
            Transport::Stdio(peer) => {
                peer.exchange(wire::encode(&request)?, None).await?;
            }
        }
        tokio::select! {
            () = self.state.stop.cancelled() => Err(self.failure().unwrap_or(McpError::Disconnected)),
            result = tokio::time::timeout(std::time::Duration::from_secs(10), wait) => result.map_err(|_| McpError::Timeout)?.map_err(|_| McpError::Disconnected),
        }
    }
}
