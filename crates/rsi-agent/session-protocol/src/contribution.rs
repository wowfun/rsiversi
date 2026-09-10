//! Durable provenance for pre-start business decisions.

use super::{
    ContributionId, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, Result, SessionError, validate_safe_text,
};
use rsi_approval_protocol::{ApprovalDecision, ApprovalOutcome};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// A Tool call was refused before any intent or external execution was admitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolRejection {
    /// The exact live approval request was denied.
    ApprovalDenied {
        /// Bounded decision and answerer provenance; must contain Deny.
        outcome: ApprovalOutcome,
    },
    /// One pinned business policy denied this call.
    PolicyDenied {
        /// Stable identity of the deciding contribution.
        contribution_id: ContributionId,
        /// Bounded nonempty explanation preserved for model and human replay.
        reason: String,
    },
}

impl ToolRejection {
    /// Checks the denial and its durable diagnostic bounds.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::ApprovalDenied { outcome } => {
                outcome
                    .validate()
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
                if outcome.decision != ApprovalDecision::Deny {
                    return Err(SessionError::Invalid(
                        "Tool rejection requires denied approval".into(),
                    ));
                }
                Ok(())
            }
            Self::PolicyDenied { reason, .. } => validate_safe_text(
                "Tool policy rejection",
                reason,
                MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
                false,
            ),
        }
    }

    /// Returns the durable explanation without consulting a live policy or approval service.
    pub fn message(&self) -> Cow<'_, str> {
        match self {
            Self::ApprovalDenied { outcome } => outcome.reason.as_ref().map_or(
                Cow::Borrowed("Live approval denied the Tool call; it was not executed."),
                |reason| Cow::Owned(format!("Live approval denied the Tool call: {reason}")),
            ),
            Self::PolicyDenied { reason, .. } => Cow::Borrowed(reason),
        }
    }
}
