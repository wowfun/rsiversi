//! Product-owned private Session inputs; wire framing stays in ACP.
use rsi_acp::server::Failure;
use rsi_acp_protocol::schema;
use rsi_agent_composition::AgentCompositionFactory;
use rsi_agent_composition_protocol::{
    AgentCompositionContract, AgentCompositionError, AgentCompositionPin, AgentGenerationSeed,
};
use rsi_agent_presets::{AgentPresetCatalogConfig, AgentPresetId};
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use rsi_credentials_protocol::{
    CredentialRef, CredentialSource, CredentialsError, CredentialsResolve, ResolvedCredential,
    SecretValue,
};
use rsi_meta::{Context, FiberHandle, LocalContract, ResolvedFactory, UpdateMode};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) const PRESET: &str = "acp-internal";
pub(crate) struct InputsContract;
impl LocalContract for InputsContract {
    const KEY: &'static str = "rsi.acp.private-inputs";
    type Service = PrivateInputs;
}
pub(crate) struct PrivateInputs {
    source: Arc<crate::integration_source::SeededSource>,
    context: Context,
    paths: rsi_host::HostPaths,
    process: Arc<dyn rsi_process::DuplexProcess>,
    sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    builds: Arc<tokio::sync::Semaphore>,
    tasks: TaskTracker,
    stop: CancellationToken,
}
impl std::fmt::Debug for PrivateInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateAcpInputs").finish_non_exhaustive()
    }
}
struct Binding {
    cwd: String,
    request_sha256: [u8; 32],
    pin: AgentCompositionPin,
    fiber: FiberHandle,
    mcp: Arc<rsi_mcp::McpService>,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<SessionId, Binding>,
    pending: BTreeMap<SessionId, CancellationToken>,
}

impl PrivateInputs {
    pub(crate) fn new(
        source: Arc<crate::integration_source::SeededSource>,
        context: Context,
        paths: rsi_host::HostPaths,
        process: Arc<dyn rsi_process::DuplexProcess>,
        sandbox: Arc<dyn rsi_sandbox::Sandbox>,
    ) -> Arc<Self> {
        Arc::new(Self {
            source,
            context,
            paths,
            process,
            sandbox,
            state: Mutex::new(State::default()),
            changed: tokio::sync::Notify::new(),
            builds: Arc::new(tokio::sync::Semaphore::new(8)),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
        })
    }
    pub(crate) fn pin(
        &self,
        header: &SessionHeader,
        seed: Option<&AgentGenerationSeed>,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        let unavailable = || {
            AgentCompositionError::InvalidInput(
                "ACP Session inputs must be prepared again by its client".into(),
            )
        };
        if self.stop.is_cancelled() {
            return Err(AgentCompositionError::ShuttingDown);
        }
        let root = header
            .fork_origin()
            .map_or(header.session_id(), |origin| &origin.root_session_id);
        let state = self.state.lock().expect("private ACP inputs");
        let binding = state.entries.get(root).ok_or_else(unavailable)?;
        if header.canonical_cwd() != binding.cwd || header.agent_preset_id().as_str() != PRESET {
            return Err(unavailable());
        }
        if !matches_seed(binding, seed) {
            return Err(unavailable());
        }
        Ok(binding.pin.clone())
    }

