//! Original application message identity without execution authority.

use crate::{Result, SessionError};
use serde::{Deserialize, Serialize};

/// Caller-supplied presentation identity, independent of native execution provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MessageAttribution {
    /// Original message identity.
    pub id: String,
    /// Original application metadata; never an authority credential.
    pub source: serde_json::Value,
}
impl<'de> Deserialize<'de> for MessageAttribution {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: String,
            source: serde_json::Value,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            id: wire.id,
            source: wire.source,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
impl MessageAttribution {
    /// Validates attribution at the input and durable boundaries.
    pub fn validate(&self) -> Result<()> {
        crate::validate_safe_text("message attribution ID", &self.id, 256, false)?;
        rsi_ai_protocol::validate_json_structure(&self.source)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        crate::bounded_compact_json_len(self, 16 * 1024)
            .map_err(|_| SessionError::Invalid("message attribution exceeds 16 KiB".into()))?;
        Ok(())
    }
}
