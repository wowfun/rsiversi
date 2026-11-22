//! Complete invocation selection shared by Headers, overrides and domain plugins.

use crate::{FrozenAgentSettings, Result, SessionError};
use rsi_ai_protocol::{ModelRef, ReasoningEffortId};
use serde::{Deserialize, Serialize};

/// One exact route and optional adapter-owned reasoning choice.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    /// Exact provider deployment and model.
    pub model: ModelRef,
    /// None requests this model's declared or unknown provider default.
    pub reasoning_effort: Option<ReasoningEffortId>,
}
impl ModelSelection {
    /// Pinned contribution identity for changing this selection in a Session.
    pub const COMMAND: &'static str = "rsi.model-selection.command";

    /// Revalidates the complete route; effort is constructor/decode validated.
    pub fn validate(&self) -> Result<()> {
        self.model
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))
    }
    /// Returns the immutable creation-time selection.
    pub fn baseline(settings: &FrozenAgentSettings) -> Self {
        Self {
            model: settings.default_model().clone(),
            reasoning_effort: settings.default_reasoning_effort().cloned(),
        }
    }
}
