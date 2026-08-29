use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    AiError, MAX_CONTENT_BLOCKS, MAX_LANGUAGE_EVENTS, MAX_LANGUAGE_OUTPUT_BYTES, MAX_SOURCES,
    MAX_WARNINGS, validation,
};

const MAX_SOURCE_FIELD_BYTES: usize = 16 * 1024;
const MAX_WARNING_MESSAGE_BYTES: usize = 4 * 1024;

/// Token counts reported for one provider attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input tokens served from a provider cache, when reported.
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to a provider cache, when reported.
    pub cache_write_tokens: Option<u64>,
    /// Output tokens attributed to hidden reasoning, when reported.
    pub reasoning_tokens: Option<u64>,
}

/// Why a language response stopped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model reached a natural or caller-provided stop condition.
    Stop,
    /// The response ended with one or more tool calls for the caller to execute.
    ToolCalls,
    /// The provider stopped at the configured output-token limit.
    MaxTokens,
    /// Provider content policy stopped the response.
    ContentFilter,
    /// Cancellation ended the response after a normalized terminal event.
    Cancelled,
}

/// One model-requested tool call. Arguments deliberately remain raw text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Provider-neutral identifier used to correlate the later tool result.
    pub id: String,
    /// Exact declared tool name selected by the model.
    pub name: String,
    /// Raw function JSON or freeform input preserved without lossy parsing.
    pub arguments: String,
    /// Provider-neutral syntax used for the call and its later result.
    pub kind: ToolCallKind,
}

/// Provider-neutral syntax used to emit one tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    /// JSON arguments for a function tool.
    Function,
    /// Raw input governed by a freeform grammar.
    Freeform,
}

/// One complete provider-neutral content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    /// User-visible assistant text.
    Text { text: String },
    /// Provider-exposed reasoning text kept distinct from visible output.
    Reasoning { text: String },
    /// One complete model-requested tool call.
    ToolCall(ToolCall),
}

/// Metadata that opens one streamed content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentStart {
    /// Opens a user-visible text block.
    Text,
    /// Opens a provider-exposed reasoning block.
    Reasoning,
    /// Opens a tool call whose arguments arrive as deltas.
    ToolCall {
        /// Correlation identifier for the later tool result.
        id: String,
        /// Exact declared tool name.
        name: String,
        /// Exact provider-neutral call syntax.
        kind: ToolCallKind,
    },
}

/// One incremental update for an open content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ContentDelta {
    /// Appends visible text to an open text block.
    Text(String),
    /// Appends reasoning text to an open reasoning block.
    Reasoning(String),
    /// Appends raw JSON text to an open tool-call block.
    ToolArguments(String),
}

/// One citeable source returned by a provider-hosted operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Provider-neutral source identifier unique within the response.
    pub id: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

/// A safe provider or compatibility warning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    /// Stable bounded warning code.
    pub code: String,
    /// Safe bounded explanation for callers.
    pub message: String,
}

/// Bounded provider-private JSON that never contains binary data or secrets.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExtension {
    /// Provider-family namespace that owns the extension meaning.
    pub namespace: String,
    /// Namespace-local extension format version.
    pub version: u32,
    /// Bounded JSON value containing no secrets or binary media.
    pub value: Value,
}

impl<'de> Deserialize<'de> for ProviderExtension {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireExtension {
            namespace: String,
            version: u32,
            value: Value,
        }

        let wire = WireExtension::deserialize(deserializer)?;
        let extension = Self {
            namespace: wire.namespace,
            version: wire.version,
            value: wire.value,
        };
        extension
            .validate("provider_extension")
            .map(|()| extension)
            .map_err(serde::de::Error::custom)
    }
}

impl ProviderExtension {
    pub(crate) fn validate(&self, field: &str) -> Result<(), StreamError> {
        validation::identifier(&format!("{field}.namespace"), &self.namespace)
            .map_err(|message| StreamError::invalid("stream.invalid_extension", message))?;
        validation::validate_json_structure(&self.value)
            .map_err(|error| StreamError::invalid("stream.invalid_extension", error.to_string()))?;
        validation::extension_size(&format!("{field}.value"), &self.value)
            .map_err(|message| StreamError::invalid("stream.extension_too_large", message))
    }
}

