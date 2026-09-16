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
        }
    }
}
impl AgentCompositionSource for SeededSource {
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
#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct SourceFactory(pub(crate) Arc<AgentCompositionSnapshot>);
#[cfg(not(unix))]
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for SourceFactory {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(config.clone())
            .requiring_local::<rsi_mcp::McpOwnerContract>()
            .requiring_local::<rsi_retrieval::RetrievalContract>())
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_agent_composition::AgentCompositionSourceContract>(Arc::new(
                SeededSource::new(
                    self.0.clone(),
                    plan.local::<rsi_mcp::McpOwnerContract>()?,
                    plan.local::<rsi_retrieval::RetrievalContract>()?,
                ),
            ))?;
        Ok(())
    }
}
