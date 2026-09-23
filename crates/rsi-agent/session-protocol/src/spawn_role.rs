//! Durable definition chosen by one fresh named spawn.
use crate::{
    DelegationRole, ModelSelection, Result, SessionError, validate_identifier, validate_sha256,
};
use serde::{Deserialize, Serialize};

/// Stable provider and name, independent of editable source contents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRoleReference {
    /// Trusted definition provider namespace.
    pub provider: String,
    /// Validated role name.
    pub name: String,
}
impl SpawnRoleReference {
    /// Validates both bounded identifiers.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("role provider", &self.provider)?;
        validate_identifier("role name", &self.name)?;
        if self.provider.len() > 64 || self.name.len() > 64 {
            return Err(SessionError::Invalid(
                "spawn role reference exceeds 64 bytes".into(),
            ));
        }
        Ok(())
    }
}

/// Complete normalized source, separate from the effective descendant restriction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRoleSeed {
    /// Source name used for selection and retry identity.
    pub reference: SpawnRoleReference,
    /// Original instructions and requested allow/deny sets.
    pub role: DelegationRole,
    /// Definition default route and optional effort.
    pub model: Option<ModelSelection>,
    /// Bounded display locator; never execution authority.
    pub source: String,
    /// Digest of the Markdown bytes or serialized inline role selected at admission.
    pub sha256: String,
}
impl SpawnRoleSeed {
    /// Validates external definition data before it becomes durable.
    pub fn validate(&self) -> Result<()> {
        self.reference.validate()?;
        self.role.validate()?;
        if self.reference.name != self.role.name
            || self.source.len() > 4096
            || self.source.chars().any(char::is_control)
            || self.source.is_empty()
        {
            return Err(SessionError::Invalid(
                "invalid spawn definition source".into(),
            ));
        }
        if let Some(model) = &self.model {
            model.validate()?;
        }
        validate_sha256("spawn definition digest", &self.sha256)
    }
}

/// Accepted invocation identity and its exact source definition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRoleRecord {
    /// Complete admitted definition.
    pub seed: SpawnRoleSeed,
    /// Canonical original request digest, excluding mutable source bytes.
    pub request_sha256: String,
}
impl SpawnRoleRecord {
    /// Validates the durable seed and immutable invocation identity.
    pub fn validate(&self) -> Result<()> {
        self.seed.validate()?;
        validate_sha256("spawn request digest", &self.request_sha256)
    }
}