/// Normalized language events emitted by every adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LanguageEvent {
    /// Opens the next indexed content block.
    ContentStarted {
        /// Zero-based contiguous block index.
        index: u32,
        /// Metadata fixing the block's content kind.
        content: ContentStart,
    },
    /// Appends data to an open content block.
    ContentDelta {
        /// Index of the open block receiving the delta.
        index: u32,
        /// Kind-matched incremental content.
        delta: ContentDelta,
    },
    /// Closes an open content block.
    ContentFinished {
        /// Index of the block to close.
        index: u32,
    },
    /// Adds one citeable source to the response.
    Source { source: Source },
    /// Adds one non-terminal compatibility or provider warning.
    Warning { warning: Warning },
    /// Supplies the response's sole cumulative usage record.
    Usage { usage: TokenUsage },
    /// Terminates a successful response after every content block is closed.
    Finished {
        reason: FinishReason,
        /// Optional bounded state needed to replay provider reasoning or tool context.
        replay: Option<ProviderExtension>,
    },
    /// Terminates a failed response and exposes validated partial output for
    /// diagnostics, never as a successful [`LanguageOutput`].
    Failed {
        error: AiError,
        /// Optional bounded provider state observed before failure.
        replay: Option<ProviderExtension>,
    },
}

impl LanguageEvent {
    /// Validates the context-free fields and individual bounds of one event.
    /// Stream ordering and aggregate limits remain owned by
    /// [`LanguageAssembler`].
    ///
    /// # Errors
    ///
    /// Returns a stable stream error when one field could not belong to any
    /// valid normalized language stream.
    pub fn validate(&self) -> Result<(), StreamError> {
        let validate_index = |index: u32| {
            if usize::try_from(index).map_or(true, |index| index >= MAX_CONTENT_BLOCKS) {
                Err(StreamError::invalid(
                    "stream.too_many_blocks",
                    format!("content index exceeds the {MAX_CONTENT_BLOCKS}-block limit"),
                ))
            } else {
                Ok(())
            }
        };
        match self {
            Self::ContentStarted { index, content } => {
                validate_index(*index)?;
                if let ContentStart::ToolCall { id, name, .. } = content {
                    validation::identifier("tool_call.id", id).map_err(|message| {
                        StreamError::invalid("stream.invalid_tool_call", message)
                    })?;
                    validation::tool_name("tool_call.name", name).map_err(|message| {
                        StreamError::invalid("stream.invalid_tool_call", message)
                    })?;
                }
                Ok(())
            }
            Self::ContentDelta { index, delta } => {
                validate_index(*index)?;
                let bytes = match delta {
                    ContentDelta::Text(value)
                    | ContentDelta::Reasoning(value)
                    | ContentDelta::ToolArguments(value) => value.len(),
                };
                if bytes == 0 {
                    return Err(StreamError::invalid(
                        "stream.empty_delta",
                        "language content deltas must make nonempty progress",
                    ));
                }
                if bytes > MAX_LANGUAGE_OUTPUT_BYTES {
                    return Err(StreamError::invalid(
                        "stream.output_too_large",
                        format!(
                            "one language delta exceeds the {MAX_LANGUAGE_OUTPUT_BYTES}-byte per-event limit"
                        ),
                    ));
                }
                Ok(())
            }
            Self::ContentFinished { index } => validate_index(*index),
            Self::Source { source } => {
                validation::identifier("source.id", &source.id)
                    .map_err(|message| StreamError::invalid("stream.invalid_source", message))?;
                if let Some(title) = &source.title {
                    validation::safe_text("source.title", title, MAX_SOURCE_FIELD_BYTES, false)
                        .map_err(|message| {
                            StreamError::invalid("stream.invalid_source", message)
                        })?;
                }
                if let Some(url) = &source.url {
                    validation::safe_text("source.url", url, MAX_SOURCE_FIELD_BYTES, false)
                        .map_err(|message| {
                            StreamError::invalid("stream.invalid_source", message)
                        })?;
                }
                Ok(())
            }
            Self::Warning { warning } => {
                validation::identifier("warning.code", &warning.code)
                    .map_err(|message| StreamError::invalid("stream.invalid_warning", message))?;
                validation::safe_text(
                    "warning.message",
                    &warning.message,
                    MAX_WARNING_MESSAGE_BYTES,
                    false,
                )
                .map_err(|message| StreamError::invalid("stream.invalid_warning", message))
            }
            Self::Usage { .. } => Ok(()),
            Self::Finished { replay, .. } => {
                if let Some(replay) = replay {
                    replay.validate("replay")?;
                }
                Ok(())
            }
            Self::Failed { error, replay } => {
                error.validate().map_err(|error| {
                    StreamError::invalid("stream.invalid_provider_error", error.to_string())
                })?;
                if let Some(replay) = replay {
                    replay.validate("replay")?;
                }
                Ok(())
            }
        }
    }
}

