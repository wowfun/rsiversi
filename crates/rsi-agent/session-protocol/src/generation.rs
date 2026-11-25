//! Diagnostic identity of current executable inputs, independent of code retention.
use crate::{AgentPresetId, Result, validate_sha256};
use serde::{Deserialize, Serialize};

/// Digest of the evaluated configuration and executable catalog. This is not a CAS address.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSelection {
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<AgentPresetId>,
}
impl GenerationSelection {
    /// Validates the diagnostic digest at its owning boundary.
    pub fn new(sha256: impl Into<String>) -> Result<Self> {
        let sha256 = sha256.into();
        validate_sha256("generation selection", &sha256)?;
        Ok(Self {
            sha256,
            preset: None,
        })
    }
    /// Stable diagnostic identity, independent of process-local pin ownership.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    /// Logical preset selected after creation, when it differs from the Header.
    pub fn preset(&self) -> Option<&AgentPresetId> {
        self.preset.as_ref()
    }

    /// Records the new owner-selected preset alongside its executable digest.
    pub fn with_preset(mut self, preset: Option<AgentPresetId>) -> Self {
        self.preset = preset;
        self
    }
}
impl<'de> Deserialize<'de> for GenerationSelection {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            sha256: String,
            preset: Option<AgentPresetId>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.sha256)
            .map(|selection| selection.with_preset(wire.preset))
            .map_err(serde::de::Error::custom)
    }
}
