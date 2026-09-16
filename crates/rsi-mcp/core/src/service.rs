use crate::{
    discovery::discover,
    error::{McpError, Result},
    transport::Connection,
};
use rsi_credentials_protocol::CredentialsResolve;
use rsi_mcp_protocol::{
    McpConfig, McpManifest, McpToolChoice, McpTransportKind, ServerConfig, ServerManifest,
    ServerStatus, TransportConfig,
};
use rsi_process::DuplexProcess;
use rsi_sandbox::Sandbox;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
/// Validated immutable server schema with cached canonical identity and size.
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenServer {
    manifest: ServerManifest,
    sha256: String,
    encoded_bytes: usize,
}
impl FrozenServer {
    /// Freezes a discovered or decoded saved server manifest before dispatch.
    pub fn new(manifest: ServerManifest) -> Result<Self> {
        manifest.validate().map_err(|_| McpError::Protocol)?;
        let encoded_bytes = rsi_agent_session_protocol::DomainStateValue::encode(&manifest)
            .map_err(|_| McpError::Capacity)?
            .encoded_len();
        let sha256 = manifest.sha256();
        Ok(Self {
            manifest,
            sha256,
            encoded_bytes,
        })
    }
    /// Borrows the exact schema without mutation authority.
    pub fn manifest(&self) -> &ServerManifest {
        &self.manifest
    }
    /// Returns the cached canonical digest.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}
impl std::ops::Deref for FrozenServer {
    type Target = ServerManifest;
    fn deref(&self) -> &Self::Target {
        &self.manifest
    }
}
#[derive(Debug)]
struct Verified {
    manifest: Arc<FrozenServer>,
    connection: Arc<Connection>,
}
#[derive(Debug)]
struct Entry {
    config: ServerConfig,
    target_sha256: String,
    retired: AtomicBool,
    epoch: AtomicU64,
    refresh: Arc<Semaphore>,
    stop: CancellationToken,
    tasks: TaskTracker,
    verified: Mutex<Option<Verified>>,
    last: Mutex<Option<Arc<FrozenServer>>>,
    failure: Mutex<Option<McpError>>,
}
impl Entry {
    fn new(config: ServerConfig) -> Self {
        Self {
            target_sha256: config.target_sha256(),
            config,
            retired: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            refresh: Arc::new(Semaphore::new(1)),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            verified: Mutex::new(None),
            last: Mutex::new(None),
            failure: Mutex::new(None),
        }
    }
    fn invalidate(&self) {
        if self.retired.load(Ordering::Acquire) {
            self.stop.cancel();
        }
        if let Some(verified) = self.verified.lock().expect("MCP entry poisoned").as_ref() {
            verified.connection.close();
        }
    }
    fn current(&self) -> Result<(Arc<FrozenServer>, Arc<Connection>)> {
        if !self.config.enabled || self.retired.load(Ordering::Acquire) {
            return Err(McpError::Disabled);
        }
        let value = self.verified.lock().expect("MCP entry poisoned");
        let verified = value.as_ref().ok_or_else(|| {
            self.failure
                .lock()
                .expect("MCP failure poisoned")
                .unwrap_or(McpError::Disconnected)
        })?;
        if let Some(error) = verified.connection.failure() {
            return Err(error);
        }
        Ok((verified.manifest.clone(), verified.connection.clone()))
    }
    async fn shutdown(&self) {
        self.retired.store(true, Ordering::Release);
        self.invalidate();
        // A connecting refresh keeps its permit until its connection is closed.
        let _permit = self.refresh.acquire().await;
        let value = self.verified.lock().expect("MCP entry poisoned").take();
        if let Some(value) = value {
            value.connection.shutdown().await;
        }
        self.tasks.close();
        self.tasks.wait().await;
    }
}
/// Process-wide MCP owner. Only complete verified catalogs enter fresh compositions.
pub struct McpService {
    seed: Mutex<Option<SeedCache>>,
    entries: RwLock<BTreeMap<String, Arc<Entry>>>,
    configure: Arc<Semaphore>,
    tasks: TaskTracker,
    closed: AtomicBool,
    credentials: Arc<dyn CredentialsResolve>,
    process: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
}
struct SeedCache {
    servers: Vec<Arc<FrozenServer>>,
    seed: rsi_agent_composition_protocol::AgentGenerationSeed,
}
impl std::fmt::Debug for McpService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpService")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
struct RefreshGuard {
    entry: Arc<Entry>,
    connection: Option<Arc<Connection>>,
    settled: bool,
}
impl Drop for RefreshGuard {
    fn drop(&mut self) {
        if !self.settled {
            if let Some(connection) = &self.connection {
                connection.close();
            }
            *self.entry.failure.lock().expect("MCP failure poisoned") =
                Some(McpError::Disconnected);
        }
    }
}
impl McpService {
    /// Constructs a disabled owner; configuration is explicit and separately authorized.
    pub fn new(
        credentials: Arc<dyn CredentialsResolve>,
        process: Arc<dyn DuplexProcess>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
            seed: Mutex::new(None),
            configure: Arc::new(Semaphore::new(1)),
            tasks: TaskTracker::new(),
            closed: AtomicBool::new(false),
            credentials,
            process,
            sandbox,
        }
    }
    fn entry(&self, id: &str) -> Result<Arc<Entry>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::Disabled);
        }
        self.entries
            .read()
            .expect("MCP entries poisoned")
            .get(id)
            .cloned()
            .ok_or(McpError::NotFound)
    }
    /// Replaces explicit owner configuration, draining old epochs before returning.
    /// The caller must enforce Local-only changes to stdio entries.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the owner state lock.
    pub async fn configure(&self, config: McpConfig) -> Result<()> {
        config.validate().map_err(|_| McpError::Protocol)?;
        let permit = self
            .configure
            .clone()
            .try_acquire_owned()
            .map_err(|_| McpError::Busy)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::Disabled);
        }
        let old = {
            let mut entries = self.entries.write().expect("MCP entries poisoned");
            let mut old = std::mem::take(&mut *entries);
            for config in config.servers {
                let entry = if old
                    .get(&config.id)
                    .is_some_and(|entry| entry.config == config)
                {
                    old.remove(&config.id).expect("selected entry")
                } else {
                    Arc::new(Entry::new(config))
                };
                entries.insert(entry.config.id.clone(), entry);
            }
            for entry in old.values() {
                entry.retired.store(true, Ordering::Release);
                entry.invalidate();
            }
            old
        };
        let (send, receive) = oneshot::channel();
        self.tasks.spawn(async move {
            for (_, entry) in old {
                entry.shutdown().await;
            }
            drop(permit);
            let _ = send.send(());
        });
        tokio::time::timeout(std::time::Duration::from_secs(30), receive)
            .await
            .map_err(|_| McpError::Timeout)?
            .map_err(|_| McpError::Disconnected)
    }
    /// Captures all current complete manifests synchronously, without discovery or network work.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the owner state lock.
    pub fn manifest(&self) -> Result<McpManifest> {
        Ok(McpManifest {
            servers: self
                .current_manifest()?
                .iter()
                .map(|server| server.manifest.clone())
                .collect(),
        })
    }
    pub(crate) fn generation_seed(
        &self,
    ) -> Result<rsi_agent_composition_protocol::AgentGenerationSeed> {
        let servers = self.current_manifest()?;
        let mut cached = self.seed.lock().expect("MCP seed poisoned");
        if let Some(cached) = cached.as_ref()
            && cached.servers.len() == servers.len()
            && cached
                .servers
                .iter()
                .zip(&servers)
                .all(|(old, new)| Arc::ptr_eq(old, new))
        {
            return Ok(cached.seed.clone());
        }
        let manifest = McpManifest {
            servers: servers
                .iter()
                .map(|server| server.manifest.clone())
                .collect(),
        };
        let seed = rsi_agent_composition_protocol::AgentGenerationSeed::new(vec![
            manifest.snapshot().map_err(|_| McpError::Capacity)?,
        ])
        .map_err(|_| McpError::Capacity)?;
        *cached = Some(SeedCache {
            servers,
            seed: seed.clone(),
        });
        Ok(seed)
    }
    pub(crate) fn readiness(&self) -> Result<()> {
        self.current_manifest().map(|_| ())
    }
    fn current_manifest(&self) -> Result<Vec<Arc<FrozenServer>>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::Disabled);
        }
        let entries = self.entries.read().expect("MCP entries poisoned");
        let mut servers = Vec::new();
        let mut bytes = br#"{"servers":[]}"#.len();
        let mut tools = 0;
        let mut selected = 0;
        let mut resources = 0;
        let mut public_names = std::collections::BTreeSet::new();
        for entry in entries.values().filter(|entry| entry.config.enabled) {
            let server = entry.current()?.0;
            bytes += server.encoded_bytes + usize::from(!servers.is_empty());
            tools += server.tools.len();
            selected += server.tools.iter().filter(|tool| tool.selected).count();
            resources += server.resources.len() + usize::from(server.instructions.is_some());
            if bytes > rsi_agent_session_protocol::MAXIMUM_DOMAIN_STATE_BYTES
                || tools > rsi_mcp_protocol::MAXIMUM_TOOLS
                || selected + usize::from(resources > 0)
                    > rsi_tools_protocol::MAXIMUM_REGISTERED_TOOLS
                || resources > rsi_mcp_protocol::MAXIMUM_RESOURCES
            {
                return Err(McpError::Capacity);
            }
            servers.push(server);
        }
        for server in &servers {
            for tool in &server.tools {
                if !public_names.insert(&tool.public_name) {
                    return Err(McpError::Capacity);
                }
            }
        }
        Ok(servers)
    }
    /// Observes actual bounded endpoint state; no network request is made.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the owner state lock.
    pub fn status(&self) -> Vec<ServerStatus> {
        self.entries
            .read()
            .expect("MCP entries poisoned")
            .values()
            .map(|entry| {
                let current = entry.current();
                let last = entry
                    .last
                    .lock()
                    .expect("MCP last manifest poisoned")
                    .clone();
                ServerStatus {
                    id: entry.config.id.clone(),
                    transport: match entry.config.transport {
                        TransportConfig::StreamableHttp { .. } => McpTransportKind::Http,
                        TransportConfig::Stdio { .. } => McpTransportKind::Stdio,
                    },
                    credential: match &entry.config.transport {
                        TransportConfig::StreamableHttp { credential, .. } => credential.clone(),
                        TransportConfig::Stdio { .. } => None,
                    },
                    tools: last.as_ref().map_or_else(Vec::new, |manifest| {
                        manifest
                            .tools
                            .iter()
                            .map(|tool| McpToolChoice {
                                name: tool.definition.name.clone(),
                                selected: tool.selected,
                            })
                            .collect()
                    }),
                    enabled: entry.config.enabled,
                    epoch: entry.epoch.load(Ordering::Acquire).to_string(),
                    last_verified_sha256: last.as_ref().map(|manifest| manifest.sha256.clone()),
                    ready: current.is_ok(),
                    error: current.err(),
                }
            })
            .collect()
    }
    /// Explicitly reconnects and atomically verifies an entire catalog within 30 seconds.
    pub async fn refresh(
        &self,
        id: &str,
        cancellation: CancellationToken,
    ) -> Result<Arc<FrozenServer>> {
        let entry = self.entry(id)?;
        let permit = entry
            .refresh
            .clone()
            .try_acquire_owned()
            .map_err(|_| McpError::Busy)?;
        if !entry.config.enabled || entry.retired.load(Ordering::Acquire) {
            return Err(McpError::Disabled);
        }
        let credentials = self.credentials.clone();
        let process = self.process.clone();
        let sandbox = self.sandbox.clone();
        let cancelled = cancellation.child_token();
        let _cancel_on_drop = cancelled.clone().drop_guard();
        let (send, receive) = oneshot::channel();
        let owned = entry.clone();
        entry.tasks.spawn(async move {
            let result = refresh_owned(owned, credentials, process, sandbox, cancelled).await;
            // Reply loss cannot release admission before actual failed-connection settlement.
            drop(permit);
            let _ = send.send(result);
        });
        receive.await.map_err(|_| McpError::Disconnected)?
    }
    /// Invokes an exact frozen Tool only while its full catalog still matches this target.
    pub async fn call(
        &self,
        frozen: &FrozenServer,
        raw_name: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if !frozen
            .tools
            .iter()
            .any(|tool| tool.selected && tool.definition.name == raw_name)
        {
            return Err(McpError::NotFound);
        }
        self.request(
            frozen,
            "tools/call",
            json!({"name":raw_name,"arguments":arguments}),
            cancellation,
        )
        .await
    }
    /// Reads one resource declared in this frozen catalog; URIs grant no ambient fetch authority.
    pub async fn resource(
        &self,
        frozen: &FrozenServer,
        uri: &str,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        if !frozen.resources.iter().any(|resource| resource.uri == uri) {
            return Err(McpError::NotFound);
        }
        self.request(frozen, "resources/read", json!({"uri":uri}), cancellation)
            .await
    }
    async fn request(
        &self,
        frozen: &FrozenServer,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        let entry = self.entry(&frozen.id).map_err(|error| {
            if error == McpError::NotFound {
                McpError::CatalogChanged
            } else {
                error
            }
        })?;
        if entry.target_sha256 != frozen.target_sha256 {
            return Err(McpError::CatalogChanged);
        }
        let (current, connection) = entry.current()?;
        if current.sha256 != frozen.sha256 {
            return Err(McpError::CatalogChanged);
        }
        tokio::select! { () = cancellation.cancelled() => Err(McpError::Cancelled), value = connection.request(method, params) => value }
    }
    /// Stops admission and awaits all endpoint/child settlement.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the owner state lock.
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let _permit = self.configure.acquire().await;
        let entries = self
            .entries
            .read()
            .expect("MCP entries poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for entry in &entries {
            entry.retired.store(true, Ordering::Release);
            entry.invalidate();
        }
        for entry in entries {
            entry.shutdown().await;
        }
        self.tasks.close();
        self.tasks.wait().await;
    }
}
impl Drop for McpService {
    fn drop(&mut self) {
        for entry in self
            .entries
            .get_mut()
            .expect("MCP entries poisoned")
            .values()
        {
            entry.retired.store(true, Ordering::Release);
            entry.invalidate();
        }
    }
}
/// Local integration owner, distinct from any remote status/control operations.
#[derive(Debug)]
pub struct McpContract;
impl rsi_meta::LocalContract for McpContract {
    const KEY: &'static str = "rsi.mcp";
    type Service = McpService;
}