    pub(crate) async fn prepare(
        self: &Arc<Self>,
        id: SessionId,
        cwd: String,
        servers: Vec<schema::McpServer>,
        seed: Option<AgentGenerationSeed>,
    ) -> Result<(), Failure> {
        let request_sha256: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&(&cwd, &servers)).map_err(|_| Failure::Parameters)?)
                .into();
        let cancellation = self.stop.child_token();
        let cancel_on_drop = cancellation.clone().drop_guard();
        let (response, receive) = tokio::sync::oneshot::channel();
        let (accepted, acknowledgment) = tokio::sync::oneshot::channel();
        {
            let mut state = self.state.lock().expect("private ACP inputs");
            if self.stop.is_cancelled() {
                return Err(Failure::Backend);
            }
            if state.pending.contains_key(&id) {
                return Err(Failure::Busy);
            }
            if let Some(binding) = state.entries.get(&id) {
                return if binding.cwd == cwd
                    && binding.request_sha256 == request_sha256
                    && matches_seed(binding, seed.as_ref())
                {
                    Ok(())
                } else {
                    Err(Failure::Parameters)
                };
            }
            if state
                .entries
                .keys()
                .chain(state.pending.keys())
                .collect::<BTreeSet<_>>()
                .len()
                >= 256
            {
                return Err(Failure::Busy);
            }
            let permit = self
                .builds
                .clone()
                .try_acquire_owned()
                .map_err(|_| Failure::Busy)?;
            state.pending.insert(id.clone(), cancellation.clone());
            let id = id.clone();
            let owner = self.clone();
            self.tasks.spawn(async move {
                let result = owner
                    .build(
                        &id,
                        cwd,
                        servers,
                        seed.as_ref(),
                        request_sha256,
                        cancellation.clone(),
                    )
                    .await;
                owner
                    .publish(&id, result, cancellation, response, acknowledgment)
                    .await;
                drop(permit);
            });
        }
        receive.await.map_err(|_| Failure::Backend)??;
        {
            let mut state = self.state.lock().expect("private ACP inputs");
            accepted.send(()).map_err(|()| Failure::Backend)?;
            state.pending.remove(&id);
            self.changed.notify_waiters();
        }
        cancel_on_drop.disarm();
        Ok(())
    }

    async fn publish(
        &self,
        id: &SessionId,
        result: Result<Binding, Failure>,
        cancellation: CancellationToken,
        response: tokio::sync::oneshot::Sender<Result<(), Failure>>,
        acknowledgment: tokio::sync::oneshot::Receiver<()>,
    ) {
        let result = match result {
            Ok(binding) if !cancellation.is_cancelled() => {
                self.state
                    .lock()
                    .expect("private ACP inputs")
                    .entries
                    .insert(id.clone(), binding);
                Ok(())
            }
            Ok(binding) => {
                let _cleanup = retire(binding).await;
                Err(Failure::Backend)
            }
            Err(error) => Err(error),
        };
        let published = result.is_ok();
        let delivered = response.send(result).is_ok();
        let acknowledged = delivered && published && acknowledgment.await.is_ok();
        if published && !acknowledged {
            let binding = self
                .state
                .lock()
                .expect("private ACP inputs")
                .entries
                .remove(id);
            if let Some(binding) = binding {
                let _cleanup = retire(binding).await;
            }
        }
        if !acknowledged {
            self.state
                .lock()
                .expect("private ACP inputs")
                .pending
                .remove(id);
            self.changed.notify_waiters();
        }
    }

    async fn build(
        &self,
        id: &SessionId,
        cwd: String,
        servers: Vec<schema::McpServer>,
        seed: Option<&AgentGenerationSeed>,
        request_sha256: [u8; 32],
        cancellation: CancellationToken,
    ) -> Result<Binding, Failure> {
        let (config, credentials) = configuration(id, &cwd, servers)?;
        let mcp = Arc::new(rsi_mcp::McpService::new_with_all_discovered_tools(
            Arc::new(credentials),
            self.process.clone(),
            self.sandbox.clone(),
        ));
        let result = async {
            let ids = config
                .servers
                .iter()
                .map(|server| server.id.clone())
                .collect::<Vec<_>>();
            mcp.configure(config).await.map_err(|_| Failure::Backend)?;
            for id in ids {
                mcp.refresh(&id, cancellation.clone())
                    .await
                    .map_err(|_| Failure::Backend)?;
            }
            let manifest = mcp
                .manifest()
                .map_err(|_| Failure::Backend)?
                .snapshot()
                .map_err(|_| Failure::Backend)?;
            if let Some(seed) = seed
                && seed.find(manifest.identity()) != Some(&manifest)
            {
                return Err(Failure::Parameters);
            }
            self.compose(&mcp, manifest, seed).await
        }
        .await;
        match result {
            Ok((pin, fiber)) => Ok(Binding {
                cwd,
                request_sha256,
                pin,
                fiber,
                mcp,
            }),
            Err(error) => {
                mcp.shutdown().await.map_err(|_| Failure::Backend)?;
                Err(error)
            }
        }
    }
    async fn compose(
        &self,
        mcp: &Arc<rsi_mcp::McpService>,
        manifest: rsi_agent_session_protocol::DomainSnapshot,
        seed: Option<&AgentGenerationSeed>,
    ) -> Result<(AgentCompositionPin, FiberHandle), Failure> {
        let source = self.source.private_snapshot(manifest)?;
        let root = crate::standard_agent_preset_root(&self.paths).map_err(|_| Failure::Backend)?;
        let preset = AgentPresetId::new(PRESET).expect("private preset identity");
        let presets = source
            .presets()
            .with_config(
                AgentPresetCatalogConfig::new(preset.clone())
                    .with_system_preset(preset.clone(), root.join(PRESET)),
            )
            .map_err(|_| Failure::Backend)?;
        let snapshot = source
            .derive_private(
                presets,
                [ResolvedFactory::linked(
                    "rsi.mcp.tools",
                    "acp-private-1",
                    UpdateMode::RestartRequired,
                    Arc::new(rsi_mcp::McpToolsFactory::with_service(mcp.clone())),
                )],
                source
                    .generation_seed()
                    .map_err(|_| Failure::Backend)?
                    .clone(),
            )
            .map_err(|_| Failure::Backend)?;
        let (context, _) = self
            .context
            .clone()
            .isolate_local_fresh::<AgentCompositionContract>()
            .map_err(|_| Failure::Backend)?;
        let fiber = context
            .apply(
                ResolvedFactory::linked(
                    "rsi.acp.private-composition",
                    "1",
                    UpdateMode::RestartRequired,
                    Arc::new(AgentCompositionFactory::with_source(
                        Arc::new(snapshot),
                        rsi_meta_scope::ScopeRoot::new(128).map_err(|_| Failure::Backend)?,
                    )),
                ),
                serde_json::Value::Null,
            )
            .await
            .map_err(|_| Failure::Backend)?;
        let pin = match context.lookup_local::<AgentCompositionContract>() {
            Some(composition) => composition
                .pin(&preset, seed)
                .await
                .map_err(|_| Failure::Backend),
            None => Err(Failure::Backend),
        };
        match pin {
            Ok(pin) => Ok((pin, fiber)),
            Err(error) => {
                let _cleanup = fiber.dispose().await;
                Err(error)
            }
        }
    }
    pub(crate) async fn close(self: &Arc<Self>, id: &SessionId) -> Result<(), Failure> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let pending = {
                let state = self.state.lock().expect("private ACP inputs");
                if let Some(pending) = state.pending.get(id) {
                    pending.cancel();
                    true
                } else {
                    false
                }
            };
            if !pending {
                break;
            }
            changed.await;
        }
        let receive = {
            let mut state = self.state.lock().expect("private ACP inputs");
            if self.stop.is_cancelled() {
                return Err(Failure::Backend);
            }
            if state.pending.contains_key(id) {
                return Err(Failure::Busy);
            }
            let Some(binding) = state.entries.remove(id) else {
                return Ok(());
            };
            state.pending.insert(id.clone(), CancellationToken::new());
            let owner = self.clone();
            let id = id.clone();
            let (send, receive) = tokio::sync::oneshot::channel();
            self.tasks.spawn(async move {
                let result = retire(binding).await;
                owner
                    .state
                    .lock()
                    .expect("private ACP inputs")
                    .pending
                    .remove(&id);
                owner.changed.notify_waiters();
                let _ = send.send(result);
            });
            receive
        };
        receive.await.map_err(|_| Failure::Backend)?
    }
    pub(crate) async fn shutdown(&self) -> Result<(), Failure> {
        {
            let _state = self.state.lock().expect("private ACP inputs");
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
        let entries = std::mem::take(&mut self.state.lock().expect("private ACP inputs").entries);
        let mut clean = true;
        for binding in entries.into_values() {
            clean &= retire(binding).await.is_ok();
        }
        if clean { Ok(()) } else { Err(Failure::Backend) }
    }
}
fn matches_seed(binding: &Binding, seed: Option<&AgentGenerationSeed>) -> bool {
    seed.is_none_or(|seed| {
        binding
            .pin
            .domains()
            .baseline()
            .iter()
            .filter(|domain| {
                matches!(
                    domain.identity().id(),
                    "rsi.mcp.manifest" | "rsi.tools.outputs"
                )
            })
            .all(|domain| seed.find(domain.identity()) == Some(domain))
    })
}
async fn retire(binding: Binding) -> Result<(), Failure> {
    let Binding {
        pin, fiber, mcp, ..
    } = binding;
    drop(pin);
    let outcome = fiber.dispose().await;
    let settled = mcp.shutdown().await;
    if outcome.is_clean() && settled.is_ok() {
        Ok(())
    } else {
        Err(Failure::Backend)
    }
}

