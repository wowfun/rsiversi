//! Candidate metadata from an explicit provider listing, independent of live routes.
use serde::{Deserialize, Serialize};

pub const MAX_DISCOVERED_MODELS: usize = 4096;
pub const MAX_DISCOVERY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: Option<String>,
    pub context_window_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
}
impl DiscoveredModel {
    pub fn validate(&self) -> Result<(), &'static str> {
        if crate::ModelRef::new("discovery", &self.id).is_err()
            || self
                .name
                .as_ref()
                .is_some_and(|name| name.is_empty() || name.len() > 256)
            || self.context_window_tokens == Some(0)
            || self.max_output_tokens == Some(0)
            || self
                .context_window_tokens
                .zip(self.max_output_tokens)
                .is_some_and(|(context, output)| output > context)
        {
            return Err("invalid discovered model metadata");
        }
        Ok(())
    }
}
pub fn validate_discovered_models(models: &[DiscoveredModel]) -> Result<(), &'static str> {
    if models.len() > MAX_DISCOVERED_MODELS {
        return Err("model discovery exceeds 4096 candidates");
    }
    let mut names = std::collections::BTreeSet::new();
    for model in models {
        model.validate()?;
        if !names.insert(&model.id) {
            return Err("duplicate discovered model identifier");
        }
    }
    Ok(())
}