async fn refresh_owned(
    entry: Arc<Entry>,
    credentials: Arc<dyn CredentialsResolve>,
    process: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
    cancellation: CancellationToken,
) -> Result<Arc<FrozenServer>> {
    entry.epoch.fetch_add(1, Ordering::AcqRel);
    let previous = entry.verified.lock().expect("MCP entry poisoned").take();
    if let Some(previous) = previous {
        previous.connection.shutdown().await;
    }
    let mut guard = RefreshGuard {
        entry: entry.clone(),
        connection: None,
        settled: false,
    };
    let work = async {
        let mut connection = Connection::connect(
            &entry.config,
            credentials.clone(),
            process.clone(),
            sandbox.clone(),
        )
        .await?;
        guard.connection = Some(connection.clone());
        let mut discovered = discover(&connection, &entry.config, false).await;
        if connection.silent_probe()
            && discovered == Err(McpError::Timeout)
            && matches!(
                entry.config.transport,
                rsi_mcp_protocol::TransportConfig::Stdio { .. }
            )
        {
            // Only a silent discovery probe permits one legacy restart. It has
            // executed no Tool, and its process is reaped before reconnecting.
            connection.shutdown().await;
            connection = Connection::connect(&entry.config, credentials, process, sandbox).await?;
            guard.connection = Some(connection.clone());
            discovered = discover(&connection, &entry.config, true).await;
        }
        let manifest = Arc::new(FrozenServer::new(discovered?)?);
        if entry.retired.load(Ordering::Acquire) {
            return Err(McpError::Disabled);
        }
        Ok((manifest, connection))
    };
    let outcome = tokio::select! {
        biased;
        () = entry.stop.cancelled() => Err(McpError::Disabled),
        () = cancellation.cancelled() => Err(McpError::Cancelled),
        outcome = tokio::time::timeout(std::time::Duration::from_secs(30), work) => outcome.map_err(|_| McpError::Timeout).and_then(|result| result),
    };
    guard.settled = true;
    match outcome {
        Ok((manifest, connection)) => {
            *entry.last.lock().expect("MCP last manifest poisoned") = Some(manifest.clone());
            *entry.failure.lock().expect("MCP failure poisoned") = None;
            *entry.verified.lock().expect("MCP entry poisoned") = Some(Verified {
                manifest: manifest.clone(),
                connection,
            });
            Ok(manifest)
        }
        Err(error) => {
            if let Some(connection) = &guard.connection {
                connection.shutdown().await;
            }
            *entry.failure.lock().expect("MCP failure poisoned") = Some(error);
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
