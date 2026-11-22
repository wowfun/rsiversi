use crate::{Result, SessionError};
use rsi_agent_session_protocol::ModelSelection;
use rsi_ai_protocol::{LanguageModelDescription, ReasoningEffortId};
use serde::{Deserialize, Serialize};

/// Current route description or a bounded explanation without invocation work.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)] // One current selection per reply, bounded by the API codec.
pub enum ModelAvailability {
    /// Current committed provider facts.
    Available {
        /// Exact route, configuration and declared profile.
        description: LanguageModelDescription,
    },
    /// Selection remains retained even while its route or effort is unavailable.
    Unavailable {
        /// Safe diagnostic, at most 2048 UTF-8 bytes.
        reason: String,
    },
}
/// Current selection is independent of an older usage reduction watermark.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectionRead {
    /// Durable selection or the immutable Header baseline.
    pub selection: ModelSelection,
    /// Read-time availability; unavailable state never changes the selection.
    pub availability: ModelAvailability,
}
impl ModelSelectionRead {
    /// Validates route identity, effort support and bounded failure text.
    pub fn validate(&self) -> Result<()> {
        self.selection
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        match &self.availability {
            ModelAvailability::Available { description } => {
                description
                    .validate()
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
                if description.model() != &self.selection.model {
                    return Err(SessionError::Invalid(
                        "selected model description changed the route".into(),
                    ));
                }
                description
                    .profile()
                    .reasoning_efforts()
                    .resolve(self.selection.reasoning_effort.as_ref())
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
            }
            ModelAvailability::Unavailable { reason }
                if reason.is_empty() || reason.len() > 2048 =>
            {
                return Err(SessionError::Invalid(
                    "model availability diagnostic is empty or oversized".into(),
                ));
            }
            ModelAvailability::Unavailable { .. } => {}
        }
        Ok(())
    }
    /// Resolved declared default, or explicit selection; unavailable profiles remain absent.
    pub fn effective_effort(&self) -> Option<&ReasoningEffortId> {
        match &self.availability {
            ModelAvailability::Available { description } => self
                .selection
                .reasoning_effort
                .as_ref()
                .or_else(|| description.profile().reasoning_efforts().default_effort()),
            ModelAvailability::Unavailable { .. } => None,
        }
    }
}

/// Model picker input bound to the displayed Session revision.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectSessionModel {
    /// Client-generated immutable operation identity.
    pub request_id: rsi_agent_session_protocol::DomainRequestId,
    /// Predecessor captured by the caller.
    pub expected_revision: rsi_agent_session_protocol::CommandRevision,
    /// Complete model and reasoning choice.
    pub selection: ModelSelection,
}
impl SelectSessionModel {
    /// Uses the same durable command identity as the model contribution.
    pub fn invocation(&self) -> Result<rsi_agent_session_protocol::SessionCommandInvocation> {
        use rsi_agent_session_protocol::{
            CommandArguments, ContributionId, SessionCommandInvocation,
        };
        let value = serde_json::to_value(&self.selection)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(SessionCommandInvocation {
            command: ContributionId::new(ModelSelection::COMMAND).expect("model command identity"),
            request_id: self.request_id.clone(),
            expected_revision: self.expected_revision,
            arguments: CommandArguments::new(value)
                .map_err(|error| SessionError::Invalid(error.to_string()))?,
        })
    }
}
