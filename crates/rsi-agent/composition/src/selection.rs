//! Current executable identity; no reconstruction payload is retained.
use crate::AgentCompositionSnapshot;
use rsi_agent_composition_protocol::{AgentCompositionError, Result};
use rsi_agent_session_protocol::GenerationSelection;
use sha2::{Digest, Sha256};
pub(super) fn identify(
    snapshot: &AgentCompositionSnapshot,
    profile_digest: &str,
) -> Result<GenerationSelection> {
    let mut digest = Sha256::new();
    digest.update(b"rsi-agent-selection-v1\0");
    crate::catalog::hash_field(
        &mut digest,
        snapshot.contributions.durable_digest().as_bytes(),
    );
    crate::catalog::hash_field(&mut digest, &snapshot.code);
    crate::catalog::hash_field(&mut digest, profile_digest.as_bytes());
    GenerationSelection::new(format!("{:x}", digest.finalize()))
        .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))
}