/// Complete language output assembled through the same grammar callers stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageOutput {
    /// Ordered complete content blocks.
    pub content: Vec<ContentBlock>,
    /// Successful terminal reason.
    pub finish_reason: FinishReason,
    /// Sole cumulative usage record, when reported.
    pub usage: Option<TokenUsage>,
    /// Optional provider state required for a later request replay.
    pub replay: Option<ProviderExtension>,
    /// Ordered safe warnings retained from the attempt.
    pub warnings: Vec<Warning>,
    /// Ordered citeable sources retained from the attempt.
    pub sources: Vec<Source>,
}

impl LanguageOutput {
    /// Concatenates user-visible text blocks without including reasoning.
    #[must_use]
    pub fn visible_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Reasoning { .. } | ContentBlock::ToolCall(_) => None,
            })
            .collect()
    }

    /// Revalidates a complete output through the canonical stream grammar.
    pub fn validate(&self) -> Result<(), StreamError> {
        let mut assembler = LanguageAssembler::new();
        for (index, block) in self.content.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| {
                StreamError::invalid("stream.too_many_blocks", "content block index overflowed")
            })?;
            let (content, delta) = match block {
                ContentBlock::Text { text } => {
                    (ContentStart::Text, ContentDelta::Text(text.clone()))
                }
                ContentBlock::Reasoning { text } => (
                    ContentStart::Reasoning,
                    ContentDelta::Reasoning(text.clone()),
                ),
                ContentBlock::ToolCall(call) => (
                    ContentStart::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        kind: call.kind,
                    },
                    ContentDelta::ToolArguments(call.arguments.clone()),
                ),
            };
            assembler.push(&LanguageEvent::ContentStarted { index, content })?;
            assembler.push(&LanguageEvent::ContentDelta { index, delta })?;
            assembler.push(&LanguageEvent::ContentFinished { index })?;
        }
        for source in &self.sources {
            assembler.push(&LanguageEvent::Source {
                source: source.clone(),
            })?;
        }
        for warning in &self.warnings {
            assembler.push(&LanguageEvent::Warning {
                warning: warning.clone(),
            })?;
        }
        if let Some(usage) = self.usage {
            assembler.push(&LanguageEvent::Usage { usage })?;
        }
        assembler.push(&LanguageEvent::Finished {
            reason: self.finish_reason.clone(),
            replay: self.replay.clone(),
        })?;
        let rebuilt = assembler.finish().map_err(|error| match error {
            LanguageAssemblyError::Protocol(error) => error,
            LanguageAssemblyError::Provider { .. } => unreachable!("success event cannot fail"),
        })?;
        if &rebuilt != self {
            return Err(StreamError::invalid(
                "stream.non_canonical_output",
                "language output is not canonical",
            ));
        }
        Ok(())
    }
}

/// Validated language content retained when a provider attempt fails.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguagePartialOutput {
    /// Complete content blocks accepted before failure.
    pub content: Vec<ContentBlock>,
    /// Usage observed before failure, when reported.
    pub usage: Option<TokenUsage>,
    /// Optional provider state observed before failure.
    pub replay: Option<ProviderExtension>,
    /// Safe warnings observed before failure.
    pub warnings: Vec<Warning>,
    /// Sources observed before failure.
    pub sources: Vec<Source>,
}

/// A stable stream-grammar failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct StreamError {
    code: &'static str,
    message: String,
}

impl StreamError {
    /// Constructs a stable stream failure at an adapter or orchestration boundary.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }

