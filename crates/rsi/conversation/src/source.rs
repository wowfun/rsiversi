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
    InputText { index: u32 },
    /// One entered message's image reference.
    InputImage { index: u32 },
    /// One entered message's verbatim file reference.
    InputFile { index: u32 },
    /// Exact inline text preview of one frozen user reference.
    InputReference { index: u32 },
    /// One assistant text delta.
    ModelText { index: u16 },
    /// One reasoning delta.
    ModelReasoning { index: u16 },
    /// One streamed Tool-argument delta.
    ModelToolArguments { index: u16 },
    /// Display text of a normalized model failure.
    ModelFailure,
    /// Redacted prepared provider identity and request metadata.
    ModelSnapshot,
    /// Complete arguments from a Tool intent or pre-execution rejection.
    ToolArguments,
    /// Raw shell command string from a bash intent or pre-execution rejection.
    ToolCommand,
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
impl FactField {
    /// Exact input field for a claimed or authored block at its Fact-local position.
    pub fn input(index: u32, content: &AgentMessageContent) -> Self {
        match content {
            AgentMessageContent::Text { .. } => Self::InputText { index },
            AgentMessageContent::Image { .. } => Self::InputImage { index },
            AgentMessageContent::File { .. } => Self::InputFile { index },
            AgentMessageContent::Reference { .. } => Self::InputReference { index },
        }
    }
}
impl<'de> Deserialize<'de> for FactField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: String,
            #[serde(default, deserialize_with = "index")]
            index: Option<u32>,
        }
        fn index<'de, D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<u32>, D::Error> {
            u32::deserialize(deserializer).map(Some)
        }
        let wire = Wire::deserialize(deserializer)?;
        if let Some(index) = wire.index {
            match wire.kind.as_str() {
                "input_text" => return Ok(Self::InputText { index }),
                "input_image" => return Ok(Self::InputImage { index }),
                "input_file" => return Ok(Self::InputFile { index }),
                "input_reference" => return Ok(Self::InputReference { index }),
                _ => {}
            }
        }
        let index = wire
            .index
            .map(u16::try_from)
            .transpose()
            .map_err(serde::de::Error::custom)?;
        let field = match (wire.kind.as_str(), index) {
            ("turn_input", None) => Self::TurnInput,
            ("model_text", Some(index))
                if usize::from(index) < rsi_ai_protocol::MAX_CONTENT_BLOCKS =>
            {
                Self::ModelText { index }
            }
            ("model_reasoning", Some(index))
                if usize::from(index) < rsi_ai_protocol::MAX_CONTENT_BLOCKS =>
            {
                Self::ModelReasoning { index }
            }
            ("model_tool_arguments", Some(index))
                if usize::from(index) < rsi_ai_protocol::MAX_CONTENT_BLOCKS =>
            {
                Self::ModelToolArguments { index }
            }
            ("model_failure", None) => Self::ModelFailure,
            ("model_snapshot", None) => Self::ModelSnapshot,
            ("tool_arguments", None) => Self::ToolArguments,
            ("tool_command", None) => Self::ToolCommand,
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
fn order(field: FactField) -> (u8, u32, u8) {
    match field {
        FactField::TurnInput => (0, 0, 0),
        FactField::InputText { index } => (1, index, 0),
        FactField::InputImage { index } => (1, index, 1),
        FactField::InputReference { index } => (1, index, 2),
        FactField::InputFile { index } => (1, index, 3),
        FactField::ModelText { index } => (2, u32::from(index), 0),
        FactField::ModelReasoning { index } => (3, u32::from(index), 0),
        FactField::ModelToolArguments { index } => (4, u32::from(index), 0),
        FactField::ModelFailure => (5, 0, 0),
        FactField::ModelSnapshot => (6, 0, 0),
        FactField::ToolCommand => (7, 0, 0),
        FactField::ToolArguments => (7, 0, 1),
        FactField::ToolRejection => (8, 0, 0),
        FactField::ToolValue => (9, 0, 0),
        FactField::ToolText { index } => (10, u32::from(index), 0),
        FactField::ToolImage { index } => (10, u32::from(index), 1),
        FactField::TurnOutcome => (11, 0, 0),
        FactField::ImageOutput => (12, 0, 0),
    }
}
pub(crate) mod decimal {
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
    /// One compact attempt's semantic text field.
    Attempt(&'a rsi_agent_session_protocol::ModelAttemptLog, FactField),
    /// Immutable image reference.
    Media(&'a MediaRef),
    /// Immutable verbatim file reference.
    File(&'a rsi_media_protocol::FileRef),
}
impl FieldValue<'_> {
    /// Reads a bounded raw source window before renderer-specific sanitization.
    ///
    /// # Errors
    /// Returns [`WindowError`] for invalid bounds or failed serialization.
    pub fn window(&self, start: usize, maximum: usize) -> Result<FieldWindow, WindowError> {
        match self {
            Self::Text(text) => FieldWindow::text(text, start, maximum),
            Self::Attempt(attempt, field) => FieldWindow::text_parts(
                attempt.events().filter_map(|item| match item {
                    Err(error) => Some(Err(WindowError::Serialization(error.to_string()))),
                    Ok((_, event)) => model_text(event, *field).map(Ok),
                }),
                start,
                maximum,
            ),
            Self::Json(value) => FieldWindow::json(value, start, maximum),
            Self::Outcome(value) => FieldWindow::json(value, start, maximum),
            Self::Rejection(value) => FieldWindow::json(value, start, maximum),
            Self::Model(value) => FieldWindow::json(value, start, maximum),
            Self::Media(value) => FieldWindow::json(value, start, maximum),
            Self::File(value) => FieldWindow::json(value, start, maximum),
        }
    }
}

/// Selects only an exact matching source; missing or variant-mismatched fields are unavailable.
#[allow(clippy::too_many_lines)] // Keep the exhaustive Fact-to-field mapping in one place.
pub fn select_field(fact: &SessionFact, source: SourceRef) -> Option<FieldValue<'_>> {
    if fact.seq() != source.seq {
        return None;
    }
    if let SessionFactBody::ModelAttemptSettled { attempt, .. } = fact.body() {
        if source.field == FactField::ModelFailure
            && let LanguageEvent::Failed { error, .. } = attempt.terminal()
        {
            return Some(FieldValue::Text(Cow::Owned(error.to_string())));
        }
        if matches!(
            source.field,
            FactField::ModelText { .. }
                | FactField::ModelReasoning { .. }
                | FactField::ModelToolArguments { .. }
        ) && attempt
            .events()
            .any(|item| item.is_ok_and(|(_, event)| model_text(event, source.field).is_some()))
        {
            return Some(FieldValue::Attempt(attempt, source.field));
        }
        return None;
    }
    let text = match (fact.body(), source.field) {
        (
            body,
            field @ (FactField::InputReference { index }
            | FactField::InputImage { index }
            | FactField::InputFile { index }
            | FactField::InputText { index }),
        ) => {
            let block = match body {
                SessionFactBody::InputMessageClaimed { content, .. } => {
                    content.get(index as usize)?
                }
                SessionFactBody::StepInputsPrepared { decision, .. } => {
                    decision.authored_content().nth(index as usize)?
                }
                SessionFactBody::ToolContextsClaimed { messages, .. }
                    if matches!(field, FactField::InputText { .. }) =>
                {
                    let text = messages
                        .iter()
                        .flat_map(|row| &row.message.text)
                        .nth(index as usize)?;
                    return Some(FieldValue::Text(Cow::Borrowed(text)));
                }
                _ => return None,
            };
            match (field, block) {
                (FactField::InputText { .. }, AgentMessageContent::Text { text }) => text,
                (
                    FactField::InputReference { .. },
                    AgentMessageContent::Reference { reference },
                ) => &reference.preview,
                (FactField::InputImage { .. }, AgentMessageContent::Image { media, .. }) => {
                    return Some(FieldValue::Media(media));
                }
                (FactField::InputFile { .. }, AgentMessageContent::File { file }) => {
                    return Some(FieldValue::File(file));
                }
                _ => return None,
            }
        }
        (SessionFactBody::TurnAccepted { text, .. }, FactField::TurnInput) => text,
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
        (_, FactField::ToolCommand) => command_text(fact.body())?,
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

fn command_text(body: &SessionFactBody) -> Option<&str> {
    match body {
        SessionFactBody::ToolIntent {
            name, arguments, ..
        }
        | SessionFactBody::ToolRejected {
            name, arguments, ..
        } if matches!(name.as_str(), "bash" | "pwsh") => arguments.get("command")?.as_str(),
        _ => None,
    }
}

fn model_text(event: LanguageEvent, field: FactField) -> Option<String> {
    match (event, field) {
        (
            LanguageEvent::ContentDelta {
                index,
                delta: ContentDelta::Text(text),
            },
            FactField::ModelText { index: expected },
        )
        | (
            LanguageEvent::ContentDelta {
                index,
                delta: ContentDelta::Reasoning(text),
            },
            FactField::ModelReasoning { index: expected },
        )
        | (
            LanguageEvent::ContentDelta {
                index,
                delta: ContentDelta::ToolArguments(text),
            },
            FactField::ModelToolArguments { index: expected },
        ) if index == u32::from(expected) => Some(text),
        _ => None,
    }
}
