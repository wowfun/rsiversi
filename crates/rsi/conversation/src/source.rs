use crate::{FieldWindow, WindowError};
use rsi_agent_session_protocol::{
    AgentMessageContent, SessionFact, SessionFactBody, ToolRejection, TurnOutcome,
};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, PreparedCallSnapshot};
use rsi_media_protocol::MediaRef;
use rsi_tools_protocol::ToolContent;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Closed semantic field within an exact durable Fact; indices are content positions.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactField {
    /// Original input in a directly accepted Turn.
    TurnInput,
    /// One entered message's text content.
    InputText { index: u16 },
    /// One entered message's image reference.
    InputImage { index: u16 },
    /// One assistant text delta.
    ModelText,
    /// One reasoning delta.
    ModelReasoning,
    /// One streamed Tool-argument delta.
    ModelToolArguments,
    /// Display text of a normalized model failure.
    ModelFailure,
    /// Redacted prepared provider identity and request metadata.
    ModelSnapshot,
    /// Complete arguments from a Tool intent or pre-execution rejection.
    ToolArguments,
    /// Typed pre-execution rejection and provenance.
    ToolRejection,
    /// Complete programmatic Tool result value.
    ToolValue,
    /// One ordered model-facing Tool text item.
    ToolText { index: u16 },
    /// One ordered model-facing Tool image item.
    ToolImage { index: u16 },
    /// Exact terminal outcome.
    TurnOutcome,
    /// A generated durable image reference.
    ImageOutput,
}
impl<'de> Deserialize<'de> for FactField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: String,
            #[serde(default, deserialize_with = "index")]
            index: Option<u16>,
        }
        fn index<'de, D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<u16>, D::Error> {
            u16::deserialize(deserializer).map(Some)
        }
        let wire = Wire::deserialize(deserializer)?;
        let field = match (wire.kind.as_str(), wire.index) {
            ("turn_input", None) => Self::TurnInput,
            ("input_text", Some(index)) => Self::InputText { index },
            ("input_image", Some(index)) => Self::InputImage { index },
            ("model_text", None) => Self::ModelText,
            ("model_reasoning", None) => Self::ModelReasoning,
            ("model_tool_arguments", None) => Self::ModelToolArguments,
            ("model_failure", None) => Self::ModelFailure,
            ("model_snapshot", None) => Self::ModelSnapshot,
            ("tool_arguments", None) => Self::ToolArguments,
            ("tool_rejection", None) => Self::ToolRejection,
            ("tool_value", None) => Self::ToolValue,
            ("tool_text", Some(index)) => Self::ToolText { index },
            ("tool_image", Some(index)) => Self::ToolImage { index },
            ("turn_outcome", None) => Self::TurnOutcome,
            ("image_output", None) => Self::ImageOutput,
            _ => {
                return Err(serde::de::Error::custom(
                    "invalid Fact field or content index",
                ));
            }
        };
        Ok(field)
    }
}
impl std::fmt::Display for FactField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Attachment-local exact source; the caller separately owns its Session binding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    /// Positive durable sequence, serialized without JavaScript number conversion.
    #[serde(with = "decimal")]
    pub seq: u64,
    /// Variant-checked payload selection.
    pub field: FactField,
}
impl Ord for SourceRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.seq
            .cmp(&other.seq)
            .then_with(|| order(self.field).cmp(&order(other.field)))
    }
}
impl PartialOrd for SourceRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
fn order(field: FactField) -> (u8, u16, u8) {
    match field {
        FactField::TurnInput => (0, 0, 0),
        FactField::InputText { index } => (1, index, 0),
        FactField::InputImage { index } => (1, index, 1),
        FactField::ModelText => (2, 0, 0),
        FactField::ModelReasoning => (3, 0, 0),
        FactField::ModelToolArguments => (4, 0, 0),
        FactField::ModelFailure => (5, 0, 0),
        FactField::ModelSnapshot => (6, 0, 0),
        FactField::ToolArguments => (7, 0, 0),
        FactField::ToolRejection => (8, 0, 0),
        FactField::ToolValue => (9, 0, 0),
        FactField::ToolText { index } => (10, index, 0),
        FactField::ToolImage { index } => (10, index, 1),
        FactField::TurnOutcome => (11, 0, 0),
        FactField::ImageOutput => (12, 0, 0),
    }
}
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde field hooks receive a borrowed field.
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        if *value == 0 {
            return Err(serde::ser::Error::custom("invalid positive Fact sequence"));
        }
        serializer.collect_str(value)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > 20
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(serde::de::Error::custom("invalid positive Fact sequence"));
        }
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Borrowed semantic payload; no full Fact or observation lease is retained.
#[derive(Debug)]
pub enum FieldValue<'a> {
    /// Exact text, or bounded formatted model diagnostic.
    Text(Cow<'a, str>),
    /// Arguments or programmatic result.
    Json(&'a serde_json::Value),
    /// Terminal outcome.
    Outcome(&'a TurnOutcome),
    /// Tool rejection.
    Rejection(&'a ToolRejection),
    /// Redacted provider preparation.
    Model(&'a PreparedCallSnapshot),
    /// Immutable image reference.
    Media(&'a MediaRef),
}
impl FieldValue<'_> {
    /// Reads a bounded raw source window before renderer-specific sanitization.
    ///
    /// # Errors
    /// Returns [`WindowError`] for invalid bounds or failed serialization.
    pub fn window(&self, start: usize, maximum: usize) -> Result<FieldWindow, WindowError> {
        match self {
            Self::Text(text) => FieldWindow::text(text, start, maximum),
            Self::Json(value) => FieldWindow::json(value, start, maximum),
            Self::Outcome(value) => FieldWindow::json(value, start, maximum),
            Self::Rejection(value) => FieldWindow::json(value, start, maximum),
            Self::Model(value) => FieldWindow::json(value, start, maximum),
            Self::Media(value) => FieldWindow::json(value, start, maximum),
        }
    }
}

/// Selects only an exact matching source; missing or variant-mismatched fields are unavailable.
pub fn select_field(fact: &SessionFact, source: SourceRef) -> Option<FieldValue<'_>> {
    if fact.seq() != source.seq {
        return None;
    }
    let text = match (fact.body(), source.field) {
        (SessionFactBody::InputMessageEntered { content, .. }, FactField::InputText { index }) => {
            let AgentMessageContent::Text { text } = content.get(usize::from(index))? else {
                return None;
            };
            text
        }
        (SessionFactBody::InputMessageEntered { content, .. }, FactField::InputImage { index }) => {
            let AgentMessageContent::Image { media } = content.get(usize::from(index))? else {
                return None;
            };
            return Some(FieldValue::Media(media));
        }
        (SessionFactBody::TurnAccepted { text, .. }, FactField::TurnInput)
        | (
            SessionFactBody::ModelEvent {
                event:
                    LanguageEvent::ContentDelta {
                        delta: ContentDelta::Text(text),
                        ..
                    },
                ..
            },
            FactField::ModelText,
        )
        | (
            SessionFactBody::ModelEvent {
                event:
                    LanguageEvent::ContentDelta {
                        delta: ContentDelta::Reasoning(text),
                        ..
                    },
                ..
            },
            FactField::ModelReasoning,
        )
        | (
            SessionFactBody::ModelEvent {
                event:
                    LanguageEvent::ContentDelta {
                        delta: ContentDelta::ToolArguments(text),
                        ..
                    },
                ..
            },
            FactField::ModelToolArguments,
        ) => text,
        (
            SessionFactBody::ModelEvent {
                event: LanguageEvent::Failed { error, .. },
                ..
            },
            FactField::ModelFailure,
        ) => return Some(FieldValue::Text(Cow::Owned(error.to_string()))),
        (
            SessionFactBody::ModelIntent { snapshot, .. }
            | SessionFactBody::ImageIntent { snapshot, .. },
            FactField::ModelSnapshot,
        ) => return Some(FieldValue::Model(snapshot)),
        (
            SessionFactBody::ToolIntent { arguments, .. }
            | SessionFactBody::ToolRejected { arguments, .. },
            FactField::ToolArguments,
        ) => return Some(FieldValue::Json(arguments)),
        (SessionFactBody::ToolRejected { rejection, .. }, FactField::ToolRejection) => {
            return Some(FieldValue::Rejection(rejection));
        }
        (SessionFactBody::ToolResult { result, .. }, FactField::ToolValue) => {
            return Some(FieldValue::Json(&result.value));
        }
        (SessionFactBody::ToolResult { result, .. }, FactField::ToolText { index }) => {
            let ToolContent::Text { text } = result.content.get(usize::from(index))? else {
                return None;
            };
            text
        }
        (SessionFactBody::ToolResult { result, .. }, FactField::ToolImage { index }) => {
            let ToolContent::Image { media } = result.content.get(usize::from(index))? else {
                return None;
            };
            return Some(FieldValue::Media(media));
        }
        (SessionFactBody::TurnTerminal { outcome, .. }, FactField::TurnOutcome) => {
            return Some(FieldValue::Outcome(outcome));
        }
        (SessionFactBody::ImageOutput { media, .. }, FactField::ImageOutput) => {
            return Some(FieldValue::Media(media));
        }
        _ => return None,
    };
    Some(FieldValue::Text(Cow::Borrowed(text)))
}
