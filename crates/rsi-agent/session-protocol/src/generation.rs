//! Diagnostic identity of current executable inputs, independent of code retention.
use crate::{Result, validate_sha256};
use serde::{Deserialize, Serialize};

/// Digest of the evaluated configuration and executable catalog. This is not a CAS address.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSelection {
    sha256: String,
}
impl GenerationSelection {
    /// Validates the diagnostic digest at its owning boundary.
    pub fn new(sha256: impl Into<String>) -> Result<Self> {
        let sha256 = sha256.into();
        validate_sha256("generation selection", &sha256)?;
        Ok(Self { sha256 })
    }
    /// Stable diagnostic identity, independent of process-local pin ownership.
    pub fn sha256(&self) -> &str {
        &self.sha256
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
        }
        Self::new(Wire::deserialize(deserializer)?.sha256).map_err(serde::de::Error::custom)
    }
}
