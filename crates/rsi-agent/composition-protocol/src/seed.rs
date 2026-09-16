//! Opaque, bounded pre-activation Domain inputs; semantic decoding stays with owners.
use crate::{AgentCompositionError, Result};
use rsi_agent_session_protocol::{
    DomainIdentity, DomainSnapshot, MAXIMUM_DOMAIN_BASELINE_BYTES, MAXIMUM_SESSION_DOMAINS,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Structurally validated complete Domain values supplied before catalog sealing.
#[derive(Clone, Debug)]
pub struct AgentGenerationSeed(Arc<Seed>);
#[derive(Debug)]
struct Seed {
    states: Vec<DomainSnapshot>,
    sha256: [u8; 32],
}
struct HashWriter(Sha256);
impl std::io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Default for AgentGenerationSeed {
    fn default() -> Self {
        Self::new(vec![]).expect("empty generation seed")
    }
}
impl PartialEq for AgentGenerationSeed {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.sha256 == other.0.sha256
    }
}
impl Eq for AgentGenerationSeed {}
impl AgentGenerationSeed {
    /// Admits sorted unique values under existing Domain and baseline bounds.
    ///
    /// # Errors
    /// Rejects duplicate or unsorted identities and complete baseline count/byte overflow.
    pub fn new(states: Vec<DomainSnapshot>) -> Result<Self> {
        if states.len() > MAXIMUM_SESSION_DOMAINS
            || states
                .windows(2)
                .any(|pair| pair[0].identity().id() >= pair[1].identity().id())
        {
            return Err(AgentCompositionError::InvalidInput(
                "Generation seed requires at most 64 sorted unique domains".into(),
            ));
        }
        let mut bytes = 0usize;
        for state in &states {
            bytes = bytes
                .checked_add(
                    state
                        .encoded_len()
                        .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))?,
                )
                .ok_or_else(|| {
                    AgentCompositionError::InvalidInput("Generation seed size overflow".into())
                })?;
            if bytes > MAXIMUM_DOMAIN_BASELINE_BYTES {
                return Err(AgentCompositionError::InvalidInput(
                    "Generation seed exceeds the 1 MiB baseline limit".into(),
                ));
            }
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, &states)
            .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))?;
        Ok(Self(Arc::new(Seed {
            states,
            sha256: writer.0.finalize().into(),
        })))
    }
    /// Cached canonical JSON identity; no re-encoding or allocation.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.0.sha256
    }
    /// Exact bounded snapshots; no provider semantics are interpreted here.
    pub fn states(&self) -> &[DomainSnapshot] {
        &self.0.states
    }
    /// Selects one exact codec identity. The plugin must decode and validate it.
    pub fn find(&self, identity: &DomainIdentity) -> Option<&DomainSnapshot> {
        self.0
            .states
            .iter()
            .find(|state| state.identity() == identity)
    }
}
/// Immutable generation-local inputs visible to declaring plugins before sealing.
#[derive(Clone, Debug)]
pub struct AgentGenerationInputs {
    /// Selected complete Domain values.
    pub seed: AgentGenerationSeed,
    /// This build restores a saved baseline and must not substitute current data.
    pub restoring: bool,
}
/// Nominal private-generation input capability.
#[derive(Debug)]
pub struct AgentGenerationInputsContract;
impl rsi_meta::LocalContract for AgentGenerationInputsContract {
    const KEY: &'static str = "rsi.agent.generation-inputs";
    type Service = AgentGenerationInputs;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::DomainStateValue;
    fn state(name: &str, value: serde_json::Value) -> DomainSnapshot {
        DomainSnapshot::new(
            DomainIdentity::new(name, 1).unwrap(),
            DomainStateValue::new(value).unwrap(),
        )
    }
    #[test]
    fn immutable_seed_identity_hashes_exact_wire_values_once() {
        let first = AgentGenerationSeed::new(vec![state(
            "exact",
            serde_json::from_str(r#"{"n":1.0000000000000001,"m":2}"#).unwrap(),
        )])
        .unwrap();
        assert_eq!(
            *first.sha256(),
            <[u8; 32]>::from(Sha256::digest(serde_json::to_vec(first.states()).unwrap()))
        );
        let clone = first.clone();
        assert!(Arc::ptr_eq(&first.0, &clone.0));
        assert_eq!(first, clone);
        let same = AgentGenerationSeed::new(first.states().to_vec()).unwrap();
        assert_eq!(first, same);
        for text in [r#"{"n":1,"m":2}"#, r#"{"m":2,"n":1.0000000000000001}"#] {
            assert_ne!(
                first,
                AgentGenerationSeed::new(vec![state("exact", serde_json::from_str(text).unwrap())])
                    .unwrap()
            );
        }
    }
    #[test]
    fn seed_enforces_the_existing_complete_baseline_budget_and_exact_numbers() {
        assert!(
            AgentGenerationSeed::new(vec![
                state("same", true.into()),
                state("same", false.into())
            ])
            .is_err()
        );
        assert!(
            AgentGenerationSeed::new(vec![state("z", true.into()), state("a", false.into())])
                .is_err()
        );
        let states = (0..5)
            .map(|i| state(&format!("domain-{i}"), "x".repeat(250 * 1024).into()))
            .collect();
        assert!(AgentGenerationSeed::new(states).is_err());
        let exact: serde_json::Value = serde_json::from_str("18446744073709551615").unwrap();
        let seed = AgentGenerationSeed::new(vec![state("exact", exact)]).unwrap();
        assert_eq!(
            seed.states()[0].state().value().to_string(),
            "18446744073709551615"
        );
    }
}
