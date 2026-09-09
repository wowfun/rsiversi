use crate::AgentContributionCatalog;
use rsi_agent_presets::AgentPresetCatalog;
use sha2::{Digest, Sha256};
use std::{fmt, sync::Arc};

/// Immutable compiler/allowlist and executable catalog selected for one build.
#[derive(Clone)]
pub struct AgentCompositionSnapshot {
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
            presets,
            contributions: Arc::new(contributions),
        }
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

impl AgentCompositionSource for AgentCompositionSnapshot {
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>> {
        Ok(Arc::new(self.clone()))
    }
}

pub(crate) struct GenerationIdentity {
    program_digest: String,
    pub(crate) catalog: Arc<AgentContributionCatalog>,
    pub(crate) effective_digest: String,
}

impl GenerationIdentity {
    pub(crate) fn new(program_digest: &str, catalog: Arc<AgentContributionCatalog>) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"rsi.agent.generation.v1");
        crate::catalog::hash_field(&mut digest, program_digest.as_bytes());
        crate::catalog::hash_field(&mut digest, catalog.digest().as_bytes());
        Self {
            program_digest: program_digest.to_owned(),
            catalog,
            effective_digest: format!("{:x}", digest.finalize()),
        }
    }

    pub(crate) fn matches(&self, program_digest: &str, catalog: &AgentContributionCatalog) -> bool {
        self.program_digest == program_digest && self.catalog.same_identity(catalog)
    }
}
