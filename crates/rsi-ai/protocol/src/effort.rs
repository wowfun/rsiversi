//! Adapter-owned reasoning choices, independent of model capacity declarations.

use crate::SemanticError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

/// Actual generation-pinned reasoning and capacity facts for one prepared request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedLanguageSettings {
    /// Explicit caller choice; absent means the adapter default was requested.
    pub requested_reasoning_effort: Option<ReasoningEffortId>,
    /// Declared resolved choice; absent means the provider default is unknown.
    pub effective_reasoning_effort: Option<ReasoningEffortId>,
    /// Exact capacity and capability declaration captured before dispatch.
    pub profile: crate::LanguageProfile,
}
impl<'de> Deserialize<'de> for PreparedLanguageSettings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            requested_reasoning_effort: Option<ReasoningEffortId>,
            effective_reasoning_effort: Option<ReasoningEffortId>,
            profile: crate::LanguageProfile,
        }
        let wire = Wire::deserialize(d)?;
        let value = Self {
            requested_reasoning_effort: wire.requested_reasoning_effort,
            effective_reasoning_effort: wire.effective_reasoning_effort,
            profile: wire.profile,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}
impl PreparedLanguageSettings {
    /// Captures a supported choice from the exact prepared adapter profile.
    pub fn new(
        profile: crate::LanguageProfile,
        requested: Option<ReasoningEffortId>,
    ) -> Result<Self, SemanticError> {
        let effective = profile.reasoning_efforts().resolve(requested.as_ref())?;
        Ok(Self {
            profile,
            requested_reasoning_effort: requested,
            effective_reasoning_effort: effective,
        })
    }
    /// Rejects inconsistent requested/effective settings in durable input.
    pub fn validate(&self) -> Result<(), SemanticError> {
        self.profile.validate()?;
        if self
            .profile
            .reasoning_efforts()
            .resolve(self.requested_reasoning_effort.as_ref())?
            != self.effective_reasoning_effort
        {
            return Err(invalid(
                "prepared effective effort differs from its captured profile",
            ));
        }
        Ok(())
    }
}

/// An exact adapter-owned identifier, without ordinal or cross-provider semantics.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReasoningEffortId(String);

impl ReasoningEffortId {
    /// Validates a 1–32 byte ASCII identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, SemanticError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 32
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err(invalid("effort must be a 1–32 byte ASCII identifier"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ReasoningEffortId {
    type Error = SemanticError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ReasoningEffortId> for String {
    fn from(value: ReasoningEffortId) -> Self {
        value.0
    }
}
impl fmt::Display for ReasoningEffortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Complete supported choices and known default from one prepared adapter generation.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningEffortProfile {
    supported: Vec<ReasoningEffortId>,
    default: Option<ReasoningEffortId>,
}
impl<'de> Deserialize<'de> for ReasoningEffortProfile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            supported: Vec<ReasoningEffortId>,
            default: Option<ReasoningEffortId>,
        }
        let value = Wire::deserialize(d)?;
        Self::new(value.supported, value.default).map_err(serde::de::Error::custom)
    }
}
impl ReasoningEffortProfile {
    /// Validates the finite, ordered choices and optional supported default.
    pub fn new(
        supported: Vec<ReasoningEffortId>,
        default: Option<ReasoningEffortId>,
    ) -> Result<Self, SemanticError> {
        if supported.len() > 16
            || supported.iter().collect::<BTreeSet<_>>().len() != supported.len()
            || default
                .as_ref()
                .is_some_and(|value| !supported.contains(value))
        {
            return Err(invalid(
                "effort profile requires at most 16 unique choices and a supported default",
            ));
        }
        Ok(Self { supported, default })
    }
    pub fn supported(&self) -> &[ReasoningEffortId] {
        &self.supported
    }
    pub fn default_effort(&self) -> Option<&ReasoningEffortId> {
        self.default.as_ref()
    }
    /// Resolves a supported explicit choice or the declared default.
    pub fn resolve(
        &self,
        requested: Option<&ReasoningEffortId>,
    ) -> Result<Option<ReasoningEffortId>, SemanticError> {
        match requested {
            Some(value) if !self.supported.contains(value) => Err(invalid(
                "selected model does not declare the requested reasoning effort",
            )),
            Some(value) => Ok(Some(value.clone())),
            None => Ok(self.default.clone()),
        }
    }
}

fn invalid(reason: &str) -> SemanticError {
    SemanticError::new("reasoning_effort.invalid", "reasoning_effort", reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoded_choices_are_bounded_and_not_equated() {
        for value in ["", "a b", "高", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"] {
            assert!(ReasoningEffortId::new(value).is_err());
            assert!(serde_json::from_value::<ReasoningEffortId>(serde_json::json!(value)).is_err());
        }
        let max = ReasoningEffortId::new("max").unwrap();
        let profile = ReasoningEffortProfile::new(vec![max.clone()], Some(max.clone())).unwrap();
        assert_eq!(profile.resolve(None).unwrap(), Some(max));
        assert!(
            profile
                .resolve(Some(&ReasoningEffortId::new("xhigh").unwrap()))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ReasoningEffortProfile>(
                serde_json::json!({"supported":["max","max"],"default":"max"})
            )
            .is_err()
        );
        assert!(
            ReasoningEffortProfile::new(vec![], Some(ReasoningEffortId::new("off").unwrap()))
                .is_err()
        );
        assert_eq!(
            ReasoningEffortProfile::default().resolve(None).unwrap(),
            None
        );
    }
}
