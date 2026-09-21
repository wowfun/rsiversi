//! Product-owned composition wiring; generic Agent and native Loader consume opaque seeds.
use rsi_agent_composition::{AgentCompositionSnapshot, AgentCompositionSource};
use rsi_agent_composition_protocol::AgentGenerationSeed;
use std::sync::{Arc, Mutex};
#[derive(Debug)]
pub(crate) struct SeededSource {
    source: Arc<dyn AgentCompositionSource>,
    mcp: Arc<rsi_mcp::McpOwner>,
    retrieval: Arc<rsi_retrieval::RetrievalService>,
    cached: Mutex<Option<SeedCache>>,
    private: std::sync::OnceLock<std::sync::Weak<crate::acp_inputs::PrivateInputs>>,
}
#[derive(Debug)]
struct SeedCache {
    base: [u8; 32],
    mcp: [u8; 32],
    retrieval: rsi_retrieval::RetrievalConfig,
    merged: AgentGenerationSeed,
}
impl SeededSource {
    pub(crate) fn new(
        source: Arc<dyn AgentCompositionSource>,
        mcp: Arc<rsi_mcp::McpOwner>,
        retrieval: Arc<rsi_retrieval::RetrievalService>,
    ) -> Self {
        Self {
            source,
            mcp,
            retrieval,
            cached: Mutex::new(None),
            private: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn private_snapshot(
        &self,
        mcp: rsi_agent_session_protocol::DomainSnapshot,
    ) -> Result<Arc<AgentCompositionSnapshot>, rsi_acp::server::Failure> {
        use rsi_acp::server::Failure;
        let source = self.source.snapshot().map_err(|_| Failure::Backend)?;
        let mut states = source
            .generation_seed()
            .map_err(|_| Failure::Backend)?
            .states()
            .to_vec();
        states.push(mcp);
        states.push(
            self.retrieval
                .config()
                .map_err(|_| Failure::Backend)?
                .snapshot(),
        );
        states.sort_by(|left, right| left.identity().id().cmp(right.identity().id()));
        let seed = AgentGenerationSeed::new(states).map_err(|_| Failure::Backend)?;
        Ok(Arc::new(source.as_ref().clone().with_generation_seed(seed)))
    }
}
impl AgentCompositionSource for SeededSource {
    fn session_pin(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
        seed: Option<&AgentGenerationSeed>,
    ) -> rsi_agent_composition_protocol::Result<
        Option<rsi_agent_composition_protocol::AgentCompositionPin>,
    > {
        if header.agent_preset_id().as_str() == crate::acp_inputs::PRESET {
            return self
                .private
                .get()
                .and_then(std::sync::Weak::upgrade)
                .ok_or_else(|| {
                    rsi_agent_composition_protocol::AgentCompositionError::InvalidInput(
                        "ACP Session inputs are unavailable".into(),
                    )
                })?
                .pin(header, seed)
                .map(Some);
        }
        self.source.session_pin(header, seed)
    }
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>> {
        let source = self.source.snapshot()?;
        let seed = source.generation_seed().and_then(|base| {
            let mcp = self.mcp.seed().map_err(|error| match error {
                rsi_mcp::McpError::Busy => {
                    "MCP configuration is changing; retry after refresh completes"
                }
                rsi_mcp::McpError::CredentialUnavailable => {
                    "MCP credentials are unavailable; configure them in Plugins"
                }
                rsi_mcp::McpError::Timeout => {
                    "MCP discovery timed out; refresh the server in Plugins"
                }
                rsi_mcp::McpError::Capacity => {
                    "MCP catalog exceeds composition limits; reduce selected Tools in Plugins"
                }
                _ => "MCP inputs are unavailable; inspect and refresh the server in Plugins",
            })?;
            let retrieval = self
                .retrieval
                .config()
                .map_err(|_| "Web retrieval settings are unavailable; repair them in Settings")?;
            let mut cached = self.cached.lock().expect("integration seed poisoned");
            if let Some(cached) = cached.as_ref()
                && cached.base == *base.sha256()
                && cached.mcp == *mcp.sha256()
                && cached.retrieval == retrieval
            {
                return Ok(cached.merged.clone());
            }
            let mut states = base.states().to_vec();
            states.extend_from_slice(mcp.states());
            states.push(retrieval.snapshot());
            states.sort_by(|left, right| left.identity().id().cmp(right.identity().id()));
            let merged = AgentGenerationSeed::new(states)
                .map_err(|_| "Combined generation inputs exceed composition limits")?;
            *cached = Some(SeedCache {
                base: *base.sha256(),
                mcp: *mcp.sha256(),
                retrieval,
                merged: merged.clone(),
            });
            Ok(merged)
        });
        Ok(Arc::new(match seed {
            Ok(seed) => source.as_ref().clone().with_generation_seed(seed),
            Err(reason) => source
                .as_ref()
                .clone()
                .with_unavailable_generation_seed(reason),
        }))
    }
}

pub(crate) fn requirements(prepared: rsi_meta::PreparedActivation) -> rsi_meta::PreparedActivation {
    prepared
        .requiring_local::<rsi_mcp::McpOwnerContract>()
        .requiring_local::<rsi_retrieval::RetrievalContract>()
        .requiring_local::<rsi_process::DuplexProcessContract>()
        .requiring_local::<rsi_sandbox::SandboxContract>()
        .requiring_local::<rsi_tools_protocol::ToolCatalogProviderContract>()
        .requiring_local::<rsi_agent_composition::AgentGenerationRootContract>()
}
pub(crate) fn capture(
    plan: &rsi_meta::ActivationPlan,
    base: Arc<dyn AgentCompositionSource>,
    paths: rsi_host::HostPaths,
) -> rsi_meta::Result<Arc<dyn AgentCompositionSource>> {
    let source = Arc::new(SeededSource::new(
        base,
        plan.local::<rsi_mcp::McpOwnerContract>()?,
        plan.local::<rsi_retrieval::RetrievalContract>()?,
    ));
    let private = crate::acp_inputs::PrivateInputs::new(
        source.clone(),
        plan.context().clone(),
        paths,
        plan.local::<rsi_process::DuplexProcessContract>()?,
        plan.local::<rsi_sandbox::SandboxContract>()?,
    );
    source
        .private
        .set(Arc::downgrade(&private))
        .expect("private source initialized once");
    plan.context()
        .provide_local::<crate::acp_inputs::InputsContract>(private.clone())?;
    plan.defer(
        "retire private ACP inputs",
        Box::new(move || {
            Box::pin(async move {
                private
                    .shutdown()
                    .await
                    .map_err(|_| "private ACP input cleanup failed".into())
            })
        }),
    )?;
    Ok(source)
}
#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct SourceFactory(
    pub(crate) Arc<AgentCompositionSnapshot>,
    pub(crate) rsi_host::HostPaths,
);
#[cfg(not(unix))]
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for SourceFactory {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(requirements(rsi_meta::PreparedActivation::new(
            config.clone(),
        )))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_agent_composition::AgentCompositionSourceContract>(capture(
                &plan,
                self.0.clone(),
                self.1.clone(),
            )?)?;
        Ok(())
    }
}
