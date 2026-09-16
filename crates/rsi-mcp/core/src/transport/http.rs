use super::{State, wire};
use crate::error::{McpError, Result};
use futures_util::StreamExt;
use reqwest::{
    Client, Method, Response, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue},
};
use rsi_credentials_protocol::{CredentialRef, CredentialsResolve};
use rsi_mcp_protocol::MAXIMUM_FRAME_BYTES;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
struct Inner {
    client: Client,
    url: String,
    credential: Option<CredentialRef>,
    credentials: Arc<dyn CredentialsResolve>,
    state: Arc<State>,
    headers: Mutex<(Option<HeaderValue>, Option<HeaderValue>)>,
}
pub(super) struct Http {
    inner: Arc<Inner>,
    watcher: Mutex<Option<JoinHandle<()>>>,
}
impl Inner {
    async fn send(
        &self,
        method: Method,
        body: Option<&Value>,
        extra: &[(String, String)],
    ) -> Result<Response> {
        let mut request = self
            .client
            .request(method, &self.url)
            .header("accept-encoding", "identity")
            .header(ACCEPT, "application/json, text/event-stream");
        if let Some(reference) = &self.credential {
            let resolved = self
                .credentials
                .resolve(reference)
                .await
                .map_err(|_| McpError::CredentialUnavailable)?;
            let mut header =
                HeaderValue::from_str(&format!("Bearer {}", resolved.secret.expose_secret()))
                    .map_err(|_| McpError::CredentialUnavailable)?;
            header.set_sensitive(true);
            request = request.header(AUTHORIZATION, header);
        }
        {
            let headers = self.headers.lock().expect("MCP headers poisoned");
            if let Some(session) = &headers.0 {
                request = request.header("mcp-session-id", session);
            }
            if let Some(version) = &headers.1 {
                request = request.header("mcp-protocol-version", version);
            }
        }
        if let Some(body) = body {
            if self.state.modern() {
                let method = body
                    .get("method")
                    .and_then(Value::as_str)
                    .ok_or(McpError::Protocol)?;
                request = request
                    .header(
                        "mcp-protocol-version",
                        rsi_mcp_protocol::LATEST_PROTOCOL_VERSION,
                    )
                    .header("mcp-method", method);
                if matches!(method, "tools/call" | "prompts/get" | "resources/read") {
                    let key = if method == "resources/read" {
                        "uri"
                    } else {
                        "name"
                    };
                    let name = body
                        .get("params")
                        .and_then(|params| params.get(key))
                        .and_then(Value::as_str)
                        .ok_or(McpError::Protocol)?;
                    request =
                        request.header("mcp-name", rsi_mcp_protocol::encode_header_value(name));
                }
                for (name, value) in extra {
                    request = request.header(name, value);
                }
            }
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(wire::encode(body)?);
        }
        let response = request.send().await.map_err(|_| McpError::Disconnected)?;
        if response
            .headers()
            .get_all("content-encoding")
            .iter()
            .any(|value| value != "identity")
        {
            return Err(McpError::Protocol);
        }
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(McpError::CredentialUnavailable);
        }
        if let Some(session) = response.headers().get("mcp-session-id")
            && !self.state.modern()
        {
            if session.is_empty()
                || session.as_bytes().len() > 256
                || !session.as_bytes().iter().all(|c| (0x21..=0x7e).contains(c))
            {
                return Err(McpError::Protocol);
            }
            let mut headers = self.headers.lock().expect("MCP headers poisoned");
            if headers.0.as_ref().is_some_and(|current| current != session) {
                return Err(McpError::Protocol);
            }
            if headers.0.is_none() {
                let mut session = session.clone();
                session.set_sensitive(true);
                headers.0 = Some(session);
            }
        }
        Ok(response)
    }
    async fn server_message(&self, value: &Value) -> Result<()> {
        if self.state.modern() {
            if value.get("id").is_some()
                || wire::changed(value)
                || value.get("method").and_then(Value::as_str)
                    == Some("notifications/subscriptions/acknowledged")
                || value
                    .pointer("/params/_meta/io.modelcontextprotocol~1subscriptionId")
                    .is_some()
            {
                return Err(McpError::Protocol);
            }
            return Ok(());
        }
        if wire::changed(value) {
            return Err(McpError::CatalogChanged);
        }
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .ok_or(McpError::Protocol)?;
        if let Some(id) = value.get("id") {
            if !valid_server_id(id) {
                return Err(McpError::Protocol);
            }
            let reply = if method == "ping" {
                json!({"jsonrpc":"2.0","id":id,"result":{}})
            } else {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Client capability is not available"}})
            };
            let response = self.send(Method::POST, Some(&reply), &[]).await?;
            if !matches!(
                response.status(),
                StatusCode::ACCEPTED | StatusCode::NO_CONTENT
            ) {
                return Err(McpError::Protocol);
            }
        }
        Ok(())
    }
}
pub(super) fn valid_server_id(id: &Value) -> bool {
    id.as_str()
        .is_some_and(|id| !id.is_empty() && id.len() <= 128)
        || id.as_i64().is_some()
        || id.as_u64().is_some()
}
fn content_type(response: &Response) -> Result<&str> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .ok_or(McpError::Protocol)
}
impl Http {
    pub fn new(
        url: &str,
        credential: Option<CredentialRef>,
        credentials: Arc<dyn CredentialsResolve>,
        state: Arc<State>,
    ) -> Result<Self> {
        let mut client = Client::builder()
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10));
        // The cleartext localhost exception never consults DNS or an ambient hosts file.
        if url::Url::parse(url)
            .map_err(|_| McpError::Protocol)?
            .host_str()
            == Some("localhost")
        {
            client = client.resolve_to_addrs(
                "localhost",
                &[
                    std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
                ],
            );
        }
        let client = client.build().map_err(|_| McpError::Disconnected)?;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                url: url.to_owned(),
                credential,
                credentials,
                state,
                headers: Mutex::new((None, None)),
            }),
            watcher: Mutex::new(None),
        })
    }
    pub fn set_version(&self, version: &str) {
        self.inner.headers.lock().expect("MCP headers poisoned").1 =
            Some(HeaderValue::from_str(version).expect("validated protocol version"));
    }
    async fn error_response(
        &self,
        response: Response,
        request: &Value,
        id: &str,
    ) -> Result<Option<Value>> {
        let status = response.status();
        if !matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND) {
            return Err(McpError::Disconnected);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| McpError::Disconnected)?;
            if chunk.len() > MAXIMUM_FRAME_BYTES.saturating_sub(bytes.len()) {
                return Err(McpError::Capacity);
            }
            bytes.extend_from_slice(&chunk);
        }
        let parsed = wire::parse(&bytes);
        let modern_error = parsed.as_ref().is_ok_and(|value| {
            value.get("error").is_some_and(|error| {
                matches!(
                    wire::remote_error(error),
                    Ok(McpError::UnsupportedVersion
                        | McpError::RequiredCapability
                        | McpError::HeaderMismatch)
                )
            })
        });
        let error = parsed.and_then(|value| wire::result(value, id));
        if modern_error {
            return error.map(Some);
        }
        if status == StatusCode::BAD_REQUEST
            && request.get("method").and_then(Value::as_str) == Some("server/discover")
        {
            return Err(McpError::RemoteError);
        }
        error.and_then(|_| Err(McpError::Protocol))
    }
    pub async fn subscribe(&self, request: &Value) -> Result<()> {
        let response = self.inner.send(Method::POST, Some(request), &[]).await?;
        if response.status() != StatusCode::OK {
            return self
                .error_response(
                    response,
                    request,
                    request["id"].as_str().ok_or(McpError::Protocol)?,
                )
                .await
                .map(|_| ());
        }
        if content_type(&response)? != "text/event-stream" {
            return Err(McpError::Protocol);
        }
        let inner = self.inner.clone();
        let task = tokio::spawn(async move {
            let work = async {
                let mut stream = response.bytes_stream();
                let mut events = wire::Events::default();
                while let Some(chunk) = stream.next().await {
                    for value in events.feed(&chunk.map_err(|_| McpError::Disconnected)?)? {
                        inner.state.subscription_message(&value, true)?;
                    }
                }
                Err::<(), McpError>(McpError::Disconnected)
            };
            tokio::select! { () = inner.state.stop.cancelled() => {}, result = work => { if let Err(error) = result { inner.state.fail(error); } } }
            inner.state.invalidate();
        });
        *self.watcher.lock().expect("MCP watcher poisoned") = Some(task);
        Ok(())
    }
    pub async fn exchange(
        &self,
        request: &Value,
        headers: &[(String, String)],
        id: Option<&str>,
    ) -> Result<Option<Value>> {
        let response = self
            .inner
            .send(Method::POST, Some(request), headers)
            .await?;
        let Some(id) = id else {
            return if matches!(
                response.status(),
                StatusCode::ACCEPTED | StatusCode::NO_CONTENT
            ) {
                Ok(None)
            } else {
                Err(McpError::Protocol)
            };
        };
        if response.status() != StatusCode::OK {
            return self.error_response(response, request, id).await;
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAXIMUM_FRAME_BYTES as u64)
        {
            return Err(McpError::Capacity);
        }
        let sse = match content_type(&response)? {
            "text/event-stream" => true,
            "application/json" => false,
            _ => return Err(McpError::Protocol),
        };
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        let mut events = wire::Events::default();
        let mut total = 0usize;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| McpError::Disconnected)?;
            if chunk.len() > MAXIMUM_FRAME_BYTES.saturating_sub(total) {
                return Err(McpError::Capacity);
            }
            total += chunk.len();
            if sse {
                let messages = events.feed(&chunk)?;
                let mut result = None;
                for value in messages {
                    if value.get("method").is_some() {
                        self.inner.server_message(&value).await?;
                    } else {
                        if result.is_some() {
                            return Err(McpError::Protocol);
                        }
                        result = Some(wire::result(value, id)?);
                    }
                }
                if let Some(result) = result {
                    return Ok(Some(result));
                }
            } else {
                bytes.extend_from_slice(&chunk);
            }
        }
        if sse {
            Err(McpError::Disconnected)
        } else {
            wire::result(wire::parse(&bytes)?, id).map(Some)
        }
    }
    pub async fn watch(&self) -> Result<()> {
        let response = self.inner.send(Method::GET, None, &[]).await?;
        // Streamable HTTP explicitly allows servers to decline a separate event stream.
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        if response.status() != StatusCode::OK || content_type(&response)? != "text/event-stream" {
            return Err(McpError::Protocol);
        }
        let inner = self.inner.clone();
        let task = tokio::spawn(async move {
            let work = async {
                let mut stream = response.bytes_stream();
                let mut events = wire::Events::default();
                while let Some(chunk) = stream.next().await {
                    for value in events.feed(&chunk.map_err(|_| McpError::Disconnected)?)? {
                        tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            inner.server_message(&value),
                        )
                        .await
                        .map_err(|_| McpError::Timeout)??;
                    }
                }
                Err::<(), _>(McpError::Disconnected)
            };
            tokio::select! { () = inner.state.stop.cancelled() => {}, result = work => { if let Err(error) = result { inner.state.fail(error); } } }
            inner.state.invalidate();
        });
        *self.watcher.lock().expect("MCP watcher poisoned") = Some(task);
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.inner.state.invalidate();
        let task = self.watcher.lock().expect("MCP watcher poisoned").take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.inner.state.invalidate();
    }
}
