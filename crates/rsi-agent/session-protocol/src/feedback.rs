//! Operator feedback about a finalized assistant message, independent of model context.

use crate::{ModelEventPurpose, Result, SessionError, SessionFactBody};
use rsi_ai_protocol::{FinishReason, LanguageEvent};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// Rating of one assistant message.
#[derive(schemars::JsonSchema, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackRating {
    /// The message was helpful.
    Positive,
    /// The message was unhelpful.
    Negative,
}

/// Optional reason for an operator's rating.
#[derive(schemars::JsonSchema, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeedbackCategory {
    /// Quality of the outcome.
    TaskResult,
    /// Following the operator's instructions.
    InstructionFollowing,
    /// Product interaction or presentation.
    ProductInteraction,
    /// Availability or reliability.
    ServiceStability,
    /// Cost or resource consumption.
    ResourceCost,
    /// Security, privacy, or permission handling.
    SecurityPrivacyPermission,
    /// A reason outside the named categories.
    Other,
}

/// Nonblank operator text, preserved verbatim.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct FeedbackNote(String);

impl TryFrom<String> for FeedbackNote {
    type Error = SessionError;

    fn try_from(text: String) -> Result<Self> {
        if text.len() > 8192
            || text.chars().all(|c| {
                // The Web and DSH use ECMAScript WhiteSpace and LineTerminator for blank notes.
                (c.is_whitespace() && c != '\u{0085}') || c == '\u{feff}'
            })
        {
            return Err(SessionError::Invalid(
                "feedback note must be nonblank and at most 8192 UTF-8 bytes".into(),
            ));
        }
        Ok(Self(text))
    }
}

/// Complete current rating. Omitting a category or note clears the previous value.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageFeedback {
    /// Operator judgment.
    pub rating: FeedbackRating,
    /// Optional reason selected in the feedback dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<FeedbackCategory>,
    /// Optional original text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<FeedbackNote>,
}

/// Compare-and-set on a local assistant answer. A missing value retracts its rating.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageFeedbackChange {
    /// Fact sequence of the answer in the owning Session.
    pub message_seq: NonZeroU64,
    /// Control sequence of the rating seen by the operator, absent on creation.
    pub if_version: Option<NonZeroU64>,
    /// Desired rating after this change.
    pub value: Option<MessageFeedback>,
}

/// Latest live rating and the version used for a subsequent update.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedMessageFeedback {
    /// Sequence of the control that stored this value.
    pub version: NonZeroU64,
    /// Complete durable rating.
    pub value: MessageFeedback,
}

/// Outcome of a version-checked operator change. Both cases carry the authoritative head.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MessageFeedbackOutcome {
    /// The requested state was observed or committed.
    Saved {
        /// Current rating, absent after a removal.
        current: Option<SavedMessageFeedback>,
    },
    /// Another edit changed the observed predecessor.
    Conflict {
        /// Current rating at the conflict, absent if already removed.
        current: Option<SavedMessageFeedback>,
    },
}

/// Whether the observed head requires a new control, is already satisfied, or has changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackDecision {
    /// Store the requested replacement or removal.
    Append,
    /// Return the current head without writing.
    Unchanged,
    /// The caller must inspect the current head before modifying it.
    Conflict,
}

impl SessionFactBody {
    /// Whether this local Fact settles an assistant answer eligible for operator feedback.
    pub fn is_feedback_target(&self) -> bool {
        match self {
            Self::ModelAttemptSettled {
                purpose: ModelEventPurpose::Conversation,
                attempt,
                ..
            } => matches!(
                attempt.terminal(),
                LanguageEvent::Finished {
                    reason: FinishReason::Stop | FinishReason::ToolCalls,
                    ..
                }
            ),
            Self::ImageOutput { .. } => true,
            _ => false,
        }
    }
}

impl MessageFeedbackChange {
    /// Checks the exact predecessor. Removing a missing rating always succeeds.
    pub fn decide(&self, current: Option<&SavedMessageFeedback>) -> FeedbackDecision {
        if current.is_none() && self.value.is_none() {
            return FeedbackDecision::Unchanged;
        }
        if current.map(|item| item.version) != self.if_version {
            return FeedbackDecision::Conflict;
        }
        if current.map(|item| &item.value) == self.value.as_ref() {
            FeedbackDecision::Unchanged
        } else {
            FeedbackDecision::Append
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notes_preserve_nonblank_utf8_and_cas_distinguishes_stale_noops() {
        let limit = "🦀".repeat(2048);
        for valid in [limit.clone(), "  好  ".into(), "\u{0085}".into()] {
            let note: FeedbackNote = serde_json::from_value(json!(valid)).unwrap();
            assert_eq!(serde_json::to_value(note).unwrap(), valid);
        }
        for invalid in [
            limit + "a",
            "  \n ".into(),
            "\u{feff}".into(),
            String::new(),
        ] {
            assert!(serde_json::from_value::<FeedbackNote>(json!(invalid)).is_err());
        }
        assert!(
            serde_json::from_value::<MessageFeedback>(
                json!({"rating":"positive","category":"unknown"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<MessageFeedbackChange>(
                json!({"message_seq":0,"if_version":null,"value":null})
            )
            .is_err()
        );
        let mut change: MessageFeedbackChange = serde_json::from_value(json!({
            "message_seq":11,"if_version":null,"value":{"rating":"positive"}
        }))
        .unwrap();
        let head = SavedMessageFeedback {
            version: 3.try_into().unwrap(),
            value: change.value.clone().unwrap(),
        };
        assert_eq!(change.decide(None), FeedbackDecision::Append);
        assert_eq!(change.decide(Some(&head)), FeedbackDecision::Conflict);
        change.if_version = Some(head.version);
        assert_eq!(change.decide(Some(&head)), FeedbackDecision::Unchanged);
        change.value = None;
        assert_eq!(change.decide(Some(&head)), FeedbackDecision::Append);
        assert_eq!(change.decide(None), FeedbackDecision::Unchanged);
    }
}
