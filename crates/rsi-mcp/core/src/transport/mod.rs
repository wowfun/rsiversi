use crate::error::{McpError, Result};
use rsi_credentials_protocol::CredentialsResolve;
use rsi_mcp_protocol::{ServerConfig, TransportConfig};
use rsi_process::DuplexProcess;
use rsi_sandbox::Sandbox;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
mod http;
pub(crate) mod process;
mod protocol;
mod stdio;
mod subscription;
mod wire;

pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const MAXIMUM_OUTSTANDING_REQUESTS: usize = 9;

#[derive(Debug)]
pub(crate) struct State {
    failure: Mutex<Option<McpError>>,
    pub stop: CancellationToken,
    version: Mutex<&'static str>,
    subscription: Mutex<Option<Arc<subscription::Subscription>>>,
}
impl State {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            failure: Mutex::new(None),
            stop: CancellationToken::new(),
            version: Mutex::new(rsi_mcp_protocol::LATEST_PROTOCOL_VERSION),
            subscription: Mutex::new(None),
        })
    }
    pub fn fail(&self, error: McpError) {
        self.failure
            .lock()
            .expect("MCP state poisoned")
            .get_or_insert(error);
        self.stop.cancel();
    }
    pub fn invalidate(&self) {
        self.fail(McpError::Disconnected);
    }
    pub fn failure(&self) -> Option<McpError> {
        *self.failure.lock().expect("MCP state poisoned")
    }
    pub fn valid(&self) -> bool {
        self.failure().is_none()
    }
}
enum Transport {
    Http(http::Http),
    Stdio(stdio::Stdio),
}
/// One exact connection epoch. A cancelled started exchange invalidates the epoch.
pub(crate) struct Connection {
    transport: Transport,
    state: Arc<State>,
    next: AtomicU64,
    silent_probe: AtomicBool,
    admission: Semaphore,
    outstanding: Semaphore,
    parameters: Mutex<std::collections::BTreeMap<String, Vec<rsi_mcp_protocol::HttpParameter>>>,
}
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConnection")
            .field("valid", &self.valid())
            .finish_non_exhaustive()
    }
}
struct ExchangeGuard<'a> {
    connection: &'a Connection,
    settled: bool,
}
impl Drop for ExchangeGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.connection.close();
        }
    }
}
impl Connection {
    pub async fn connect(
        config: &ServerConfig,
        credentials: Arc<dyn CredentialsResolve>,
        process: Arc<dyn DuplexProcess>,
        sandbox: Arc<dyn Sandbox>,
        execution: Option<&rsi_execution::ExecutionLease>,
    ) -> Result<Arc<Self>> {
        let state = State::new();
        let transport = match &config.transport {
            TransportConfig::StreamableHttp { url, credential } => Transport::Http(
                http::Http::new(url, credential.clone(), credentials, state.clone())?,
            ),
            TransportConfig::Stdio {
                program,
                arguments,
                cwd,
                environment,
            } => Transport::Stdio(
                stdio::Stdio::spawn(
                    program,
                    arguments,
                    cwd,
                    environment,
                    credentials.as_ref(),
                    process.as_ref(),
                    sandbox.as_ref(),
                    state.clone(),
                )
                .await?,
            ),
            TransportConfig::SshStdio { .. } => Transport::Stdio(
                stdio::Stdio::spawn_target(
                    config,
                    execution.ok_or(McpError::ProcessUnavailable)?,
                    credentials.as_ref(),
                    state.clone(),
                )
                .await?,
            ),
        };
        Ok(Arc::new(Self {
            transport,
            state,
            next: AtomicU64::new(1),
            silent_probe: AtomicBool::new(false),
            admission: Semaphore::new(1),
            outstanding: Semaphore::new(MAXIMUM_OUTSTANDING_REQUESTS),
            parameters: Mutex::new(std::collections::BTreeMap::new()),
        }))
    }
    pub fn valid(&self) -> bool {
        self.state.valid()
    }
    pub fn seal_bootstrap(&self) {
        if let Transport::Stdio(peer) = &self.transport {
            peer.seal();
        }
    }
    pub fn failure(&self) -> Option<McpError> {
        self.state.failure()
    }
    pub fn close(&self) {
        self.state.invalidate();
        if let Transport::Stdio(peer) = &self.transport {
            peer.close();
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.close();
        match &self.transport {
            Transport::Http(peer) => {
                peer.shutdown().await;
                Ok(())
            }
            Transport::Stdio(peer) => peer.shutdown().await,
        }
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let timeout =
            if method == "server/discover" && matches!(self.transport, Transport::Stdio(_)) {
                std::time::Duration::from_secs(2)
            } else {
                REQUEST_TIMEOUT
            };
        self.request_until(method, params, tokio::time::Instant::now() + timeout)
            .await
    }
    pub async fn request_until(
        &self,
        method: &str,
        params: Value,
        deadline: tokio::time::Instant,
    ) -> Result<Value> {
        self.request_observed(method, params, deadline, None).await
    }
    pub async fn request_observed(
        &self,
        method: &str,
        params: Value,
        deadline: tokio::time::Instant,
        dispatched: Option<&AtomicBool>,
    ) -> Result<Value> {
        self.request_authorized(method, params, deadline, dispatched, None)
            .await
    }
    pub async fn request_authorized(
        &self,
        method: &str,
        mut params: Value,
        deadline: tokio::time::Instant,
        dispatched: Option<&AtomicBool>,
        execution: Option<&rsi_execution::ExecutionLease>,
    ) -> Result<Value> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let _outstanding = self.outstanding.try_acquire().map_err(|_| McpError::Busy)?;
        let id = self.next.fetch_add(1, Ordering::Relaxed).to_string();
        self.state.request_meta(&mut params)?;
        let headers = self.parameter_headers(method, &params)?;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let bytes = wire::encode(&request)?;
        let body = http::RequestBody::new(&request, bytes);
        drop(request);
        let _permit = tokio::select! { biased;
            () = self.state.stop.cancelled() => return Err(self.failure().unwrap_or(McpError::Disconnected)),
            () = tokio::time::sleep_until(deadline) => return Err(McpError::Timeout),
            permit = self.admission.acquire() => permit.map_err(|_| McpError::Disconnected)?,
        };
        if let Some(error) = self.failure() {
            return Err(error);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(McpError::Timeout);
        }
        let exchange = match &self.transport {
            Transport::Stdio(peer) => Some(peer.admit(execution)?),
            Transport::Http(_) => None,
        };
        let mut guard = ExchangeGuard {
            connection: self,
            settled: false,
        };
        let future = async {
            match &self.transport {
                Transport::Http(peer) => peer.exchange(body, &headers, Some(&id), dispatched).await,
                Transport::Stdio(peer) => {
                    peer.exchange(
                        body.into_bytes(),
                        Some(&id),
                        dispatched,
                        exchange.as_ref().expect("stdio exchange"),
                    )
                    .await
                }
            }
        };
        let value = tokio::select! {
            () = self.state.stop.cancelled() => Err(self.failure().unwrap_or(McpError::Disconnected)),
            value = tokio::time::timeout_at(deadline, future) => value.map_err(|_| McpError::Timeout)?,
        };
        // An ordinary remote RPC error settles this exact exchange; it is not a disconnect.
        let value = value.map_err(|error| self.failure().unwrap_or(error));
        guard.settled = value.is_ok()
            || matches!(
                value,
                Err(McpError::RemoteError
                    | McpError::MethodNotFound
                    | McpError::HeaderMismatch
                    | McpError::RequiredCapability
                    | McpError::UnsupportedVersion)
            );
        if guard.settled
            && let Some(exchange) = &exchange
        {
            exchange.finish();
        }
        if let Err(error) = value
            && !guard.settled
        {
            self.state.fail(error);
        }
        let result = value.and_then(|value| value.ok_or(McpError::Protocol))?;
        let result = self.state.complete_result(method, result);
        if result == Err(McpError::Protocol) {
            self.state.fail(McpError::Protocol);
        }
        result
    }
    pub async fn initialized(&self, version: &str) -> Result<()> {
        let _permit = self.admission.try_acquire().map_err(|_| McpError::Busy)?;
        self.state.set_version(version)?;
        let request = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        let bytes = wire::encode(&request)?;
        let mut guard = ExchangeGuard {
            connection: self,
            settled: false,
        };
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            match &self.transport {
                Transport::Http(peer) => {
                    peer.set_version(version);
                    peer.exchange(http::RequestBody::new(&request, bytes), &[], None, None)
                        .await?;
                    peer.watch().await?;
                }
                Transport::Stdio(peer) => {
                    let exchange = peer.admit(None)?;
                    peer.exchange(bytes, None, None, &exchange).await?;
                    exchange.finish();
                }
            }
            Ok::<_, McpError>(())
        })
        .await
        .map_err(|_| McpError::Timeout)??;
        guard.settled = true;
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
