use crate::AgentContributionCatalog;
use rsi_agent_presets::AgentPresetCatalog;
use sha2::{Digest, Sha256};
use std::{fmt, sync::Arc};

/// Immutable compiler/allowlist and executable catalog selected for one build.
#[derive(Clone)]
pub struct AgentCompositionSnapshot {
    pub(crate) seeds: Result<rsi_agent_composition_protocol::AgentGenerationSeed, &'static str>,
    pub(crate) presets: AgentPresetCatalog,
    pub(crate) contributions: Arc<AgentContributionCatalog>,
}

impl fmt::Debug for AgentCompositionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentCompositionSnapshot")
            .field("contributions", &self.contributions)
            .finish_non_exhaustive()
    }
}

impl AgentCompositionSnapshot {
    /// Freezes application-selected preset and contribution authority together.
    pub fn new(presets: AgentPresetCatalog, contributions: AgentContributionCatalog) -> Self {
        Self {
            seeds: Ok(rsi_agent_composition_protocol::AgentGenerationSeed::default()),
            presets,
            contributions: Arc::new(contributions),
        }
    }
    /// Borrows current opaque inputs so product source decorators can preserve other owners.
    ///
    /// # Errors
    /// Returns the source owner's static, redacted reason when fresh inputs are unavailable.
    pub fn generation_seed(
        &self,
    ) -> Result<&rsi_agent_composition_protocol::AgentGenerationSeed, &'static str> {
        self.seeds.as_ref().map_err(|reason| *reason)
    }
    /// Captures validated current Domain inputs together with this executable catalog.
    #[must_use]
    pub fn with_generation_seed(
        mut self,
        seed: rsi_agent_composition_protocol::AgentGenerationSeed,
    ) -> Self {
        self.seeds = Ok(seed);
        self
    }

    /// Declares that fresh inputs are unavailable while keeping the executable catalog
    /// available for restoration with a saved seed. This never enables a fresh cache hit.
    #[must_use]
    pub fn with_unavailable_generation_seed(mut self, reason: &'static str) -> Self {
        self.seeds = Err(reason);
        self
    }
}

/// Application-owned selection of the complete next-build catalog snapshot.
///
/// Implementations return a previously staged immutable value without executing
/// plugins or loading artifacts here. Failed refresh/admission must be reported;
/// the composition provider never falls back to its cached generation on error.
/// Existing pins remain valid independently of subsequent source publication.
pub trait AgentCompositionSource: fmt::Debug + Send + Sync + 'static {
    /// Captures one compiler/catalog pair for the entire build.
    ///
    /// # Errors
    /// Returns an error when selection or native admission is unavailable.
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>>;
}

/// Ordinary Local supply of the application-owned staged Agent source.
#[derive(Debug)]
pub struct AgentCompositionSourceContract;
impl rsi_meta::LocalContract for AgentCompositionSourceContract {
    const KEY: &'static str = "rsi.agent.composition-source";
    type Service = dyn AgentCompositionSource;
}

impl AgentCompositionSource for AgentCompositionSnapshot {
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>> {
        Ok(Arc::new(self.clone()))
    }
}

pub(crate) struct GenerationIdentity {
    program_digest: String,
    seed_sha256: [u8; 32],
    restoring: bool,
    pub(crate) catalog: Arc<AgentContributionCatalog>,
    pub(crate) effective_digest: String,
}

impl GenerationIdentity {
    pub(crate) const fn cache_slot(&self) -> usize {
        self.restoring as usize
    }

    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.program_digest == other.program_digest && self.catalog.same_identity(&other.catalog)
    }

    pub(crate) fn new(
        program_digest: &str,
        catalog: Arc<AgentContributionCatalog>,
        inputs: &rsi_agent_composition_protocol::AgentGenerationInputs,
    ) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"rsi.agent.generation.v3");
        crate::catalog::hash_field(&mut digest, program_digest.as_bytes());
        crate::catalog::hash_field(&mut digest, catalog.digest().as_bytes());
        digest.update([u8::from(inputs.restoring)]);
        crate::catalog::hash_field(&mut digest, inputs.seed.sha256());
        Self {
            seed_sha256: *inputs.seed.sha256(),
            restoring: inputs.restoring,
            program_digest: program_digest.to_owned(),
            catalog,
            effective_digest: format!("{:x}", digest.finalize()),
        }
    }

    pub(crate) fn matches(
        &self,
        program_digest: &str,
        catalog: &AgentContributionCatalog,
        inputs: &rsi_agent_composition_protocol::AgentGenerationInputs,
    ) -> bool {
        self.program_digest == program_digest
            && self.catalog.same_identity(catalog)
            && self.seed_sha256 == *inputs.seed.sha256()
            && self.restoring == inputs.restoring
    }
}