struct Secrets(BTreeMap<String, SecretValue>);
impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AcpEphemeralCredentials")
    }
}
#[async_trait::async_trait]
impl CredentialsResolve for Secrets {
    async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> rsi_credentials_protocol::Result<ResolvedCredential> {
        if reference.owner.as_str() != rsi_mcp::CREDENTIAL_OWNER {
            return Err(CredentialsError::NotConfigured(
                "ACP session credential".into(),
            ));
        }
        let secret = self
            .0
            .get(&reference.slot)
            .cloned()
            .ok_or_else(|| CredentialsError::NotConfigured("ACP session credential".into()))?;
        Ok(ResolvedCredential {
            secret,
            source: CredentialSource::Environment {
                variable: "ACP_SESSION_ENV".into(),
            },
        })
    }
}
fn configuration(
    id: &SessionId,
    cwd: &str,
    servers: Vec<schema::McpServer>,
) -> Result<(rsi_mcp::McpConfig, Secrets), Failure> {
    rsi_acp_protocol::validate_session_setup(
        &serde_json::json!({"cwd":cwd,"mcpServers":servers}),
        false,
    )
    .map_err(|_| Failure::Parameters)?;
    let mut secrets = BTreeMap::new();
    let mut config = rsi_mcp::McpConfig::default();
    for server in servers {
        let schema::McpServer::Stdio(server) = server else {
            return Err(Failure::Parameters);
        };
        let mut environment = BTreeMap::new();
        for entry in server.env {
            if entry.value.is_empty() {
                environment.insert(
                    entry.name,
                    rsi_mcp::EnvironmentValue::Literal {
                        value: String::new(),
                    },
                );
                continue;
            }
            let digest = hex::encode(Sha256::digest(
                format!("{}\0{}\0{}", id.as_str(), server.name, entry.name).as_bytes(),
            ));
            let slot = format!("acp-{digest}");
            secrets.insert(
                slot.clone(),
                SecretValue::new(entry.value).map_err(|_| Failure::Parameters)?,
            );
            environment.insert(
                entry.name,
                rsi_mcp::EnvironmentValue::Credential {
                    reference: CredentialRef::new(rsi_mcp::CREDENTIAL_OWNER, slot)
                        .map_err(|_| Failure::Parameters)?,
                },
            );
        }
        config.servers.push(rsi_mcp::ServerConfig {
            id: server.name,
            enabled: true,
            tools: vec![],
            transport: rsi_mcp::TransportConfig::Stdio {
                program: server.command,
                arguments: server.args,
                cwd: cwd.into(),
                environment,
            },
        });
    }
    config.validate().map_err(|_| Failure::Parameters)?;
    Ok((config, Secrets(secrets)))
}
