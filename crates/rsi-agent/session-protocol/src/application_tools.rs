use super::{Result, SessionError, validate_identifier, validate_safe_text};
use rsi_tools_protocol::MAXIMUM_REGISTERED_TOOLS;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One application declaration actually selected for a Conversation request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationToolBinding {
    /// Model-visible name selected from the captured runtime.
    pub name: String,
    /// Registration occurrence in the application process.
    pub registration: String,
    /// Registration owner authenticated by the native retained-result identity.
    pub owner: String,
}

/// Application participation in one model request series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationToolView {
    /// The selected process did not supply an executable catalog by preparation.
    Unavailable {
        /// Bounded operator diagnostic.
        reason: String,
    },
    /// Names and identities of the exact selected registration occurrences.
    Ready {
        /// Application process incarnation.
        generation: String,
        /// Stable receipt source, when the application supports cold outcome recovery.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_source: Option<String>,
        /// Ordered by exact tool name.
        tools: Vec<ApplicationToolBinding>,
    },
}

impl ApplicationToolView {
    /// Validates registration provenance against the producing request's sorted names.
    pub fn validate(&self, selected: &[String]) -> Result<()> {
        match self {
            Self::Unavailable { reason } => {
                validate_safe_text("application tool diagnostic", reason, 512, false)
            }
            Self::Ready {
                generation,
                receipt_source,
                tools,
            } => {
                validate_identifier("application generation", generation)?;
                if let Some(source) = receipt_source {
                    validate_identifier("application receipt source", source)?;
                }
                if tools.len() > MAXIMUM_REGISTERED_TOOLS {
                    return Err(SessionError::Invalid(
                        "application tool count exceeds capacity".into(),
                    ));
                }
                let mut owners = BTreeSet::new();
                let mut registrations = BTreeSet::new();
                let mut previous = None;
                for row in tools {
                    let name = row.name.as_str();
                    rsi_tools_protocol::validate_model_tool_name(name)
                        .map_err(|e| SessionError::Invalid(e.to_string()))?;
                    if previous.is_some_and(|last| last >= name)
                        || selected
                            .binary_search_by(|candidate| candidate.as_str().cmp(name))
                            .is_err()
                        || !owners.insert(&row.owner)
                        || !registrations.insert(&row.registration)
                    {
                        return Err(SessionError::Invalid(
                            "ambiguous or unselected application tool registration".into(),
                        ));
                    }
                    validate_identifier("application registration", &row.registration)?;
                    rsi_tools_protocol::validate_identifier("application tool owner", &row.owner)
                        .map_err(|e| SessionError::Invalid(e.to_string()))?;
                    previous = Some(name);
                }
                Ok(())
            }
        }
    }
    /// Selected registrations, empty while the application is unavailable.
    #[must_use]
    pub fn bindings(&self) -> &[ApplicationToolBinding] {
        match self {
            Self::Ready { tools, .. } => tools,
            Self::Unavailable { .. } => &[],
        }
    }
}