    /// Stable machine-readable failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

/// Terminal language assembly failure. Provider partial output is diagnostic
/// evidence and never a successful [`LanguageOutput`].
#[derive(Clone, Debug, Error, PartialEq)]
pub enum LanguageAssemblyError {
    /// The normalized event stream violated ordering, bounds, or terminal grammar.
    #[error(transparent)]
    Protocol(#[from] StreamError),
    /// The provider emitted a semantic failure after some valid output.
    #[error("{error}")]
    Provider {
        error: AiError,
        /// Valid normalized output accepted before the failure, for diagnostics.
        partial: Box<LanguagePartialOutput>,
    },
}

impl LanguageAssemblyError {
    /// Stable code for protocol failures, or a provider-neutral error kind.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Protocol(error) => error.code(),
            Self::Provider { error, .. } => error.kind().code(),
        }
    }
}

#[derive(Debug)]
enum OpenContent {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        kind: ToolCallKind,
    },
}

impl OpenContent {
    fn push(&mut self, delta: &ContentDelta) -> Result<usize, StreamError> {
        let added = match (self, delta) {
            (Self::Text(text), ContentDelta::Text(delta))
            | (Self::Reasoning(text), ContentDelta::Reasoning(delta)) => {
                let added = delta.len();
                text.push_str(delta);
                added
            }
            (Self::ToolCall { arguments, .. }, ContentDelta::ToolArguments(delta)) => {
                let added = delta.len();
                arguments.push_str(delta);
                added
            }
            _ => {
                return Err(StreamError::invalid(
                    "stream.content_type_mismatch",
                    "content delta does not match the open block type",
                ));
            }
        };
        Ok(added)
    }

    fn close(self) -> ContentBlock {
        match self {
            Self::Text(text) => ContentBlock::Text { text },
            Self::Reasoning(text) => ContentBlock::Reasoning { text },
            Self::ToolCall {
                id,
                name,
                arguments,
                kind,
            } => ContentBlock::ToolCall(ToolCall {
                id,
                name,
                arguments,
                kind,
            }),
        }
    }
}

/// Strict incremental assembler shared by direct SDK and durable consumers.
#[derive(Debug, Default)]
pub struct LanguageAssembler {
    next_index: u32,
    open: BTreeMap<u32, OpenContent>,
    content: BTreeMap<u32, ContentBlock>,
    assembled_bytes: usize,
    event_count: usize,
    usage: Option<TokenUsage>,
    replay: Option<ProviderExtension>,
    finish_reason: Option<FinishReason>,
    failure: Option<AiError>,
    warnings: Vec<Warning>,
    sources: Vec<Source>,
}

