//! Durable provenance for pre-start business decisions.

use super::{
    ContributionId, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, Result, SessionError, validate_safe_text,
};
use rsi_approval_protocol::{ApprovalDecision, ApprovalOutcome};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Maximum combined UTF-8 bytes of a Step's current system instructions.
pub const MAXIMUM_STEP_INSTRUCTION_BYTES: usize = 64 * 1024;

/// One producer's current Step guidance, separate from historical input messages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepInstruction {
    /// Framework-assigned registered producer identity.
    pub producer: ContributionId,
    /// Nonempty bounded current system guidance.
    pub text: String,
}

/// Resolved system prompt with explicit ownership of immutable Header defaults.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepPrompt {
    /// Complete prompt, including any defaults chosen by the assembler.
    Assembled {
        /// Ordered resolved sections; empty clears the system prompt.
        sections: Vec<StepInstruction>,
    },
    /// Guidance appended to the immutable Header and delegation persona.
    AppendToHeader {
        /// Ordered guidance; empty retains only Header defaults.
        sections: Vec<StepInstruction>,
    },
}

impl Default for StepPrompt {
    fn default() -> Self {
        Self::AppendToHeader {
            sections: Vec::new(),
        }
    }
}

impl StepPrompt {
    /// Borrows the resolved sections without discarding their ownership tag.
    pub fn sections(&self) -> &[StepInstruction] {
        match self {
            Self::Assembled { sections } | Self::AppendToHeader { sections } => sections,
        }
    }

    /// Checks the durable representation at its owning boundary.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::AppendToHeader { sections } => validate_step_instructions(sections),
            Self::Assembled { sections } => validate_sections(
                sections,
                66,
                super::MAXIMUM_SYSTEM_PROMPT_BYTES
                    + super::MAXIMUM_DELEGATION_PERSONA_BYTES
                    + MAXIMUM_STEP_INSTRUCTION_BYTES,
                false,
                "System prompt",
            ),
        }
    }
}

/// Validates a complete ordered Step instruction assembly.
///
/// # Errors
/// Rejects duplicate producers, empty sections or the aggregate byte bound.
pub fn validate_step_instructions(sections: &[StepInstruction]) -> Result<()> {
    validate_sections(
        sections,
        64,
        MAXIMUM_STEP_INSTRUCTION_BYTES,
        true,
        "Step instructions",
    )
}

fn validate_sections(
    sections: &[StepInstruction],
    maximum_sections: usize,
    maximum_bytes: usize,
    reject_blank: bool,
    label: &str,
) -> Result<()> {
    let mut producers = std::collections::BTreeSet::new();
    let mut bytes = 0_usize;
    if sections.len() > maximum_sections {
        return Err(SessionError::Invalid(format!("too many {label} sections")));
    }
    for section in sections {
        validate_safe_text(label, &section.text, maximum_bytes, false)?;
        if (reject_blank && section.text.trim().is_empty()) || !producers.insert(&section.producer)
        {
            return Err(SessionError::Invalid(format!(
                "empty or duplicate {label} section"
            )));
        }
        bytes = bytes
            .checked_add(section.text.len())
            .ok_or_else(|| SessionError::Invalid(format!("{label} size overflow")))?;
    }
    if bytes > maximum_bytes {
        return Err(SessionError::Invalid(format!("{label} budget exceeded")));
    }
    Ok(())
}

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
