//! Historical output interpretation from a saved composition baseline.

use crate::{AgentCompositionError, DomainError, Result};
use rsi_agent_session_protocol::{DomainIdentity, DomainSnapshot};
use rsi_tools_protocol::{
    MAXIMUM_REGISTERED_TOOLS, MAXIMUM_TOOL_OUTPUT_CATALOG_BYTES, ToolOutputDeclaration,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Exact-name output declarations frozen when the generation was composed.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ToolOutputCatalog(BTreeMap<String, ToolOutputDeclaration>);

impl<'de> Deserialize<'de> for ToolOutputCatalog {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::new(BTreeMap::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl ToolOutputCatalog {
    /// Durable output catalog identity; distinct from the individual tool versions.
    ///
    /// # Panics
    /// Panics only if the fixed built-in domain identity becomes invalid.
    pub fn identity() -> DomainIdentity {
        DomainIdentity::new("rsi.tools.outputs", 1).expect("fixed domain identity")
    }

    /// Validates a complete snapshot from the sealed Tool runtime.
    ///
    /// # Errors
    /// Rejects invalid names and aggregate count or encoded-byte overflow.
    pub fn new(declarations: BTreeMap<String, ToolOutputDeclaration>) -> Result<Self> {
        let result = Self(declarations);
        result
            .validate()
            .map_err(AgentCompositionError::InvalidInput)?;
        Ok(result)
    }

    /// Reads only the supplied baseline. Absence denotes legacy untyped history.
    ///
    /// # Errors
    /// Rejects unsupported domain codecs and invalid saved output declarations.
    pub fn from_baseline(baseline: &[DomainSnapshot]) -> Result<Option<Self>> {
        let identity = Self::identity();
        let Some(snapshot) = baseline
            .iter()
            .find(|state| state.identity().id() == identity.id())
        else {
            return Ok(None);
        };
        if snapshot.identity() != &identity {
            return Err(AgentCompositionError::UnsupportedSeedCodec {
                stored: snapshot.identity().clone(),
                expected: identity,
            });
        }
        let catalog: Self =
            serde_json::from_value(snapshot.state().value().clone()).map_err(|_| {
                DomainError::InvalidState {
                    domain: identity.id().into(),
                    reason: "invalid saved tool output declarations".into(),
                }
            })?;
        Ok(Some(catalog))
    }

    /// Looks up the exact `ToolIntent` name; no provider or current registry is consulted.
    pub fn get(&self, name: &str) -> Option<&ToolOutputDeclaration> {
        self.0.get(name)
    }

    /// Whether this generation has any typed outputs.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Bounded, name-ordered declarations for read-only consumers.
    pub fn declarations(&self) -> &BTreeMap<String, ToolOutputDeclaration> {
        &self.0
    }

    /// Effect-free semantic validator for the immutable Domain definition.
    ///
    /// # Errors
    /// Rejects invalid names and aggregate count or encoded-byte overflow.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.0.len() > MAXIMUM_REGISTERED_TOOLS {
            return Err("too many tool output declarations".into());
        }
        let mut bytes = 2 + self.0.len().saturating_sub(1);
        for (name, output) in &self.0 {
            rsi_tools_protocol::validate_identifier("tool name", name)
                .map_err(|_| "invalid declared tool name".to_owned())?;
            bytes += name.len() + 3 + output.encoded_len();
        }
        if bytes > MAXIMUM_TOOL_OUTPUT_CATALOG_BYTES {
            return Err("tool output catalog exceeds 256 KiB".into());
        }
        Ok(())
    }
}
