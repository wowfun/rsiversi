//! Generation-pinned model capability reads without invocation authority.

use crate::{AiContractError, LanguageProfile, ModelRef, PreparedCallSnapshot};
use serde::{Deserialize, Serialize};

/// Committed route identity and its authoritative semantic profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Wire")]
pub struct LanguageModelDescription {
    model: ModelRef,
    profile: LanguageProfile,
    config_generation: u64,
    endpoint_fingerprint: String,
    protocol: String,
    transport: String,
    provider_family: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    model: ModelRef,
    profile: LanguageProfile,
    config_generation: u64,
    endpoint_fingerprint: String,
    protocol: String,
    transport: String,
    provider_family: String,
}
impl TryFrom<Wire> for LanguageModelDescription {
    type Error = AiContractError;
    fn try_from(wire: Wire) -> Result<Self, Self::Error> {
        Self::new(
            wire.model,
            wire.profile,
            wire.config_generation,
            wire.endpoint_fingerprint,
            wire.protocol,
            wire.transport,
            wire.provider_family,
        )
    }
}
impl LanguageModelDescription {
    /// Joins one validated profile with its committed registration identity.
    pub fn new(
        model: ModelRef,
        profile: LanguageProfile,
        config_generation: u64,
        endpoint_fingerprint: impl Into<String>,
        protocol: impl Into<String>,
        transport: impl Into<String>,
        provider_family: impl Into<String>,
    ) -> Result<Self, AiContractError> {
        let description = Self {
            model,
            profile,
            config_generation,
            endpoint_fingerprint: endpoint_fingerprint.into(),
            protocol: protocol.into(),
            transport: transport.into(),
            provider_family: provider_family.into(),
        };
        description.validate()?;
        Ok(description)
    }
    /// Exact route supplied to description or Prepare.
    pub fn model(&self) -> &ModelRef {
        &self.model
    }
    /// Semantic capabilities from that registration.
    pub fn profile(&self) -> &LanguageProfile {
        &self.profile
    }
    /// Transfers the already validated profile to an invocation planner.
    pub fn into_profile(self) -> LanguageProfile {
        self.profile
    }
    /// Provider Fiber generation; meaningful alongside endpoint/protocol/profile identity.
    pub fn config_generation(&self) -> u64 {
        self.config_generation
    }
    /// Revalidates all externally decoded fields.
    pub fn validate(&self) -> Result<(), AiContractError> {
        self.model.validate()?;
        self.profile
            .validate()
            .map_err(|error| AiContractError::invalid(error.to_string()))?;
        if self.config_generation == 0 {
            return Err(AiContractError::invalid(
                "model description has zero generation",
            ));
        }
        for (name, text) in [
            ("endpoint_fingerprint", &self.endpoint_fingerprint),
            ("protocol", &self.protocol),
            ("transport", &self.transport),
            ("provider_family", &self.provider_family),
        ] {
            crate::validate_identifier(name, text).map_err(AiContractError::invalid)?;
        }
        Ok(())
    }
    /// Recovers the exact described configuration used by an actual Language attempt.
    pub fn from_snapshot(snapshot: &PreparedCallSnapshot) -> Result<Self, AiContractError> {
        let settings = snapshot
            .language_settings
            .as_ref()
            .ok_or_else(|| AiContractError::invalid("prepared call has no Language profile"))?;
        Self::new(
            ModelRef::new(&snapshot.deployment_id, &snapshot.model)?,
            settings.profile.clone(),
            snapshot.config_generation,
            &snapshot.endpoint_fingerprint,
            &snapshot.protocol,
            &snapshot.transport,
            &snapshot.provider_family,
        )
    }
}
