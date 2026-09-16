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
mod protocol;
mod stdio;
mod subscription;
mod wire;
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
        };
        Ok(Arc::new(Self {
            transport,
            state,
            next: AtomicU64::new(1),
            silent_probe: AtomicBool::new(false),
            admission: Semaphore::new(1),
            parameters: Mutex::new(std::collections::BTreeMap::new()),
        }))
    }
    pub fn valid(&self) -> bool {
        self.state.valid()
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
    pub async fn shutdown(&self) {
        self.close();
        match &self.transport {
            Transport::Http(peer) => peer.shutdown().await,
            Transport::Stdio(peer) => peer.shutdown().await,
        }
    }
    pub async fn request(&self, method: &str, mut params: Value) -> Result<Value> {
        let _permit = self.admission.try_acquire().map_err(|_| McpError::Busy)?;
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed).to_string();
        self.state.request_meta(&mut params)?;
        let headers = self.parameter_headers(method, &params)?;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let bytes = wire::encode(&request)?;
        let timeout =
            if method == "server/discover" && matches!(self.transport, Transport::Stdio(_)) {
                std::time::Duration::from_secs(2)
            } else {
                std::time::Duration::from_secs(30)
            };
        let mut guard = ExchangeGuard {
            connection: self,
            settled: false,
        };
        let future = async {
            match &self.transport {
                Transport::Http(peer) => peer.exchange(&request, &headers, Some(&id)).await,
                Transport::Stdio(peer) => peer.exchange(bytes, Some(&id)).await,
            }
        };
        let value = tokio::select! {
            () = self.state.stop.cancelled() => Err(self.failure().unwrap_or(McpError::Disconnected)),
            value = tokio::time::timeout(timeout, future) => value.map_err(|_| McpError::Timeout)?,
        };
        // An ordinary remote RPC error settles this exact exchange; it is not a disconnect.
        let value = value.map_err(|error| self.failure().unwrap_or(error));
        guard.settled = value.is_ok()
            || matches!(
                value,
                Err(McpError::RemoteError
                    | McpError::HeaderMismatch
                    | McpError::RequiredCapability
                    | McpError::UnsupportedVersion)
            );
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
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            match &self.transport {
                Transport::Http(peer) => {
                    peer.set_version(version);
                    peer.exchange(&request, &[], None).await?;
                    peer.watch().await?;
                }
                Transport::Stdio(peer) => {
                    peer.exchange(bytes, None).await?;
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
