//! Frozen monotone child Tool restrictions; never permission grants.
use crate::{Result, SessionError, validate_identifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Maximum persona bytes in a configured role and frozen child policy.
pub const MAXIMUM_DELEGATION_PERSONA_BYTES: usize = 32 * 1024;
/// Maximum exact Tool names in a role or frozen policy.
pub const MAXIMUM_DELEGATION_TOOLS: usize = 64;

/// Trusted configuration selected by a model-facing role name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationRole {
    /// Stable configured role identity.
    pub name: String,
    /// Additional frozen instructions, without permission authority.
    pub persona: Option<String>,
    /// None keeps all current names; an empty set keeps no ordinary Tool.
    pub allow: Option<BTreeSet<String>>,
    /// Exact names removed after allow selection.
    #[serde(default)]
    pub deny: BTreeSet<String>,
}
impl DelegationRole {
    /// Validates bounded role configuration before spawn work.
    pub fn validate(&self) -> Result<()> {
        validate_identifier("delegation role", &self.name)?;
        if self.name.len() > 64 {
            return Err(invalid("role name exceeds 64 bytes"));
        }
        validate_persona(self.persona.as_deref())?;
        if let Some(allow) = &self.allow {
            validate_names(allow)?;
        }
        validate_names(&self.deny)
    }
}

/// Durable effective restriction, independent of future configuration changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationPolicy {
    role: Option<String>,
    persona: Option<String>,
    role_sha256: String,
    tools: BTreeSet<String>,
}
impl<'de> Deserialize<'de> for DelegationPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            role: Option<String>,
            persona: Option<String>,
            role_sha256: String,
            tools: BTreeSet<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            role: wire.role,
            persona: wire.persona,
            role_sha256: wire.role_sha256,
            tools: wire.tools,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
impl DelegationPolicy {
    /// Freezes current names after configured and ancestor restrictions.
    /// Unknown configured names are rejected before any child admission.
    pub fn freeze(
        role: Option<&DelegationRole>,
        current: BTreeSet<String>,
        parent: Option<&Self>,
    ) -> Result<Self> {
        validate_names(&current)?;
        if let Some(role) = role {
            role.validate()?;
            if role
                .allow
                .iter()
                .flatten()
                .chain(&role.deny)
                .any(|name| !current.contains(name))
            {
                return Err(invalid(
                    "configured delegation Tool is absent from the current catalog",
                ));
            }
        }
        let tools = current
            .into_iter()
            .filter(|name| {
                parent.is_none_or(|policy| policy.tools.contains(name))
                    && role.is_none_or(|role| {
                        role.allow.as_ref().is_none_or(|allow| allow.contains(name))
                            && !role.deny.contains(name)
                    })
            })
            .collect();
        let value = Self {
            role: role.map(|role| role.name.clone()),
            persona: role.and_then(|role| role.persona.clone()),
            role_sha256: role_digest(role)?,
            tools,
        };
        value.validate()?;
        Ok(value)
    }
    /// Checks durable shape and aggregate limits.
    pub fn validate(&self) -> Result<()> {
        if let Some(role) = &self.role {
            validate_identifier("delegation role", role)?;
            if role.len() > 64 {
                return Err(invalid("role name exceeds 64 bytes"));
            }
        }
        validate_persona(self.persona.as_deref())?;
        if self.role_sha256.len() != 64
            || !self
                .role_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("invalid delegation role digest"));
        }
        if self.role.is_none() && self.persona.is_some() {
            return Err(invalid("persona requires a selected role"));
        }
        validate_names(&self.tools)
    }
    /// Checks exact retry configuration without consulting a changed Tool catalog.
    pub fn matches_role(&self, role: Option<&DelegationRole>) -> Result<bool> {
        if let Some(role) = role {
            role.validate()?;
        }
        Ok(self.role_sha256 == role_digest(role)?)
    }
    /// Returns the selected role name.
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }
    /// Returns frozen instruction text.
    pub fn persona(&self) -> Option<&str> {
        self.persona.as_deref()
    }
    /// Returns the maximum names this child may ever expose.
    pub const fn tools(&self) -> &BTreeSet<String> {
        &self.tools
    }
    /// Returns the normalized requested role digest.
    pub fn role_sha256(&self) -> &str {
        &self.role_sha256
    }
}
fn role_digest(role: Option<&DelegationRole>) -> Result<String> {
    serde_json::to_vec(&role)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .map_err(|e| SessionError::Encoding(e.to_string()))
}
fn validate_persona(persona: Option<&str>) -> Result<()> {
    if persona
        .is_some_and(|text| text.len() > MAXIMUM_DELEGATION_PERSONA_BYTES || text.contains('\0'))
    {
        return Err(invalid("invalid delegation persona"));
    }
    Ok(())
}
fn validate_names(names: &BTreeSet<String>) -> Result<()> {
    if names.len() > MAXIMUM_DELEGATION_TOOLS
        || names.iter().any(|name| {
            rsi_tools_protocol::ToolCall::validate_fields("role", name, &serde_json::json!({}))
                .is_err()
        })
    {
        return Err(invalid("invalid delegation Tool set"));
    }
    Ok(())
}
fn invalid(message: &str) -> SessionError {
    SessionError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn names(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|name| (*name).into()).collect()
    }
    #[test]
    fn ancestor_intersection_and_exact_retry_preserve_frozen_meaning() {
        let role = DelegationRole {
            name: "reader".into(),
            persona: Some("Inspect evidence".into()),
            allow: Some(names(&["read", "spawn_agent"])),
            deny: BTreeSet::new(),
        };
        let parent =
            DelegationPolicy::freeze(Some(&role), names(&["read", "write", "spawn_agent"]), None)
                .unwrap();
        let child =
            DelegationPolicy::freeze(None, names(&["read", "write", "new_tool"]), Some(&parent))
                .unwrap();
        assert_eq!(child.tools(), &names(&["read"]));
        assert!(parent.matches_role(Some(&role)).unwrap());
        let changed = DelegationRole {
            persona: Some("changed".into()),
            ..role.clone()
        };
        assert!(!parent.matches_role(Some(&changed)).unwrap());
        assert_eq!(
            serde_json::from_slice::<DelegationPolicy>(&serde_json::to_vec(&parent).unwrap())
                .unwrap(),
            parent
        );
        assert!(DelegationPolicy::freeze(Some(&role), names(&["read"]), None).is_err());
        let empty = DelegationRole {
            allow: Some(BTreeSet::new()),
            ..role
        };
        assert!(
            DelegationPolicy::freeze(Some(&empty), names(&["read"]), None)
                .unwrap()
                .tools()
                .is_empty()
        );
    }
}