impl LanguageAssembler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_index: 0,
            open: BTreeMap::new(),
            content: BTreeMap::new(),
            assembled_bytes: 0,
            event_count: 0,
            usage: None,
            replay: None,
            finish_reason: None,
            failure: None,
            warnings: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Applies one normalized event while enforcing ordering and bounds.
    #[allow(clippy::too_many_lines)] // One exhaustive transition owns the stream grammar.
    pub fn push(&mut self, event: &LanguageEvent) -> Result<(), StreamError> {
        if self.finish_reason.is_some() || self.failure.is_some() {
            return Err(StreamError::invalid(
                "stream.already_finished",
                "language stream emitted an event after its terminal event",
            ));
        }
        self.event_count = self.event_count.checked_add(1).ok_or_else(|| {
            StreamError::invalid("stream.too_many_events", "language event count overflowed")
        })?;
        if self.event_count > MAX_LANGUAGE_EVENTS {
            return Err(StreamError::invalid(
                "stream.too_many_events",
                format!("language stream exceeds {MAX_LANGUAGE_EVENTS} events"),
            ));
        }
        event.validate()?;

        match event {
            LanguageEvent::ContentStarted { index, content } => {
                if *index != self.next_index {
                    return Err(StreamError::invalid(
                        "stream.non_contiguous_index",
                        format!(
                            "content index {index} is not the next contiguous index {}",
                            self.next_index
                        ),
                    ));
                }
                let open = match content {
                    ContentStart::Text => OpenContent::Text(String::new()),
                    ContentStart::Reasoning => OpenContent::Reasoning(String::new()),
                    ContentStart::ToolCall { id, name, kind } => {
                        self.add_bytes(id.len().saturating_add(name.len()))?;
                        OpenContent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                            kind: *kind,
                        }
                    }
                };
                self.open.insert(*index, open);
                self.next_index = self.next_index.saturating_add(1);
            }
            LanguageEvent::ContentDelta { index, delta } => {
                let open = self.open.get_mut(index).ok_or_else(|| {
                    StreamError::invalid(
                        "stream.content_not_started",
                        format!("content index {index} is not open"),
                    )
                })?;
                let added = open.push(delta)?;
                self.add_bytes(added)?;
            }
            LanguageEvent::ContentFinished { index } => {
                let open = self.open.remove(index).ok_or_else(|| {
                    StreamError::invalid(
                        "stream.content_not_started",
                        format!("content index {index} is not open"),
                    )
                })?;
                self.content.insert(*index, open.close());
            }
            LanguageEvent::Source { source } => {
                if self.sources.len() >= MAX_SOURCES {
                    return Err(StreamError::invalid(
                        "stream.too_many_sources",
                        format!("language stream exceeds {MAX_SOURCES} sources"),
                    ));
                }
                self.add_bytes(
                    source
                        .id
                        .len()
                        .saturating_add(source.title.as_ref().map_or(0, String::len))
                        .saturating_add(source.url.as_ref().map_or(0, String::len)),
                )?;
                self.sources.push(source.clone());
            }
            LanguageEvent::Warning { warning } => {
                if self.warnings.len() >= MAX_WARNINGS {
                    return Err(StreamError::invalid(
                        "stream.too_many_warnings",
                        format!("language stream exceeds {MAX_WARNINGS} warnings"),
                    ));
                }
                self.add_bytes(warning.code.len().saturating_add(warning.message.len()))?;
                self.warnings.push(warning.clone());
            }
            LanguageEvent::Usage { usage } => {
                if self.usage.replace(*usage).is_some() {
                    return Err(StreamError::invalid(
                        "stream.duplicate_usage",
                        "language stream emitted usage more than once",
                    ));
                }
            }
            LanguageEvent::Finished { reason, replay } => {
                if !self.open.is_empty() {
                    return Err(StreamError::invalid(
                        "stream.content_still_open",
                        "language stream finished while a content block remained open",
                    ));
                }
                self.finish_reason = Some(reason.clone());
                self.replay.clone_from(replay);
            }
            LanguageEvent::Failed { error, replay } => {
                for (index, open) in std::mem::take(&mut self.open) {
                    self.content.insert(index, open.close());
                }
                self.failure = Some(error.clone());
                self.replay.clone_from(replay);
            }
        }
        Ok(())
    }

    /// Returns the complete output after one terminal event.
    pub fn finish(self) -> Result<LanguageOutput, LanguageAssemblyError> {
        if let Some(error) = self.failure {
            return Err(LanguageAssemblyError::Provider {
                error,
                partial: Box::new(LanguagePartialOutput {
                    content: self.content.into_values().collect(),
                    usage: self.usage,
                    replay: self.replay,
                    warnings: self.warnings,
                    sources: self.sources,
                }),
            });
        }
        let finish_reason = self.finish_reason.ok_or_else(|| {
            LanguageAssemblyError::Protocol(StreamError::invalid(
                "stream.missing_finish",
                "language stream ended without a terminal event",
            ))
        })?;
        if self.content.is_empty() {
            return Err(LanguageAssemblyError::Protocol(StreamError::invalid(
                "stream.empty_output",
                "successful language output must contain at least one content block",
            )));
        }
        let has_tool_call = self
            .content
            .values()
            .any(|block| matches!(block, ContentBlock::ToolCall(_)));
        if matches!(finish_reason, FinishReason::ToolCalls) != has_tool_call {
            return Err(LanguageAssemblyError::Protocol(StreamError::invalid(
                "stream.finish_reason_mismatch",
                "tool-call finish reason must match the assembled content",
            )));
        }
        Ok(LanguageOutput {
            content: self.content.into_values().collect(),
            finish_reason,
            usage: self.usage,
            replay: self.replay,
            warnings: self.warnings,
            sources: self.sources,
        })
    }

    fn add_bytes(&mut self, added: usize) -> Result<(), StreamError> {
        let projected = self.assembled_bytes.checked_add(added).ok_or_else(|| {
            StreamError::invalid(
                "stream.output_too_large",
                "language output byte count overflowed",
            )
        })?;
        if projected > MAX_LANGUAGE_OUTPUT_BYTES {
            return Err(StreamError::invalid(
                "stream.output_too_large",
                format!(
                    "language output exceeds the {MAX_LANGUAGE_OUTPUT_BYTES}-byte assembled limit"
                ),
            ));
        }
        self.assembled_bytes = projected;
        Ok(())
    }
}
